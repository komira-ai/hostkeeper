//! The operator's marks the fleet decides by, and how the fleet reads them for a service made for a
//! host group ([`crate::group_scope`]): the plan position, the halt and the standing force of a
//! (group, package) are kept beside those of the package alone, and the hosts a count reads are
//! those whose role is the group's name, which is what the fleet records as a host's role in
//! group mode.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use super::{Fleet, Host};
use crate::assignment::ServiceAssignments;
use crate::halt::HaltScope;
use crate::holds::HoldMark;
use crate::host_groups::GroupName;
use crate::rollouts::Rollout;
use crate::services::ServiceDef;

/// What the fleet keeps of the operator's marks for its decisions: a copy of the holds, of the
/// hosts each standing force orders, of the assignments, the labels and the exclusions, of where
/// each service's update plan stands (`rollouts`), of the services that are halted, of the
/// versions marked bad and of the failures an operator acknowledged (`acked`), as the ops store
/// holds them ([`crate::holds::marks_of`]), refreshed under the fleet's lock
/// ([`Fleet::set_marks`]). The plan positions, halts and forces of a package in a host group are
/// kept apart, by group and then package, and read for a service made for the group
/// ([`Fleet::rollout_in`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// The holds that stand.
    pub holds: Vec<HoldMark>,
    /// Per service, the hosts its standing force orders ([`crate::force`]).
    pub forced: BTreeMap<String, BTreeSet<HostId>>,
    /// Per service, the operator's assignments ([`crate::assignment`]).
    pub assignments: BTreeMap<String, ServiceAssignments>,
    /// Per node id, the labels an operator gave the host.
    pub labels: BTreeMap<String, BTreeMap<String, String>>,
    /// The (service, node id) pairs an operator excluded.
    pub excluded: BTreeSet<(String, String)>,
    /// Per service, where its update plan stands ([`crate::rollouts`]).
    pub rollouts: BTreeMap<String, Rollout>,
    /// The services that are halted (whom each halt holds: `halt_scopes`).
    pub halted: BTreeSet<String>,
    /// Per service, the versions marked bad.
    pub bad: BTreeMap<String, Vec<Version>>,
    /// Per service, the failures an operator acknowledged, each a node id and the version it
    /// failed, by its text: a cleared halt's, and those a version set by hand or forced acknowledged.
    pub acked: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Per host group and package, where its update plan stands.
    pub group_rollouts: BTreeMap<String, BTreeMap<String, Rollout>>,
    /// Per host group, the packages that are halted in it (whom each halt holds: `halt_scopes`).
    pub group_halted: BTreeMap<String, BTreeSet<String>>,
    /// Per host group and package, the hosts its standing force orders.
    pub group_forced: BTreeMap<String, BTreeMap<String, BTreeSet<HostId>>>,
    /// Whether the coordinator holds a host group: the roles and hosts of an operator's
    /// assignments, and the minimum versions of a role, then place and bound nothing.
    pub(crate) group_mode: bool,
    /// The hosts in their turn of a maintenance run (`docs/design/maintenance-updates.md`, section
    /// 5.1; [`crate::maintenance_runs::Step::in_turn`]): from the order that starts the turn until
    /// it is done, deferred or put back. A package plan leaves them out and `min_up` counts them
    /// down ([`crate::stages`]), and no package order goes to them ([`crate::decide`]).
    pub(crate) in_maintenance: BTreeSet<HostId>,
    /// Whom the standing halt of each halted service holds ([`HaltScope`]), by the host group it
    /// stands in (`None`: the service's own) and the service. A service `halted` or
    /// `group_halted` names with no scope here holds every host.
    pub(crate) halt_scopes: BTreeMap<(Option<String>, String), HaltScope>,
}

impl Marks {
    /// These marks in the mode `previous` was in: in group mode, without the assignments of a
    /// role or a host and the minimum versions of a role, which place nothing and bound nothing
    /// there. What stays of a service's assignments is the uninstall intents and the minimum
    /// versions of a host.
    pub(super) fn in_mode_of(mut self, previous: &Self) -> Self {
        self.group_mode = previous.group_mode;
        self.placed_by_group()
    }

    fn placed_by_group(mut self) -> Self {
        if self.group_mode {
            for entries in self.assignments.values_mut() {
                entries.roles.clear();
                entries.hosts.clear();
                entries.role_min_versions.clear();
            }
        }
        self
    }
}

impl Fleet {
    /// The same fleet reading the marks of an operator's assignments as it does in group mode when
    /// `on` ([`Marks`]), from the next [`Self::set_marks`] on, and as it always has otherwise.
    pub(crate) fn set_group_mode(&mut self, on: bool) {
        self.marks.group_mode = on;
    }

    /// Whether the host `id` is in its turn of a maintenance run, as of the last refresh of the
    /// marks ([`Marks::in_maintenance`]).
    #[must_use]
    pub(crate) fn in_maintenance(&self, id: &HostId) -> bool {
        self.marks.in_maintenance.contains(id)
    }

    /// Whether the coordinator is in group mode (it holds a host group, or places by groups only),
    /// as of the last refresh of the marks.
    #[must_use]
    pub(crate) fn in_group_mode(&self) -> bool {
        self.marks.group_mode
    }

    /// Where the update plan of `def` stands: its group's, for a service made for a group.
    #[must_use]
    pub(crate) fn rollout_in(&self, def: &ServiceDef) -> Option<&Rollout> {
        match def.group() {
            None => self.rollout_of(&def.name),
            Some(group) => self
                .marks
                .group_rollouts
                .get(group.as_str())?
                .get(&def.name),
        }
    }

    /// Whether the halt of `service` in `group` (`None`: the service's own) holds every host
    /// while `desired` is its desired version, or, given `id`, holds that host
    /// ([`HaltScope::holds_every_host`], [`HaltScope::holds`]). A halted service whose marks
    /// name no scope holds every host.
    pub(super) fn halt_holds(
        &self,
        group: Option<&str>,
        service: &str,
        desired: Option<&Version>,
        id: Option<&HostId>,
    ) -> bool {
        let halted = match group {
            None => self.marks.halted.contains(service),
            Some(group) => self
                .marks
                .group_halted
                .get(group)
                .is_some_and(|halted| halted.contains(service)),
        };
        let key = (group.map(str::to_owned), service.to_owned());
        halted
            && self
                .marks
                .halt_scopes
                .get(&key)
                .is_none_or(|scope| match id {
                    None => scope.holds_every_host(desired),
                    Some(id) => scope.holds(desired, id.as_str()),
                })
    }

    /// Whether the halt of `def` (its group's, for a service made for a group) holds the host
    /// `id` while `desired` is its desired version ([`Self::halt_holds`]).
    #[must_use]
    pub(crate) fn halts_host_in(
        &self,
        def: &ServiceDef,
        desired: Option<&Version>,
        id: &HostId,
    ) -> bool {
        let group = def.group().map(GroupName::as_str);
        self.halt_holds(group, &def.name, desired, Some(id))
    }

    /// The versions of `service` marked bad: in every group alike.
    #[must_use]
    pub(crate) fn bad_versions(&self, service: &str) -> &[Version] {
        self.marks.bad.get(service).map_or(&[], Vec::as_slice)
    }

    /// Whether a halt of `def` holds every host of it at `version` ([`Self::halt_holds`]),
    /// or `version` of it is marked bad: its plan orders no one. A service made for a group is
    /// halted by its group's halt; a version is bad in every group.
    #[must_use]
    pub(crate) fn is_stopped_in(&self, def: &ServiceDef, version: &Version) -> bool {
        let group = def.group().map(GroupName::as_str);
        self.halt_holds(group, &def.name, Some(version), None)
            || self.bad_versions(&def.name).contains(version)
    }

    /// Whether the standing force of `def` orders the host `id`: its group's, for a service made
    /// for a group.
    #[must_use]
    pub(crate) fn is_forced_in(&self, def: &ServiceDef, id: &HostId) -> bool {
        match def.group() {
            None => self.is_forced(&def.name, id),
            Some(group) => self
                .marks
                .group_forced
                .get(group.as_str())
                .and_then(|packages| packages.get(&def.name))
                .is_some_and(|hosts| hosts.contains(id)),
        }
    }

    /// Whether the node `id` is in the group of `def`, or `def` is made for none.
    fn in_group_of(&self, def: &ServiceDef, id: &HostId) -> bool {
        def.group().is_none_or(|group| {
            self.node(id)
                .is_some_and(|node| node.role == group.as_str())
        })
    }

    /// [`Self::hosts_of`] for `def`: the hosts of its group alone, for a service made for one.
    pub(crate) fn hosts_in<'a>(
        &'a self,
        def: &'a ServiceDef,
    ) -> impl Iterator<Item = (&'a HostId, &'a Host)> {
        self.hosts_of(&def.name).filter(|(_, host)| {
            def.group()
                .is_none_or(|group| host.role_name() == group.as_str())
        })
    }

    /// [`Self::occupants_of`] for `def`: the hosts of its group alone, for a service made for one.
    pub(crate) fn occupants_in<'a>(
        &'a self,
        def: &'a ServiceDef,
    ) -> impl Iterator<Item = (&'a HostId, &'a Host)> {
        self.occupants_of(&def.name)
            .filter(|(id, _)| self.in_group_of(def, id))
    }

    /// [`Self::ordered_unseen`] for `def`: the hosts of its group alone, for a service made for one.
    pub(crate) fn ordered_unseen_in<'a>(
        &'a self,
        def: &'a ServiceDef,
        now: Duration,
    ) -> impl Iterator<Item = &'a HostId> {
        self.ordered_unseen(&def.name, now)
            .filter(|id| self.in_group_of(def, id))
    }

    /// How many services may have a live rollout at once for `def`: the coordinator's
    /// `--live-rollouts`, which its group's policy can only lower.
    #[must_use]
    pub(crate) fn live_limit(&self, def: &ServiceDef) -> usize {
        def.scope
            .as_ref()
            .and_then(|scope| scope.live_rollouts)
            .map_or(self.live_rollouts, |own| own.min(self.live_rollouts))
    }

    /// The packages of every group at the last take of the lock ([`crate::queue`]).
    pub(crate) fn cluster(&self) -> &crate::queue::Cluster {
        &self.cluster
    }

    /// Keeps `entries` as the cluster, at `now`.
    pub(crate) fn set_cluster(&mut self, entries: Vec<crate::queue::Entry>, now: Duration) {
        self.cluster = self.cluster.refreshed(entries, now);
    }

    /// [`Self::keep_verdicts_of`] for `def`: of a service made for a group, only the verdicts of
    /// the hosts of the group are forgotten, those of another group's hosts being about another
    /// desired version.
    pub(crate) fn keep_verdicts_in(
        &mut self,
        def: &ServiceDef,
        version: &Version,
        acked: &[crate::ops::FailureAck],
    ) {
        let nodes = &self.nodes;
        self.verdicts
            .keep_only_of(&def.name, version, acked, &|id| {
                def.group().is_none_or(|group| {
                    nodes
                        .get(id)
                        .is_some_and(|node| node.role == group.as_str())
                })
            });
    }

    /// Puts the host `id` in the group `group` in every record of it: its node and what each
    /// service last heard of it, counted or not, so that no count reads its old group after it
    /// is moved. The host's other fields are as its last heartbeat left them.
    pub(crate) fn regroup(&mut self, id: &HostId, group: &str) {
        if let Some(node) = self.nodes.get_mut(id) {
            group.clone_into(&mut node.role);
        }
        for hosts in self.hosts.values_mut().chain(self.outside.values_mut()) {
            if let Some(host) = hosts.get_mut(id) {
                group.clone_into(&mut host.role);
            }
        }
    }

    /// Notes that the canary `node_id` failed the release of `version` of `service`; a later
    /// canary failing the same version replaces it.
    pub fn note_failed_canary(&mut self, service: &str, version: &Version, node_id: &str) {
        self.canaries.insert(
            (service.to_owned(), version.as_str().to_owned()),
            node_id.to_owned(),
        );
    }

    /// The canaries that failed a release of `service`: each version, by its text, with the node
    /// id of the canary that failed it.
    pub fn failed_canaries(&self, service: &str) -> impl Iterator<Item = (&str, &str)> {
        self.canaries
            .iter()
            .filter(move |((s, _), _)| s == service)
            .map(|((_, version), node)| (version.as_str(), node.as_str()))
    }
}
