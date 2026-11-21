//! What a host that failed the desired version does to the rest of its plan: a host group's
//! `failed_hosts` (`docs/operations/rollout-plans.md`, A failed host), and which failures the
//! host can clear by itself, which the coordinator orders again after the group's `defer_backoff`
//! ([`crate::fleet::Fleet::retry_by_itself`]).
//!
//! Under [`FailedHosts::Stop`] a failed host holds its stage: the stage is not done, and the next
//! does not start, until an operator resolves the host. Under [`FailedHosts::Continue`], a
//! group's default, the failed host stays at the version it runs and is an alert
//! ([`crate::alerts`]); its stage counts it as done, failed, and the plan goes on ordering the
//! other hosts, `max_updating` at a time, the failed host holding no slot of them. Two cases stop
//! the plan under either, as [`FailedHosts::Stop`] does: a failure while the canary is the current
//! stage, and, under [`FailedHosts::Continue`], as many failed hosts as `rollback_after_failed`
//! says of the plan's hosts (the ceiling: once it is reached no host is ordered that was not, so
//! it is the one case where a healthy host is left at its version). A host whose failure it can
//! clear by itself, which the coordinator retries, counts for no ceiling. A service of the rollout file
//! has no group, and keeps [`FailedHosts::Stop`].

use hostkeeper_wire::wire::{Outcome, Phase, UpdateResult};
use serde::{Deserialize, Serialize};

use crate::fleet::Fleet;
use crate::services::ServiceDef;
use crate::stages::{HostState, Member, View};

/// What a host that failed the desired version does to the rest of its stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FailedHosts {
    /// It holds its stage until an operator resolves it.
    Stop,
    /// Its stage counts it as done, failed, and the plan goes on.
    #[default]
    Continue,
}

/// The reasons of a failure the host can clear by itself: it was busy with something else, could
/// not reach what it fetches or verifies with, or had no room. The coordinator orders the version
/// again after the group's `defer_backoff` ([`crate::fleet::Fleet::retry_by_itself`]).
pub(crate) const CLEARS_BY_ITSELF: [&str; 5] = [
    "busy",
    "verify_unavailable",
    "fetch_failed",
    "package_manager_busy",
    "no_space",
];

/// Whether `report` is a failure the host can clear by itself: its reason is one of
/// [`CLEARS_BY_ITSELF`], and it failed before it changed anything (no phase, or before the commit).
#[must_use]
pub(crate) fn clears_by_itself(report: &UpdateResult) -> bool {
    report.outcome == Outcome::Failed
        && matches!(report.phase, None | Some(Phase::BeforeCommit))
        && report
            .reason
            .as_ref()
            .is_some_and(|reason| CLEARS_BY_ITSELF.contains(&reason.as_str()))
}

/// Whether the plan of `def` goes on past its failed hosts: a package of a host group whose
/// `failed_hosts` is [`FailedHosts::Continue`].
#[must_use]
pub(crate) fn continues(def: &ServiceDef) -> bool {
    def.scope
        .as_ref()
        .is_some_and(|scope| scope.failed_hosts == FailedHosts::Continue)
}

/// The ceiling of the plan of `def` over `hosts` eligible hosts: how many failed hosts stop it,
/// under [`FailedHosts::Continue`] (`rollback_after_failed`: the larger of its percent of the hosts,
/// rounded up, and its `min_hosts`); none under [`FailedHosts::Stop`], where one does.
#[must_use]
pub(crate) fn ceiling(def: &ServiceDef, hosts: usize) -> Option<usize> {
    continues(def).then(|| def.rollback_after_failed.hosts(hosts))
}

/// How many of `members`, the plan of `def`, failed for a reason they can clear by itself, in a
/// plan that goes on past its failed hosts: the coordinator retries them, and they say nothing of
/// the release, so they count for no ceiling. None under [`FailedHosts::Stop`].
pub(crate) fn retrying(def: &ServiceDef, fleet: &Fleet, members: &[Member]) -> usize {
    if !continues(def) {
        return 0;
    }
    members
        .iter()
        .filter(|m| m.state == HostState::Failed)
        .filter(|m| {
            fleet
                .get_in(&def.name, &m.id)
                .and_then(|host| host.beat.last_update.as_ref())
                .is_some_and(clears_by_itself)
        })
        .count()
}

impl View {
    /// How many of the plan's hosts failed the desired version.
    #[must_use]
    pub(crate) fn failed(&self) -> usize {
        self.members
            .iter()
            .filter(|m| m.state == HostState::Failed)
            .count()
    }

    /// Whether the plan's failed hosts hold it, as an operator must resolve them: under
    /// [`FailedHosts::Stop`] any failed host does; under [`FailedHosts::Continue`] one does while the
    /// canary is the current stage, and so do as many as the ceiling.
    #[must_use]
    pub(crate) fn failed_holds(&self) -> bool {
        self.failed() > 0 && (self.ceiling.is_none() || self.in_canary() || self.at_ceiling())
    }

    /// Whether the plan goes on past its failed hosts and as many have failed as its ceiling, those
    /// the coordinator retries left out: no host is ordered that was not.
    #[must_use]
    pub(crate) fn at_ceiling(&self) -> bool {
        self.ceiling
            .is_some_and(|ceiling| self.failed().saturating_sub(self.retrying) >= ceiling)
    }

    /// Whether the current stage is the canary.
    fn in_canary(&self) -> bool {
        self.current == 1 && self.stages.first().is_some_and(|s| s.percent.is_none())
    }
}
