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
use crate::holds::HoldMark;
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
    /// The services that are halted.
    pub halted: BTreeSet<String>,
    /// Per service, the versions marked bad.
    pub bad: BTreeMap<String, Vec<Version>>,
    /// Per service, the failures an operator acknowledged, each a node id and the version it
    /// failed, by its text: a cleared halt's, and those a version set by hand or forced acknowledged.
    pub acked: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Per host group and package, where its update plan stands.
    pub group_rollouts: BTreeMap<String, BTreeMap<String, Rollout>>,
    /// Per host group, the packages that are halted in it.
    pub group_halted: BTreeMap<String, BTreeSet<String>>,
    /// Per host group and package, the hosts its standing force orders.
    pub group_forced: BTreeMap<String, BTreeMap<String, BTreeSet<HostId>>>,
    /// Whether the coordinator holds a host group: the roles and hosts of an operator's
    /// assignments, and the minimum versions of a role, then place and bound nothing.
    pub(crate) group_mode: bool,
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

    /// Whether the coordinator holds a host group, as of the last refresh of the marks.
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

    /// Whether `def` is halted: its group's halt, for a service made for a group.
    #[must_use]
    pub(crate) fn is_halted_in(&self, def: &ServiceDef) -> bool {
        match def.group() {
            None => self.marks.halted.contains(&def.name),
            Some(group) => self
                .marks
                .group_halted
                .get(group.as_str())
                .is_some_and(|halted| halted.contains(&def.name)),
        }
    }

    /// The versions of `service` marked bad: in every group alike.
    #[must_use]
    pub(crate) fn bad_versions(&self, service: &str) -> &[Version] {
        self.marks.bad.get(service).map_or(&[], Vec::as_slice)
    }

    /// Whether `def` is halted, or `version` of it is marked bad: its plan orders no one. A
    /// service made for a group is halted by its group's halt; a version is bad in every group.
    #[must_use]
    pub(crate) fn is_stopped_in(&self, def: &ServiceDef, version: &Version) -> bool {
        self.is_halted_in(def) || self.bad_versions(&def.name).contains(version)
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

    /// How many services may have a live rollout at once for `def`: its group's policy says, else
    /// the coordinator's.
    #[must_use]
    pub(crate) fn live_limit(&self, def: &ServiceDef) -> usize {
        def.scope
            .as_ref()
            .and_then(|scope| scope.live_rollouts)
            .unwrap_or(self.live_rollouts)
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
}
