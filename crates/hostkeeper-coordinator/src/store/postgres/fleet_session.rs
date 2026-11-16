//! The coordinator's session on the fleet's rows (`docs/design/coordinator-multi-instance.md`,
//! section 5): what the heartbeat writes of a beat, and the gate it decides under.
//!
//! A heartbeat is heard under the coordinator's own lock of its fleet, so these calls are made one
//! at a time and share one session, made when first needed and again after it is lost. Each runs
//! on a thread of its own for the call, because the client must not run on a thread of the
//! runtime a heartbeat may be answered on, and so that the decision's body, which borrows the
//! coordinator's fleet, runs where the transaction is.
//!
//! [`FleetRows::gated`] is the decision: it takes the gate ([`super::rollout::decide`]), and its
//! body reads the rows it decides on ([`Gate::snapshot`]) before it writes what it decides.
//! The coordinator keeps what it read of the hosts' rows apart ([`FleetRows::known`]): a row it
//! has already taken in is not read again.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use super::Postgres;
use super::fleet_rows::{
    self, BakeRow, HostBeat, HostRow, OrderRow, Timing, bakes, changed_hosts, end_orders, orders,
};
use super::rollout::{
    self, Admission, Ask, Gated, Limits, Uninstalling, Wait, admit, place_uninstall, stage_is_at,
};
use super::session::{Session, describe, ends_session};

/// What a decision reads of the rows once it holds the gate.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// The hosts' rows the reader had not taken in, by host.
    pub(crate) hosts: Vec<HostRow>,
    /// Every order not yet reported done.
    pub(crate) orders: Vec<OrderRow>,
    /// Every bake.
    pub(crate) bakes: Vec<BakeRow>,
    /// The time before which nothing is decided from the rows (0 for never set).
    pub(crate) quiet_until: i64,
}

/// What a body of [`FleetRows::gated`] holds: the session of the transaction that holds the gate.
pub(crate) struct Gate<'a> {
    session: &'a mut Session,
    known: &'a Mutex<BTreeMap<String, (i64, String)>>,
}

impl Gate<'_> {
    /// The rows the decision rests on, read after the gate was granted; the rows of the hosts
    /// are the ones this coordinator has not taken in yet, and they are taken in from now.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn snapshot(&mut self) -> Result<Snapshot, postgres::Error> {
        let known: Vec<(String, i64, String)> = self
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(id, (at, who))| (id.clone(), *at, who.clone()))
            .collect();
        let hosts = changed_hosts(self.session, &known)?;
        let mut kept = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        for row in &hosts {
            kept.insert(row.host_id.clone(), (row.seen_unix, row.instance.clone()));
        }
        drop(kept);
        Ok(Snapshot {
            hosts,
            orders: orders(self.session)?,
            bakes: bakes(self.session)?,
            quiet_until: fleet_rows::quiet_until(self.session)?,
        })
    }

    /// Decides whether `ask` gets an order, and writes it when it does ([`rollout::admit`]).
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn admit(
        &mut self,
        ask: &Ask<'_>,
        limits: &Limits,
    ) -> Result<Admission, postgres::Error> {
        admit(self.session, ask, limits)
    }

    /// Whether the plan stands where the decider saw it ([`rollout::stage_is_at`]).
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn stage_is_at(
        &mut self,
        group: Option<&str>,
        service: &str,
        version: &str,
        from: i64,
        first: i64,
    ) -> Result<bool, postgres::Error> {
        stage_is_at(self.session, group, service, version, from, first)
    }

    /// Takes `services` out of the queue of rollouts: each has nothing left to roll.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn finish(&mut self, services: &[&str]) -> Result<(), postgres::Error> {
        rollout::finish(self.session, services).map(|_| ())
    }

    /// Takes back the order of the host `host_id`, which was written in this transaction and was
    /// not given after all.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn take_back(&mut self, host_id: &str) -> Result<(), postgres::Error> {
        end_orders_of(self.session, host_id)
    }

    /// Writes the uninstall order `order_id` for the host of `ask` ([`rollout::place_uninstall`]).
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn place_uninstall(
        &mut self,
        ask: &Ask<'_>,
        order_id: &str,
    ) -> Result<Uninstalling, postgres::Error> {
        place_uninstall(self.session, ask, order_id)
    }
}

/// Deletes the order of the host `host_id`, whatever service it is for.
fn end_orders_of(session: &mut Session, host_id: &str) -> Result<(), postgres::Error> {
    session.execute("DELETE FROM fleet_orders WHERE host_id = $1", &[&host_id])?;
    Ok(())
}

/// The coordinator's session on the fleet's rows.
pub(crate) struct FleetRows {
    config: postgres::Config,
    instance: String,
    session: Mutex<Option<Session>>,
    /// The host rows this coordinator holds in its cache: the time of the beat and the instance
    /// that stored it.
    known: Mutex<BTreeMap<String, (i64, String)>>,
}

impl Postgres {
    /// The session of the fleet's rows of the coordinator `instance`, over this database.
    pub(crate) fn fleet_rows(&self, instance: &str) -> Arc<FleetRows> {
        Arc::new(FleetRows {
            config: self.inner.config.clone(),
            instance: instance.to_owned(),
            session: Mutex::new(None),
            known: Mutex::new(BTreeMap::new()),
        })
    }
}

/// Why the rows could not be used: the text of what the session or the server said.
pub(crate) type Unreachable = String;

impl FleetRows {
    /// Runs `body` on the session, on a thread of its own; a session that is gone is made again at
    /// the next call.
    fn on_session<R: Send>(
        &self,
        body: impl FnOnce(
            &mut Session,
            &Mutex<BTreeMap<String, (i64, String)>>,
        ) -> Result<R, postgres::Error>
        + Send,
    ) -> Result<R, Unreachable> {
        let run = || {
            let mut held = self.session.lock().unwrap_or_else(PoisonError::into_inner);
            let mut session = match held.take() {
                Some(session) => session,
                None => Session::connect(&self.config, Arc::new(AtomicU64::new(0)), false)
                    .map_err(|fail| match fail {
                        super::session::Fail::Pg(e) => describe(&e),
                        super::session::Fail::Refused(why) => why,
                    })?,
            };
            let done = body(&mut session, &self.known);
            match done {
                Ok(answer) => {
                    *held = Some(session);
                    Ok(answer)
                }
                Err(e) => {
                    if !ends_session(&e) {
                        *held = Some(session);
                    }
                    Err(describe(&e))
                }
            }
        };
        std::thread::scope(|scope| {
            scope
                .spawn(run)
                .join()
                .unwrap_or_else(|_| Err("the session of the fleet's rows stopped".to_owned()))
        })
    }

    /// Stores `beat` as the host's last, if it is not older than the one stored; true when it was
    /// stored. The host's row is then one this coordinator holds in its cache.
    ///
    /// # Errors
    /// What the session or the server said.
    pub(crate) fn beat(&self, beat: &HostBeat<'_>, timing: &Timing) -> Result<bool, Unreachable> {
        let beat = HostBeat {
            instance: &self.instance,
            ..*beat
        };
        self.on_session(|session, known| {
            let written = fleet_rows::beat(session, &beat, timing)?;
            if written {
                known.lock().unwrap_or_else(PoisonError::into_inner).insert(
                    beat.host_id.to_owned(),
                    (timing.now, beat.instance.to_owned()),
                );
            }
            Ok(written)
        })
    }

    /// The orders the rows hold for the host `host_id`.
    ///
    /// # Errors
    /// What the session or the server said.
    pub(crate) fn orders_of(&self, host_id: &str) -> Result<Vec<OrderRow>, Unreachable> {
        self.on_session(|session, _| fleet_rows::orders_of(session, host_id))
    }

    /// Ends the orders of the host `host_id` for `services`, which the beat just stored ended.
    ///
    /// # Errors
    /// What the session or the server said.
    pub(crate) fn end_orders(&self, host_id: &str, services: &[&str]) -> Result<(), Unreachable> {
        self.on_session(|session, _| end_orders(session, host_id, services).map(|_| ()))
    }

    /// Runs `body` in one transaction that holds the gate ([`rollout::decide`]): the answer of
    /// the body once committed, or [`Gated::Busy`] when another decision holds the gate and a beat
    /// does not wait. A body that fails rolls everything back.
    ///
    /// # Errors
    /// What the session or the server said, the body's failure and the gate's timeout among them.
    pub(crate) fn gated<R: Send>(
        &self,
        wait: Wait,
        body: impl FnOnce(&mut Gate<'_>) -> Result<R, postgres::Error> + Send,
    ) -> Result<Gated<R>, Unreachable> {
        self.on_session(|session, known| {
            rollout::decide(session, wait, |held| {
                body(&mut Gate {
                    session: held,
                    known,
                })
            })
        })
    }
}

impl Drop for FleetRows {
    fn drop(&mut self) {
        // A client closes by running on its own runtime, which it may not do on a thread of the
        // runtime the coordinator is dropped on.
        let session = self
            .session
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(session) = session {
            // A thread that cannot be made leaves the session to close where it is.
            let closing = std::thread::Builder::new()
                .name("hk-fleet-rows-close".to_owned())
                .spawn(move || drop(session));
            if let Err(why) = closing {
                tracing::warn!(%why, "the fleet's session could not be closed on a thread of its own");
            }
        }
    }
}
