//! The convergence, the wait and the release by host group (`docs/design/hostgroups.md`
//! section 2.1), for a coordinator that holds a host group.
//!
//! `Releases.Converge`, `WaitConverged` and `GetRelease` name a group or none. A named group must
//! list the package (`not_in_host_group`, which an unknown group is too). With none, `Converge`
//! and `WaitConverged` answer for every group that lists the package and has a desired version
//! of it: converged when all are, a pair with no desired version left out, a group with no hosts
//! converged. The convergence of a pair is the convergence of a coordinator without groups
//! ([`crate::converge`]) over the group's view of the package ([`crate::group_scope`]), its roles
//! being the group's name; the answer for several is one [`Convergence`]: the worst state of
//! them (halted, then blocked, then in progress), the roles of all of them one after the other,
//! and the sums of the hosts they do not count. `WaitConverged` answers `no_desired` at once when
//! no pair has a desired version. `GET /v1/convergence` makes no group's name, so it reads the
//! same for every group, in the same shape.
//!
//! A group has no hosts when the enrollment store holds none in it and the fleet has heard of
//! none; a package of such a group with a desired version is converged, whatever the quiet start
//! says.

use std::sync::Arc;

use hostkeeper_wire::version::Version;

use crate::converge::{Convergence, State};
use crate::fleet::Fleet;
use crate::group_scope::{group_services, package_def};
use crate::host_groups::GroupName;
use crate::membership::HostCounts;
use crate::ops::OpsState;
use crate::releases::Release;
use crate::service::Coordinator;
use crate::service::ci_calls::ReleaseError;
use crate::service::{ConvergenceError, ServiceStatus};
use crate::services::ServiceDef;
use crate::status::convergence_of;
use crate::wait::WaitRefused;
use crate::write_record::WriteRecord;

/// Which pairs of a package a call reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The package as the rollout file defines it: the coordinator holds no host group.
    Service,
    /// The package in every group that lists it and has a desired version of it.
    All,
    /// The package in one group, which lists it.
    One(GroupName),
}

/// Why a call reaches no pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unreached {
    /// The named group is not held or does not list the package.
    NotInHostGroup,
    /// The package is in neither the rollout file nor any group's list.
    UnknownService,
}

impl From<Unreached> for ConvergenceError {
    fn from(why: Unreached) -> Self {
        match why {
            Unreached::NotInHostGroup => Self::NotInHostGroup,
            Unreached::UnknownService => Self::UnknownService,
        }
    }
}

impl From<Unreached> for WaitRefused {
    fn from(why: Unreached) -> Self {
        match why {
            Unreached::NotInHostGroup => Self::NotInHostGroup,
            Unreached::UnknownService => Self::UnknownService,
        }
    }
}

impl From<Unreached> for ReleaseError {
    fn from(why: Unreached) -> Self {
        match why {
            Unreached::NotInHostGroup => Self::NotInHostGroup,
            Unreached::UnknownService => Self::UnknownService,
        }
    }
}

/// What one read of a package's convergence over some groups found.
pub(crate) struct Reading {
    /// The convergence of all the pairs read ([`aggregate`]).
    pub(crate) convergence: Convergence,
    /// How many pairs have a desired version.
    pub(crate) pairs: usize,
}

/// The convergence of several pairs as one: `parts` in the order of their groups, read at a time
/// the coordinator was `quiet` or not. The desired version it names is the first pair's, or, for a
/// call that waits on `against`, the first pair's that is not `against`. None of them: no desired
/// version, in progress.
pub(crate) fn aggregate(
    parts: Vec<Convergence>,
    quiet: bool,
    against: Option<&str>,
) -> Convergence {
    let state_of = |state: State| parts.iter().any(|part| part.state == state);
    let state = if state_of(State::Halted) {
        State::Halted
    } else if state_of(State::Blocked) {
        State::Blocked
    } else if parts.is_empty() || state_of(State::InProgress) {
        State::InProgress
    } else {
        State::Converged
    };
    let named = |part: &&Convergence| {
        against.is_some_and(|version| part.desired.as_ref().map(Version::as_str) != Some(version))
    };
    let desired = parts
        .iter()
        .find(named)
        .or(parts.first())
        .and_then(|part| part.desired.clone());
    let hosts = parts
        .iter()
        .filter_map(|part| part.hosts)
        .reduce(|sum, more| HostCounts {
            missing: sum.missing + more.missing,
            absent: sum.absent + more.absent,
            excluded: sum.excluded + more.excluded,
            unmanaged: sum.unmanaged + more.unmanaged,
            deferred: sum.deferred + more.deferred,
        });
    let picks = parts
        .iter()
        .filter_map(|part| part.picks.clone())
        .reduce(|mut sum, more| {
            sum.picked += more.picked;
            sum.no_target += more.no_target;
            sum.no_build_for_host += more.no_build_for_host;
            sum.installer_dropped += more.installer_dropped;
            sum.dropped.extend(more.dropped);
            sum
        });
    Convergence {
        desired,
        quiet,
        state,
        halt: parts.iter().find_map(|part| part.halt.clone()),
        roles: parts.iter().flat_map(|part| part.roles.clone()).collect(),
        hosts,
        picks,
        commit: crate::BUILD_COMMIT,
    }
}

impl Coordinator {
    /// Which pairs of `package` a call that names the host group `host_group` (empty: none)
    /// reads. A coordinator without host groups reads the rollout file's definition, whatever the
    /// request names: a request that names a group was refused before, while none is held.
    ///
    /// # Errors
    /// [`Unreached::NotInHostGroup`]: the named group is unknown or does not list the package.
    /// [`Unreached::UnknownService`]: no group lists the package and the rollout file does not
    /// define it.
    pub(crate) fn scope_of(&self, host_group: &str, package: &str) -> Result<Scope, Unreached> {
        if !self.desired.in_group_mode() {
            return Ok(Scope::Service);
        }
        let groups = self.desired.host_groups();
        let lists =
            |group: &crate::host_groups::HostGroup| group.packages.iter().any(|p| p == package);
        if host_group.is_empty() {
            let known = self.services.get(package).is_some() || groups.0.values().any(lists);
            return known.then_some(Scope::All).ok_or(Unreached::UnknownService);
        }
        host_group
            .parse::<GroupName>()
            .ok()
            .filter(|name| groups.0.get(name).is_some_and(lists))
            .map(Scope::One)
            .ok_or(Unreached::NotInHostGroup)
    }

    /// The release `version` of `service` as a call that names the group `host_group` (empty:
    /// none) reads it: the release is the package's, not the group's, so the group only has to
    /// list the package.
    ///
    /// # Errors
    /// [`ReleaseError`].
    pub(crate) fn release_verified_in(
        &self,
        host_group: &str,
        service: &str,
        version: &str,
    ) -> Result<Arc<Release>, ReleaseError> {
        match self.scope_of(host_group, service)? {
            Scope::Service => self.release_verified(service, version),
            Scope::All | Scope::One(_) => self
                .desired
                .release(service, version)
                .ok_or(ReleaseError::Unknown),
        }
    }

    /// Where the hosts of `service` stand, as a call that names the group `host_group` (empty:
    /// none) reads it, once the caller is checked.
    ///
    /// # Errors
    /// [`ConvergenceError::UnknownService`], [`ConvergenceError::NotInHostGroup`].
    pub(crate) fn convergence_in(
        &self,
        host_group: &str,
        service: &str,
    ) -> Result<Convergence, ConvergenceError> {
        match self.scope_of(host_group, service)? {
            Scope::Service => {
                let def = self
                    .services
                    .get(service)
                    .ok_or(ConvergenceError::UnknownService)?;
                Ok(self.convergence_of_def(def))
            }
            scope => Ok(self.read_groups(service, &scope, None).convergence),
        }
    }

    /// How the writes of `service` are recorded, as the call that names the group `host_group`
    /// (empty: none) shows them: the group's, or, with none named, the only group's that has a
    /// desired version of the package; none when several have.
    pub(crate) fn write_record_in(&self, host_group: &str, service: &str) -> Option<WriteRecord> {
        match self.scope_of(host_group, service) {
            Ok(Scope::One(group)) => self.desired.group_record(&group, service),
            Ok(Scope::All) => {
                let mut records: Vec<WriteRecord> = self
                    .desired
                    .host_groups()
                    .0
                    .keys()
                    .filter_map(|group| self.desired.group_record(group, service))
                    .collect();
                (records.len() == 1).then(|| records.remove(0))
            }
            Ok(Scope::Service) | Err(_) => self.desired.write_record(service),
        }
    }

    /// Where the hosts of the package `package` stand in the groups `scope` reaches that have a
    /// desired version of it, now. A call that waits for `against` names it: see [`aggregate`].
    pub(crate) fn read_groups(
        &self,
        package: &str,
        scope: &Scope,
        against: Option<&str>,
    ) -> Reading {
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        let ops = self.ops.state();
        let groups = self.desired.host_groups();
        let parts: Vec<Convergence> = groups
            .0
            .iter()
            .filter(|(name, _)| match scope {
                Scope::One(only) => *name == only,
                Scope::All | Scope::Service => true,
            })
            .filter_map(|(name, group)| {
                let def = package_def(name, group, &groups, &self.services, &ops, package)?;
                let want = self.desired.group_desired(name, package)?;
                Some(self.pair_convergence(&def, Some(&want), &fleet, &ops, now))
            })
            .collect();
        Reading {
            pairs: parts.len(),
            convergence: aggregate(parts, fleet.is_quiet(now), against),
        }
    }

    /// Where the hosts of `def` stand against `desired`: the convergence of the rollout file's
    /// service, or, for a service made for a host group, the same over the group's hosts, with a
    /// group that has no hosts converged.
    pub(crate) fn pair_convergence(
        &self,
        def: &ServiceDef,
        desired: Option<&crate::desired::Desired>,
        fleet: &Fleet,
        ops: &OpsState,
        now: std::time::Duration,
    ) -> Convergence {
        let convergence = convergence_of(def, desired, fleet, ops, now);
        let Some(group) = def.group() else {
            return convergence;
        };
        let counted = convergence.state == State::InProgress
            && desired.is_some()
            && convergence.roles.is_empty()
            && convergence.hosts.is_none();
        if counted && !self.group_has_hosts(group, fleet) {
            return Convergence {
                state: State::Converged,
                ..convergence
            };
        }
        convergence
    }

    /// Whether the group `group` has a host: the fleet has heard of one in it, or the enrollment
    /// store holds one. A store that cannot be read says it has.
    fn group_has_hosts(&self, group: &GroupName, fleet: &Fleet) -> bool {
        fleet
            .nodes()
            .any(|(_, node)| group.is_all() || node.role == group.as_str())
            || !self.hosts_in(group).is_ok_and(|hosts| hosts.is_empty())
    }

    /// The status of every pair of a group and a package it lists, by group and package, at
    /// `now`: the status of the service made for the group.
    pub(crate) fn group_statuses(
        &self,
        fleet: &Fleet,
        ops: &OpsState,
        now: std::time::Duration,
    ) -> std::collections::BTreeMap<(GroupName, String), ServiceStatus> {
        let groups = self.desired.host_groups();
        let mut statuses = std::collections::BTreeMap::new();
        for (name, group) in &groups.0 {
            let services = group_services(name, group, &groups, &self.services, ops, &[]);
            let all = self.desired.group_desireds(name);
            for def in group
                .packages
                .iter()
                .filter_map(|package| services.get(package))
            {
                let desired = all.get(&def.name).cloned();
                let status = self.status_of(def, &services, &all, desired, fleet, ops, now);
                statuses.insert((name.clone(), def.name.clone()), status);
            }
        }
        statuses
    }
}
