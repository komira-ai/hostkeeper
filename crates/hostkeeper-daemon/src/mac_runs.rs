//! The `runs` ring of a Mac (docs/design/host-health-alerts.md sections 4.1, 4.2 and 5.1): the
//! recent counts of the launchd jobs of each managed service, kept in the daemon's health state
//! file ([`crate::health_state::HealthFile::runs`]), from which the heartbeat's
//! `ServiceBeat.restarts` of a Mac's service is made, so the coordinator's `service_crash_loop`
//! sees a Mac's job that keeps dying as it sees a Linux unit's.
//!
//! launchd keeps no restart counter as systemd does; what it prints of a job is `runs`, the times
//! it spawned the job since it was loaded, which the observer reads as the job's restarts (its
//! `runs` less the first; `hostkeeper_launchd::LaunchdServiceManager`). The ring keeps, per
//! service and job, the counts at which that count changed and when, in Unix seconds:
//!
//! - **A sample** is kept only when the count differs from the last one kept, so the state file is
//!   written when a job restarts and not at every heartbeat. The first observation of a job is
//!   its first sample.
//! - **The recent restarts** of a job are its count now less the count of its oldest sample still
//!   needed: the newest sample at or before [`RECENT_SECS`] ago (the count when the window began),
//!   else the oldest one (the job's first observation, which counts none). So a restart more than
//!   ten minutes old is never counted, and the first observation of a job counts none.
//! - **A count that fell** (a `bootout` and `bootstrap` start launchd's count again, as loading a
//!   changed plist does), or a time before the last sample's, starts the job's samples again: a
//!   deploy is not counted as a crash, and a clock stepped back counts nothing.
//! - At most [`MAX_RUN_SAMPLES`] samples of a job are kept, the oldest dropped first. A job that
//!   leaves the service's run set, and a service observed with no counts, are forgotten.
//!
//! The service's `count`, `last_result` and `exit_status` are made as on Linux
//! ([`hostkeeper_observe::restarts::summarize`]). Because the ring is in the state file, a restart
//! of the daemon (its own crash or update) keeps the window: the daemon's own job, in a loop, is
//! counted across its own restarts.

use std::collections::BTreeMap;

use hostkeeper_observe::RunUnitRestarts;
use hostkeeper_observe::restarts::summarize;
use hostkeeper_proto::hostkeeper::agent::v1::ServiceRestarts;
use serde::{Deserialize, Serialize};

pub use hostkeeper_observe::restarts::RECENT_SECS;

/// The most samples of one job the ring keeps.
pub const MAX_RUN_SAMPLES: usize = 64;

/// A job's count when it changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunSample {
    /// When it was observed, Unix seconds.
    pub at: u64,
    /// The job's restarts then.
    pub count: u32,
}

/// The samples of every observed service's jobs: by service, then by job label.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunsRing {
    pub services: BTreeMap<String, BTreeMap<String, Vec<RunSample>>>,
}

impl RunsRing {
    /// The restarts of `service`, whose jobs counted `seen` at `now_unix`, with the ring moved on
    /// to them; and whether the ring kept a sample it did not hold before, or forgot a job, so
    /// that the state file is to be written. `None` when `seen` is empty, and the service is
    /// forgotten.
    pub fn record(
        &mut self,
        service: &str,
        seen: &[RunUnitRestarts],
        now_unix: u64,
    ) -> (Option<ServiceRestarts>, bool) {
        let mut before = self.services.remove(service).unwrap_or_default();
        let mut kept = BTreeMap::new();
        let mut recent = 0u32;
        let mut changed = false;
        for unit in seen {
            let mut samples = before.remove(&unit.unit).unwrap_or_default();
            let (rise, pushed) = push(&mut samples, now_unix, unit.restarts.count);
            recent = recent.saturating_add(rise);
            changed |= pushed;
            kept.insert(unit.unit.clone(), samples);
        }
        changed |= !before.is_empty();
        let restarts = summarize(seen, recent);
        if restarts.is_some() {
            self.services.insert(service.to_owned(), kept);
        }
        (restarts, changed)
    }
}

/// Adds the count `count` at `now` to one job's `samples` (the module's doc): the job's restarts
/// within the window, and whether a sample was kept.
fn push(samples: &mut Vec<RunSample>, now: u64, count: u32) -> (u32, bool) {
    let last = samples.last().copied();
    let again = last.is_none_or(|last| (count < last.count) | (now < last.at));
    if again {
        samples.clear();
    }
    let pushed = again | last.is_some_and(|last| last.count != count);
    if pushed {
        samples.push(RunSample { at: now, count });
    }
    let start = now.saturating_sub(RECENT_SECS);
    while samples.get(1).is_some_and(|second| second.at <= start) {
        samples.remove(0);
    }
    if samples.len() > MAX_RUN_SAMPLES {
        samples.remove(0);
    }
    let base = samples.first().map_or(count, |first| first.count);
    (count.saturating_sub(base), pushed)
}
