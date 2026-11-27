//! A service's restarts as the heartbeat sends them (`ServiceBeat.restarts`): the counts of its
//! run units ([`Observation::restarts`]) summed, with the restarts of the last
//! [`RECENT_SECS`] seconds that a [`RestartWindow`] counts across observations.
//!
//! The window keeps, per service and run unit, the counts it was given and when, in seconds of a
//! monotonic clock the caller reads (the observer reads no clock). A unit's recent restarts are
//! its count now less its count at its oldest observation in the window, so a restart before the
//! window is never counted and the first observation of a unit counts none. A count that fell
//! (systemd sets `NRestarts` back to 0 when the unit is started by hand, as a deploy does), or a
//! time before the last one, starts the unit's window again, so a deploy's restart is not
//! counted. At most [`MAX_SAMPLES`] observations of a unit are kept, the oldest dropped first; a
//! unit that leaves the run set, or a service observed with no counts, is forgotten.
//!
//! The service's `count` and `recent` are the sums over its units, saturating; its
//! `last_result` and `exit_status` are those of the first unit, in run order, whose result is
//! not `success` (nor empty), or else of its first unit, the result cut to
//! [`MAX_LAST_RESULT`] bytes. A service none of whose units has a count has no restarts.
//!
//! [`Observation::restarts`]: crate::Observation::restarts

use std::collections::{BTreeMap, VecDeque};

use hostkeeper_proto::health::MAX_LAST_RESULT;
use hostkeeper_proto::hostkeeper::agent::v1::ServiceRestarts;

use crate::observe::RunUnitRestarts;

/// The span `recent` counts restarts over, in seconds.
pub const RECENT_SECS: u64 = 600;

/// The most observations of one unit the window keeps.
pub const MAX_SAMPLES: usize = 256;

/// One observation of a unit's count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    at: u64,
    count: u32,
}

/// The recent counts of every service's run units.
#[derive(Debug, Default)]
pub struct RestartWindow {
    services: BTreeMap<String, BTreeMap<String, VecDeque<Sample>>>,
}

impl RestartWindow {
    /// An empty window.
    pub fn new() -> RestartWindow {
        RestartWindow::default()
    }

    /// The restarts of `service`, whose run units counted `seen` at `now_secs`, and the window
    /// moved on to them; `None` when `seen` is empty.
    pub fn record(
        &mut self,
        service: &str,
        seen: &[RunUnitRestarts],
        now_secs: u64,
    ) -> Option<ServiceRestarts> {
        let mut before = self.services.remove(service).unwrap_or_default();
        let mut kept = BTreeMap::new();
        let mut recent = 0u32;
        for unit in seen {
            let mut samples = before.remove(&unit.unit).unwrap_or_default();
            recent = recent.saturating_add(push(&mut samples, now_secs, unit.restarts.count));
            kept.insert(unit.unit.clone(), samples);
        }
        let restarts = summarize(seen, recent)?;
        self.services.insert(service.to_owned(), kept);
        Some(restarts)
    }
}

/// The restarts of a service whose run units counted `seen`, `recent` of them in the last
/// [`RECENT_SECS`] seconds as its caller counted them: `count` the units' counts summed, and the
/// `last_result` and `exit_status` of the first unit whose result is not `success` (nor empty),
/// else of the first unit (the module's doc); `None` when `seen` is empty. The window of a Mac's
/// launchd jobs, which the daemon keeps in its state file, gives its counts here too.
pub fn summarize(seen: &[RunUnitRestarts], recent: u32) -> Option<ServiceRestarts> {
    let shown = seen
        .iter()
        .find(|unit| !matches!(unit.restarts.result.as_str(), "" | "success"))
        .or(seen.first())?;
    let result = shown.restarts.result.as_str();
    Some(ServiceRestarts {
        count: seen
            .iter()
            .fold(0u32, |sum, unit| sum.saturating_add(unit.restarts.count)),
        recent,
        last_result: result[..result.floor_char_boundary(MAX_LAST_RESULT)].to_owned(),
        exit_status: shown.restarts.exit_status,
    })
}

/// Adds the observation `count` at `now` to one unit's `samples`, and gives the unit's restarts
/// within the window.
fn push(samples: &mut VecDeque<Sample>, now: u64, count: u32) -> u32 {
    if samples
        .back()
        .is_some_and(|last| (count < last.count) | (now < last.at))
    {
        samples.clear();
    }
    samples.push_back(Sample { at: now, count });
    while samples
        .front()
        .is_some_and(|first| first.at.saturating_add(RECENT_SECS) < now)
    {
        samples.pop_front();
    }
    if samples.len() > MAX_SAMPLES {
        samples.pop_front();
    }
    samples
        .front()
        .map_or(0, |first| count.saturating_sub(first.count))
}
