//! The automatic retry: a failure the host can clear by itself
//! ([`crate::failed_hosts::clears_by_itself`]: it was busy, could not fetch or verify, or had no
//! room) is set aside by the coordinator, as `Hosts.Retry` sets one aside
//! ([`Fleet::set_aside`]), once the group's `defer_backoff` has passed since the host reported it:
//! the host is ordered the version again at that heartbeat, at the try past the failed one. The
//! n-th failure in a row of one version waits `initial_secs * factor^(n-1)`, capped at `max_secs`,
//! as the n-th deferral does ([`crate::defer::DeferBackoff::wait`]).
//!
//! A run of failures counts each failure once: a report heard again in later heartbeats is the
//! same failure, and one heard after the coordinator set the last aside is a new one. The run ends
//! when the host reports anything else of the service: a success, a failure it cannot clear by
//! itself (which stands until an operator acts), or a version it runs. Like what an operator sets
//! aside, the runs are kept in memory only: after a restart the first report heard starts one, and
//! the host is ordered again `initial_secs` after it.
//!
//! The run's last failure stays the host's `failed_update` alert while the coordinator retries
//! ([`Fleet::failing`]), so the alert does not clear and come back at each try.

use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, UpdateResult};

use super::super::Fleet;
use crate::defer::DeferBackoff;
use crate::failed_hosts::clears_by_itself;

/// A run of failures of one version the host can clear by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Failing {
    /// The version it failed.
    target: Version,
    /// How many failures of it in a row.
    failures: u32,
    /// When the last was heard.
    at: Duration,
    /// The last.
    report: UpdateResult,
    /// Whether the next failure heard is a new one: none is counted yet, or the coordinator set the
    /// last aside.
    next_is_new: bool,
}

impl Failing {
    /// Notes that its last failure was set aside: the next failure heard is a new one.
    pub(super) fn set_aside(&mut self) {
        self.next_is_new = true;
    }
}

/// What the alert of a run of failures reads ([`Fleet::failing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    /// The last failure.
    pub(crate) report: UpdateResult,
    /// How many failures of the version in a row.
    pub(crate) failures: u32,
}

impl Fleet {
    /// Notes the report of `service` the host `id`'s last counted beat kept, heard at `now`, and,
    /// when it is a failure the host can clear by itself and `backoff` has passed since it was
    /// heard, sets it aside so the host is ordered again. True when it set it aside.
    pub(crate) fn retry_by_itself(
        &mut self,
        service: &str,
        id: &HostId,
        backoff: &DeferBackoff,
        now: Duration,
    ) -> bool {
        let key = (id.clone(), service.to_owned());
        // No report: none heard yet, or the one set aside, which the run keeps.
        let Some((host, report)) = self
            .hosts
            .get(service)
            .and_then(|hosts| hosts.get(id))
            .and_then(|host| Some((host, host.beat.last_update.as_ref()?)))
        else {
            return false;
        };
        let runs = host.beat.health != Health::Absent && host.beat.version == report.target;
        if runs || !clears_by_itself(report) {
            self.retries.failing.remove(&key);
            return false;
        }
        let report = report.clone();
        let fresh = Failing {
            target: report.target.clone(),
            failures: 0,
            at: now,
            report: report.clone(),
            next_is_new: true,
        };
        let failing = self
            .retries
            .failing
            .entry(key)
            .or_insert_with(|| fresh.clone());
        if failing.target != report.target {
            *failing = fresh;
        }
        if failing.next_is_new || failing.report != report {
            failing.failures = failing.failures.saturating_add(1);
            failing.at = now;
            failing.report = report.clone();
            failing.next_is_new = false;
        }
        if now < failing.at.saturating_add(backoff.wait(failing.failures, 0)) {
            return false;
        }
        failing.next_is_new = true;
        let failures = failing.failures;
        self.set_aside(service, id);
        tracing::info!(
            %service,
            node_id = %id,
            target = %report.target.as_str(),
            reason = ?report.reason.as_ref().map(|r| r.as_str()),
            order_try = ?report.order_try,
            failures,
            "a failure the host can clear by itself was set aside: the host is ordered again"
        );
        true
    }

    /// The run of failures of `service` the host `id` is in, while the coordinator retries it: its
    /// last failure is set aside, and the host has not reported another outcome since.
    #[must_use]
    pub(crate) fn failing(&self, service: &str, id: &HostId) -> Option<Run> {
        self.retries
            .failing
            .get(&(id.clone(), service.to_owned()))
            .filter(|run| run.next_is_new)
            .map(|run| Run {
                report: run.report.clone(),
                failures: run.failures,
            })
    }

    /// The reason the host `id` declined `target` of `service` for, and how many times in a row it
    /// gave it ([`crate::defer`]), if it declined `target`.
    #[must_use]
    pub(crate) fn declined_in_a_row(
        &self,
        service: &str,
        id: &HostId,
        target: &Version,
    ) -> Option<(&hostkeeper_wire::wire::Token, u32)> {
        self.defers
            .streak(&(id.clone(), service.to_owned()), target)
    }

    /// How many failures in a row of `service` the host `id` has had, if it is in a run.
    #[must_use]
    pub(crate) fn failures_in_a_row(&self, service: &str, id: &HostId) -> Option<u32> {
        self.retries
            .failing
            .get(&(id.clone(), service.to_owned()))
            .map(|run| run.failures)
    }
}
