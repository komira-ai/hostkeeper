//! The console's tick, its drain and its start line (`docs/design/console-broker.md`, decisions
//! D4 and D12, section 12).
//!
//! Every [`SWEEP_EVERY`] the sweep, on the coordinator's clock: ends each session not attached in
//! its window (its `console_close` by `coordinator`); tells the table the bytes each stream has
//! carried, so a stream that carried none for `--console-idle-s` closes `idle` (4005), and one
//! attached for `--console-total-s` closes `total_limit` (4006); and closes `token_revoked`
//! (4008) each stream whose credential is gone (its operator has no token any more, or, for a
//! stream attached with a browser's session, that session ended by sign-out or expiry), or whose
//! operator the console operators file no longer names, the files as re-read on the
//! `--tls-reload-secs` tick.
//!
//! At stop, every stream closes `coordinator_stopping` (4010), and the stop waits for their
//! clean-up (each serial console's payload deactivated, each close line written), for
//! [`DRAIN_GRACE`] at most. At start, with console files, one `console_coordinator_start` line
//! says that every session open before it ended there: none outlives a coordinator.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::future::{Either, select};

use hostkeeper_console::close::Close;
use hostkeeper_console::operators::Need;
use hostkeeper_console::session::Limit;

use crate::audit_actions::AuditOnly;
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;
use crate::ssh_tunnel::{Credential, credential_holds};

/// How often the sweep runs.
pub(crate) const SWEEP_EVERY: Duration = Duration::from_secs(5);

/// How long the stop waits for the streams it closed to clean up.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(40);

impl Coordinator {
    /// One sweep of the console's sessions, as the module says.
    pub(crate) fn console_sweep(&self) {
        let now = (self.clock)();
        let mut table = self.console.lock_table();
        self.end_unattached(&mut table.sessions, now);
        let table = &mut *table;
        for (host, live) in &mut table.live {
            let moved = live.moved.total();
            table.sessions.observe(host, moved, now);
            // A console in use keeps the browser's sign-in it attached with alive, as the
            // requests of that sign-in would: a byte that crossed since the last sweep uses it.
            // A session that has ended is not brought back.
            if moved != live.used_at {
                live.used_at = moved;
                if let (Credential::Session(key), Some(login)) = (&live.credential, self.login()) {
                    login.sessions.touch(key);
                }
            }
        }
        let due = table.sessions.due(now);
        let limited = due.iter().filter_map(|(host, limit)| {
            let close = match limit {
                Limit::Idle => Close::Idle,
                Limit::Total => Close::TotalLimit,
            };
            table.live.get(host).map(|live| (live, close))
        });
        limited.for_each(|(live, close)| live.stop(close, COORDINATOR));
        // Only an attached stream has an operator to lose: the live ones are looked at.
        let revoked = table.live.iter().filter(|(host, live)| {
            table.sessions.get(host).is_some_and(|session| {
                let by = session.opened_by.as_str();
                !(credential_holds(self, by, &live.credential)
                    && self.console.files.grant(by, Need::Console).is_ok())
            })
        });
        revoked.for_each(|(_, live)| live.stop(Close::TokenRevoked, COORDINATOR));
    }

    /// Closes every stream `coordinator_stopping` and waits until each has cleaned up, for
    /// `grace` at most.
    pub(crate) async fn console_drain(&self, grace: Duration) {
        let emptied = async {
            loop {
                let ended = self.console.ended.notified();
                {
                    let table = self.console.lock_table();
                    if table.live.is_empty() {
                        return;
                    }
                    for live in table.live.values() {
                        live.stop(Close::CoordinatorStopping, COORDINATOR);
                    }
                }
                ended.await;
            }
        };
        if tokio::time::timeout(grace, emptied).await.is_err() {
            tracing::error!("console streams did not clean up in time: the stop goes on");
        }
    }

    /// Writes `console_coordinator_start` when the coordinator has console files; best effort.
    pub(crate) fn console_started(&self) {
        if self.console.files.given() {
            let _ = self.console_audit(
                AuditOnly::ConsoleCoordinatorStart,
                COORDINATOR,
                None,
                "every console session open before this start ended",
            );
        }
    }
}

/// [`Coordinator::console_sweep`] every [`SWEEP_EVERY`], for as long as it is polled.
pub(crate) async fn sweep_every(coordinator: Arc<Coordinator>) -> Infallible {
    let mut ticks = tokio::time::interval(SWEEP_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        coordinator.console_sweep();
    }
}

/// The console's part of a stop: [`Coordinator::console_drain`] within [`DRAIN_GRACE`], while
/// `serving` (the listeners, which never end but by failing) goes on.
///
/// # Errors
/// The listeners failed meanwhile.
pub(crate) async fn drained<S, E>(coordinator: &Coordinator, serving: S) -> Result<(), E>
where
    S: Future<Output = Result<Infallible, E>> + Unpin,
{
    let drain = Box::pin(coordinator.console_drain(DRAIN_GRACE));
    match select(drain, serving).await {
        Either::Left(((), _)) => Ok(()),
        Either::Right((served, _)) => {
            let Err(why) = served;
            Err(why)
        }
    }
}
