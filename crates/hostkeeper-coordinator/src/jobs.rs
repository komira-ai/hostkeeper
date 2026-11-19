//! The coordinator's periodic work over a database (`docs/design/coordinator-multi-instance.md`,
//! section 9): every `--job-tick-secs` this coordinator claims the jobs that are due
//! ([`PgJobs::claim`]) and runs the ones it got. Of any number of coordinators one runs a job in
//! a period; a lost claim costs one period. Each job's effect is a conditional write of its own,
//! so a run on two coordinators at once would still log, audit or delete once:
//!
//! - `alerts`: the alerts observed and noted in their rows; the coordinator whose insert made a
//!   row logs the alert ([`crate::alerts`]);
//! - `request-expiry`: the enrollment requests that waited longer than the request TTL
//!   expired by one conditional update, and each audited by the coordinator that expired it;
//! - `prune`: expired sessions, forgotten failure counts and instances unseen for a day deleted.
//!
//! Over files there is one coordinator and nothing to claim: the alerts are observed every
//! [`OBSERVE_EVERY`] and the requests expire at the reload tick, as before.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use crate::alerts::{OBSERVE_EVERY, observe_every};
use crate::service::Coordinator;
use crate::store::postgres::alert_rows::PgAlerts;
use crate::store::postgres::periodic::{ALERTS, PRUNE, PgJobs, REQUEST_EXPIRY};

/// The claimed jobs of a coordinator over a database, and how often it claims.
#[derive(Debug)]
pub(crate) struct Jobs {
    jobs: PgJobs,
    tick: Duration,
}

impl Jobs {
    /// `jobs`, claimed every `tick`.
    pub(crate) fn new(jobs: PgJobs, tick: Duration) -> Jobs {
        Jobs { jobs, tick }
    }
}

impl Coordinator {
    /// The same coordinator running its periodic work by the claims of `jobs`, and noting its
    /// alerts in `alerts`, a database's rows.
    #[must_use]
    pub(crate) fn with_jobs(mut self, jobs: Jobs, alerts: PgAlerts) -> Self {
        self.alerts = std::mem::take(&mut self.alerts).over(alerts);
        self.jobs = Some(Arc::new(jobs));
        self
    }

    /// Whether the coordinator claims its periodic work ([`Self::with_jobs`]): then the reload
    /// tick does not expire the enrollment requests, the job `request-expiry` does.
    #[must_use]
    pub(crate) fn claims_jobs(&self) -> bool {
        self.jobs.is_some()
    }
}

/// The coordinator's periodic work, for as long as the future is polled: over a database the
/// jobs it claims every tick ([`run_claimed`]), else the alerts observed every
/// [`OBSERVE_EVERY`]. It never returns.
pub async fn periodic(coordinator: Arc<Coordinator>) -> Infallible {
    let Some(jobs) = coordinator.jobs.clone() else {
        return observe_every(coordinator, OBSERVE_EVERY).await;
    };
    let mut ticks = tokio::time::interval(jobs.tick);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        run_claimed(&coordinator, &jobs).await;
    }
}

/// One tick: claims the jobs that are due and runs each one this coordinator got, off the
/// runtime's threads; the names of the jobs it ran. A claim that fails runs nothing (the jobs are
/// due again at the next tick, on any coordinator), and a job that fails is logged.
pub(crate) async fn run_claimed(coordinator: &Arc<Coordinator>, jobs: &Arc<Jobs>) -> Vec<String> {
    let claiming = Arc::clone(jobs);
    let claimed = tokio::task::spawn_blocking(move || claiming.jobs.claim())
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    let claimed = claimed.unwrap_or_else(|why| {
        tracing::warn!(%why, "the periodic jobs could not be claimed");
        Vec::new()
    });
    for name in &claimed {
        let (coordinator, jobs, job) = (Arc::clone(coordinator), Arc::clone(jobs), name.clone());
        let ran = tokio::task::spawn_blocking(move || run(&coordinator, &jobs, &job)).await;
        if let Err(why) = ran.map_err(|e| e.to_string()).and_then(|ran| ran) {
            tracing::error!(%why, job = %name, "a periodic job failed");
        }
    }
    claimed
}

/// Runs the job `name`.
fn run(coordinator: &Coordinator, jobs: &Jobs, name: &str) -> Result<(), String> {
    match name {
        ALERTS => coordinator
            .observe_alerts()
            .map(drop)
            .map_err(|why| why.to_string()),
        REQUEST_EXPIRY => {
            if let Some(enrollment) = coordinator.enrollment() {
                enrollment.expire_requests();
            }
            Ok(())
        }
        PRUNE => jobs.jobs.prune().map(|pruned| {
            tracing::info!(?pruned, "the rows no rule reads were pruned");
        }),
        _ => Err(format!("no job is named {name}")),
    }
}
