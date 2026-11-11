//! A host moved into another host group is moving until its packages match the group
//! (`docs/design/hostgroups.md` section 5). `HostGroups.Move` writes the host's record, which
//! names the group the host moves from ([`crate::host_move`]); this is the sequence that follows,
//! decided at each of the host's heartbeats.
//!
//! 1. **Admission.** For every package of the group that the host must install or change (it has
//!    a desired version, the host can be ordered it, and the host does not run it at that
//!    version), the package's plan must be finished, or hold a free place in its current stage
//!    or already a place of this host, and be neither paused, halted nor at a version marked bad.
//!    Until every one is, the host is ordered nothing and is told [`crate::decide::Gate::Moving`]. A coordinator
//!    in its quiet start admits no one. When the host is admitted it holds a place in each of
//!    those plans, as a host that joins does: it counts as started in them ([`crate::stages`]).
//!    The admission is kept in the fleet's memory, per package and version, and is made again after
//!    a restart.
//! 2. **Facts.** The move raised the host's facts generation ([`crate::facts`]), and the host
//!    is sent its facts, which name the new group. A host whose daemon announced `facts-v1` over
//!    gRPC is ordered nothing, and is told [`crate::decide::Gate::Moving`], until its heartbeat
//!    reports the generation, decided at each heartbeat: a change of the host's facts during the
//!    move holds it again until it reports that one. A host whose daemon did not announce the
//!    feature is not held for facts.
//! 3. **Uninstalls.** The host's uninstall intents run, then its installs and updates wait for
//!    them ([`crate::decide::Gate::Moving`]).
//! 4. **Installs and updates.** The packages of the group are ordered in the order of its list
//!    ([`crate::install_order`]), back to back, without the plan's gate ([`crate::stages::gate`],
//!    [`crate::stages::uninstall_gate`]) for the packages and versions it was admitted to, and under
//!    every other rule of the decision ([`crate::decide`]).
//!
//! The move ends, at a heartbeat, once no uninstall intent of the host stands that it lists and
//! no operator holds, and every package of the group is at its desired version or cannot be
//! ordered it to the host: the host record's `moving_from` is cleared. A pair an operator holds
//! on the host, one it is excluded from and one whose desired version is below the host's minimum
//! version are the host's own and left aside; the move goes on around them. A second move
//! replaces the target: [`Fleet::begin_move`] drops the admission.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_enrollment::enroll::{EnrollmentStore, Membership, StoreError};
use hostkeeper_enrollment::names::HostId as EnrolledId;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use crate::decide::{consider, heard};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::group_scope::Basis;
use crate::host_groups::GroupName;
use crate::service::Coordinator;
use crate::services::{ServiceDef, Services};
use crate::stages::{self, HostState};

/// The moves under way, by host.
#[derive(Clone, Debug, Default)]
pub(crate) struct Moves(BTreeMap<HostId, Sequence>);

/// One host's move.
#[derive(Clone, Debug)]
struct Sequence {
    /// The group the host moves into.
    to: GroupName,
    /// The packages the host was admitted to, each with the desired version it was admitted at;
    /// none until the host is admitted.
    admitted: Option<BTreeMap<String, Version>>,
    /// Whether the host's daemon has not reported the generation of its facts yet, as of the
    /// host's last heartbeat.
    facts_pending: bool,
}

impl Fleet {
    /// Begins the move of the host `id` into `to`, dropping the admission of a move under way.
    pub(crate) fn begin_move(&mut self, id: HostId, to: GroupName) {
        self.moves.0.insert(
            id,
            Sequence {
                to,
                admitted: None,
                facts_pending: false,
            },
        );
    }

    /// Makes the move of the host `id` what its record says: under way into `group` when
    /// `moving` and in a group, else none. A move the fleet holds for another group is replaced.
    pub(crate) fn sync_move(&mut self, id: &HostId, group: Option<&GroupName>, moving: bool) {
        let Some(group) = group.filter(|_| moving) else {
            self.moves.0.remove(id);
            return;
        };
        if self.moves.0.get(id).is_none_or(|move_| move_.to != *group) {
            self.begin_move(id.clone(), group.clone());
        }
    }

    /// Whether the host `id` is moving.
    pub(crate) fn is_moving(&self, id: &HostId) -> bool {
        self.moves.0.contains_key(id)
    }

    /// Whether the host `id` is moving and not admitted yet.
    pub(crate) fn waits_to_move(&self, id: &HostId) -> bool {
        self.moves
            .0
            .get(id)
            .is_some_and(|move_| move_.admitted.is_none())
    }

    /// Whether the host `id` is moving and is ordered nothing for now: it is not admitted yet, or
    /// its daemon has not reported its facts' generation.
    pub(crate) fn holds_moving(&self, id: &HostId) -> bool {
        self.waits_to_move(id)
            || self
                .moves
                .0
                .get(id)
                .is_some_and(|move_| move_.facts_pending)
    }

    /// Notes whether the move of the host `id` waits for the host to report its facts.
    fn await_facts(&mut self, id: &HostId, pending: bool) {
        self.moves
            .0
            .get_mut(id)
            .into_iter()
            .for_each(|move_| move_.facts_pending = pending);
    }

    /// Whether the host `id` is moving and admitted: its uninstalls go past the plan's gate.
    pub(crate) fn is_admitted(&self, id: &HostId) -> bool {
        self.moves
            .0
            .get(id)
            .is_some_and(|move_| move_.admitted.is_some())
    }

    /// The version of `def` the host `id` was admitted to, if its move into the group of `def`
    /// admitted it to the package.
    pub(crate) fn admitted_at(&self, def: &ServiceDef, id: &HostId) -> Option<&Version> {
        let move_ = self
            .moves
            .0
            .get(id)
            .filter(|move_| def.group() == Some(&move_.to))?;
        move_.admitted.as_ref()?.get(&def.name)
    }

    /// Admits the host `id` to the packages `packages`, each at its version.
    pub(crate) fn admit(&mut self, id: &HostId, packages: BTreeMap<String, Version>) {
        self.moves
            .0
            .entry(id.clone())
            .and_modify(|move_| move_.admitted = Some(packages));
    }

    /// Whether the host `id` has a package to uninstall in its move: an intent of the host for a
    /// package that the group does not list, that it lists, and that no hold or exclusion leaves
    /// alone.
    pub(crate) fn uninstalls_pending(&self, services: &Services, id: &HostId) -> bool {
        services
            .defs()
            .filter(|def| def.roles.is_empty())
            .any(|def| {
                let name = def.name.as_str();
                self.uninstall_order(name, id).is_some()
                    && self.lists(id, name)
                    && !self.is_held(name, id)
                    && !self.is_excluded(name, id)
            })
    }
}

/// Whether the host `id` must install or change `def`, wanted at `want`: the host's own holds,
/// exclusions and minimum version leave the pair aside, as do a host that runs the version, and
/// a package the host cannot be ordered the version of (no target for it, no digest, an agent
/// that cannot install, a host the fleet has no word of the package from).
fn needs(def: &ServiceDef, want: &Desired, fleet: &Fleet, id: &HostId, now: Duration) -> bool {
    let name = def.name.as_str();
    let aside = fleet.is_held(name, id)
        || fleet.is_excluded(name, id)
        || fleet
            .min_version(def, id)
            .is_some_and(|floor| !want.version.at_least(floor));
    let at = heard(fleet, name, id).is_some_and(|host| {
        host.beat.health != hostkeeper_wire::wire::Health::Absent
            && host.beat.version == want.version
    });
    !aside && !at && !consider(def, want, &[], fleet, id, now).unreachable
}

/// The packages of the group of `services` that the host `id` must install or change, each with
/// the desired state it is wanted at, in no particular order.
fn needed<'a>(
    services: &'a Services,
    desired: &'a BTreeMap<String, Desired>,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Vec<(&'a ServiceDef, &'a Desired)> {
    services
        .defs()
        .filter(|def| !def.roles.is_empty())
        .filter_map(|def| Some((def, desired.get(&def.name)?)))
        .filter(|(def, want)| needs(def, want, fleet, id, now))
        .collect()
}

/// Whether the plan of `def` at `want` admits the host `id`: it is neither halted, at a version
/// marked bad nor paused, and the host is no member of it, already holds a place of it, or the
/// plan has a free place in its current stage or has rolled every other host.
fn admits(def: &ServiceDef, want: &Desired, fleet: &Fleet, id: &HostId, now: Duration) -> bool {
    if fleet.is_stopped_in(def, &want.version) {
        return false;
    }
    let view = stages::current(def, want, fleet, now);
    if view.paused.is_some() {
        return false;
    }
    let Some(mine) = view.members.iter().find(|member| member.id == *id) else {
        return true;
    };
    let mut others = view
        .members
        .iter()
        .filter(|member| member.id != *id)
        .peekable();
    mine.state != HostState::Waiting
        || view.free() > 0
        || (others.peek().is_some() && others.all(|member| member.state == HostState::Done))
}

/// What the plans admit the host `id` to: each package it must install or change at the version
/// wanted, or none when a plan does not admit it, or the coordinator is in its quiet start.
fn admission(
    needed: &[(&ServiceDef, &Desired)],
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Option<BTreeMap<String, Version>> {
    if fleet.is_quiet(now) {
        return None;
    }
    needed
        .iter()
        .all(|(def, want)| admits(def, want, fleet, id, now))
        .then(|| {
            needed
                .iter()
                .map(|(def, want)| (def.name.clone(), want.version.clone()))
                .collect()
        })
}

/// Ends the move of `host` into `group` in its record, unless the record has the host in
/// another group by now.
pub(crate) fn clear(
    store: &EnrollmentStore,
    host: &EnrolledId,
    group: &GroupName,
) -> Result<(), StoreError> {
    let in_group = store
        .membership(host)?
        .is_some_and(|held| held.host_group == group.as_str());
    if in_group {
        store.set_membership(
            host,
            Membership {
                host_group: group.to_string(),
                moving_from: None,
            },
        )?;
    }
    Ok(())
}

impl Coordinator {
    /// Takes the next step of the move of the host `id`, at its heartbeat and under the fleet's
    /// lock, after the heartbeat is noted: the fleet's move is made what the host's record says
    /// ([`Basis`]); a host whose packages match its group ends its move; a host not admitted yet
    /// is admitted when the plans admit it; a host that must report its facts' generation is held
    /// until it does.
    pub(crate) fn step_move(
        &self,
        fleet: &mut Fleet,
        basis: &Basis<'_>,
        id: &HostId,
        now: Duration,
    ) {
        fleet.sync_move(id, basis.group(), basis.moving());
        let Some(group) = basis.group().filter(|_| basis.moving()) else {
            return;
        };
        let services = &*basis.services;
        let desired = self.desired_for(services);
        self.keep_releases(fleet);
        let to_do = needed(services, &desired, fleet, id, now);
        if to_do.is_empty() && !fleet.uninstalls_pending(services, id) {
            self.end_move(fleet, id, group);
            return;
        }
        if fleet.waits_to_move(id)
            && let Some(admitted) = admission(&to_do, fleet, id, now)
        {
            fleet.admit(id, admitted);
        }
        let pending = fleet
            .facts
            .awaits(id, self.ops.facts_generation(id.as_str()));
        fleet.await_facts(id, pending);
    }

    /// Clears the move of the host `id` into `group` from its record and the fleet; a record that
    /// cannot be written leaves it to the host's next heartbeat.
    fn end_move(&self, fleet: &mut Fleet, id: &HostId, group: &GroupName) {
        let cleared = self
            .enrollment()
            .zip(EnrolledId::parse(id.as_str()).ok())
            .map_or(Ok(()), |(enrollment, enrolled)| {
                clear(enrollment.store(), &enrolled, group)
            });
        match cleared {
            Ok(()) => {
                fleet.moves.0.remove(id);
            }
            Err(why) => tracing::error!(
                %why,
                node_id = %id,
                "a host's move could not be ended in its record: it ends at its next heartbeat"
            ),
        }
    }
}
