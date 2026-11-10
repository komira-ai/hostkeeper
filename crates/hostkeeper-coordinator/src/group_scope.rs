//! The decision by host group (`docs/design/hostgroups.md`, sections 1.4, 2 and 3): what a
//! heartbeat of one host is decided over once the coordinator holds a host group.
//!
//! The coordinator is in group mode while it holds at least one group. In group mode a host runs
//! the packages of its group and nothing the rollout file's roles, selectors or an operator's
//! assignments say places a host. The decision itself is the engine of a coordinator without
//! groups ([`crate::decide`], [`crate::stages`], [`crate::reconcile`], [`crate::rollouts`],
//! [`crate::halt`]), given the group's view of its inputs:
//!
//! - one [`ServiceDef`] per package of the group, in the order of the group's list, made from
//!   the group's policy (stages, canary, bake, rollback, backoff) and the rollout file's
//!   definition of the package, when it has one, for what a package alone has (its version scheme,
//!   its repository and the floor of its minimum version), the group's own minimum version above
//!   it. Its roles are the group's name alone: the role the fleet records for a host in group
//!   mode is the name of its group, and the empty name for a host in none, so a host is
//!   assigned a package exactly when its group lists it, and a hold of a role holds the
//!   group;
//! - the group's desired state of each package ([`crate::desired`]), whose limits are the
//!   policy's `max_updating` and `min_up` for the group's name;
//! - the marks of each (group, package) ([`OpsState::marks_in`]), beside the marks of the package
//!   alone: a version marked bad is bad in every group.
//!
//! The definition carries a [`GroupScope`] naming its group. Every place that keeps a mark or
//! counts hosts per service reads the scope of the definition it is given, and with none reads
//! what it always has.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_enrollment::names::HostId as EnrolledId;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use crate::canary::Canary;
use crate::desired::Desired;
use crate::host_groups::{GroupName, HostGroup};
use crate::identity::Identity;
use crate::ops::OpsState;
use crate::service::Coordinator;
use crate::services::{DEFAULT_REPOSITORY, ServiceDef, Services, VersionScheme};
use crate::stages::Plan;

/// The host group a [`ServiceDef`] was made for, and what the group's policy says that no key of
/// the definition does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupScope {
    /// The group.
    pub(crate) group: GroupName,
    /// How many of the group's packages may roll at once; none: the coordinator's
    /// `--live-rollouts`.
    pub(crate) live_rollouts: Option<usize>,
    /// How many hosts of the group may hold a slot of one package at once: the policy's
    /// `max_updating`, which an uninstall of a package the group no longer lists takes a slot of.
    pub(crate) max_updating: u32,
}

impl ServiceDef {
    /// The group the definition was made for, if it was made for one.
    pub(crate) fn group(&self) -> Option<&GroupName> {
        self.scope.as_ref().map(|scope| &scope.group)
    }
}

/// The higher of two minimum versions, either of which may be absent.
pub(crate) fn higher(a: Option<Version>, b: Option<Version>) -> Option<Version> {
    match (a, b) {
        (Some(a), Some(b)) if a.at_least(&b) => Some(a),
        (Some(_), Some(b)) => Some(b),
        (a, b) => a.or(b),
    }
}

/// The definition of `package` in `group`, listed at `place` in the group's list: the earlier in
/// the list, the higher its priority.
fn def_of(
    group: &GroupName,
    listed: &HostGroup,
    file: &Services,
    state: &OpsState,
    package: &str,
    place: usize,
) -> ServiceDef {
    let base = file.get(package);
    let policy = &listed.policy;
    ServiceDef {
        name: package.to_owned(),
        version_scheme: base.map_or(VersionScheme::default(), |def| def.version_scheme),
        roles: BTreeSet::from([group.to_string()]),
        priority: i32::try_from(listed.packages.len() - place).unwrap_or(i32::MAX),
        selector: None,
        bake_beats: policy.bake_beats,
        canary: policy.canary.map(|canary| Canary {
            count: canary.count,
        }),
        plan: policy.plan().unwrap_or_default(),
        rollback: policy.rollback,
        rollback_after_failed: policy.rollback_after_failed,
        min_version: higher(
            base.and_then(|def| def.min_version.clone()),
            state.group_marks(group, package).min_version,
        ),
        defer_backoff: policy.defer_backoff,
        repository: base.map_or_else(
            || DEFAULT_REPOSITORY.to_owned(),
            |def| def.repository.clone(),
        ),
        scope: Some(GroupScope {
            group: group.clone(),
            live_rollouts: policy.live_rollouts.map(|n| n as usize),
            max_updating: policy.max_updating,
        }),
    }
}

/// The definition of `package` in `group`, as `listed` holds it: none when `listed` does not list
/// the package.
pub(crate) fn package_def(
    group: &GroupName,
    listed: &HostGroup,
    file: &Services,
    state: &OpsState,
    package: &str,
) -> Option<ServiceDef> {
    let place = listed
        .packages
        .iter()
        .position(|listed| listed == package)?;
    Some(def_of(group, listed, file, state, package, place))
}

/// The definition of `package`, which the group lists no longer and a host has the uninstall intent
/// of the generation `generation` for: it runs on no role, so no host is assigned it, and is
/// offered after every package the group lists, the intent of the lowest generation first.
fn leaving_def(
    group: &GroupName,
    listed: &HostGroup,
    file: &Services,
    package: &str,
    generation: u64,
) -> ServiceDef {
    let base = file.get(package);
    ServiceDef {
        name: package.to_owned(),
        version_scheme: base.map_or(VersionScheme::default(), |def| def.version_scheme),
        roles: BTreeSet::new(),
        priority: 0_i32.saturating_sub(i32::try_from(generation).unwrap_or(i32::MAX)),
        selector: None,
        bake_beats: 0,
        canary: None,
        plan: Plan::default(),
        rollback: crate::reconcile::Rollback::Manual,
        rollback_after_failed: crate::reconcile::FailThreshold::default(),
        min_version: None,
        defer_backoff: crate::defer::DeferBackoff::default(),
        repository: base.map_or_else(
            || DEFAULT_REPOSITORY.to_owned(),
            |def| def.repository.clone(),
        ),
        scope: Some(GroupScope {
            group: group.clone(),
            live_rollouts: listed.policy.live_rollouts.map(|n| n as usize),
            max_updating: listed.policy.max_updating,
        }),
    }
}

/// The services of the group `group`, as `listed` holds it, over the rollout file `file` and the
/// operations state `state`: its packages in the order of its list, then the packages in
/// `leaving`, each with the generation of its uninstall intent.
pub(crate) fn group_services(
    group: &GroupName,
    listed: &HostGroup,
    file: &Services,
    state: &OpsState,
    leaving: &[(String, u64)],
) -> Services {
    let packages = listed
        .packages
        .iter()
        .enumerate()
        .map(|(place, package)| def_of(group, listed, file, state, package, place));
    let leaving = leaving
        .iter()
        .filter(|(package, _)| !listed.packages.contains(package))
        .map(|(package, generation)| leaving_def(group, listed, file, package, *generation));
    Services::from_defs(packages.chain(leaving).collect())
}

/// What a heartbeat of one host is decided over: the services it is decided for and, in group
/// mode, its group.
pub(crate) struct Basis<'a> {
    /// The rollout file's services, or the host's group's packages (none for a host in no
    /// group).
    pub(crate) services: Cow<'a, Services>,
    /// Whether the coordinator holds a host group.
    in_group_mode: bool,
    /// The host's group, when it is in one.
    group: Option<GroupName>,
}

impl Basis<'_> {
    /// The role the fleet records for the host `who`: its certificate's, or in group mode the name
    /// of its group, empty for a host in none.
    pub(crate) fn role_of(&self, who: &Identity) -> String {
        if !self.in_group_mode {
            return who.role.name().to_owned();
        }
        self.group
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    /// The host's group, when it is in one.
    pub(crate) fn group(&self) -> Option<&GroupName> {
        self.group.as_ref()
    }
}

impl Coordinator {
    /// The group the enrollment store records for the host `host`: none for a host in none, a host
    /// never enrolled, a coordinator without enrollment and a record that cannot be read (logged).
    pub(crate) fn group_of(&self, host: &HostId) -> Option<GroupName> {
        let enrolled = EnrolledId::parse(host.as_str()).ok();
        let read = self
            .enrollment()
            .zip(enrolled)
            .map(|(enrollment, id)| enrollment.store().membership(&id));
        match read {
            Some(Ok(Some(membership))) => membership.host_group.parse().ok(),
            Some(Err(why)) => {
                tracing::error!(%why, node_id = %host, "cannot read an enrolled host's group: it is in none");
                None
            }
            Some(Ok(None)) | None => None,
        }
    }

    /// The desired state of `service` for the host `basis` is of: the coordinator's, or in group
    /// mode its group's; none for a host in no group.
    pub(crate) fn desired_in(&self, basis: &Basis<'_>, service: &str) -> Option<Desired> {
        if !basis.in_group_mode {
            return self.desired.get(service);
        }
        self.desired.group_desired(basis.group()?, service)
    }

    /// The desired state of every package of `services`, by package: the coordinator's, or, for
    /// the services of a host group, the group's.
    pub(crate) fn desired_for(&self, services: &Services) -> BTreeMap<String, Desired> {
        match services.group() {
            None => self.desired.all(),
            Some(group) => self.desired.group_desireds(group),
        }
    }

    /// What a heartbeat of `host` is decided over ([`Basis`]).
    pub(crate) fn basis_of(&self, host: &HostId) -> Basis<'_> {
        if !self.desired.in_group_mode() {
            return Basis {
                services: Cow::Borrowed(&self.services),
                in_group_mode: false,
                group: None,
            };
        }
        let listed = self
            .group_of(host)
            .and_then(|name| Some((self.desired.host_group(&name)?, name)));
        let services = listed
            .as_ref()
            .map_or_else(Services::default, |(listed, name)| {
                self.ops.read(|state| {
                    let leaving: Vec<(String, u64)> = state
                        .assignments
                        .iter()
                        .filter_map(|(package, entries)| {
                            let generation = entries.uninstall.get(host.as_str())?;
                            Some((package.clone(), *generation))
                        })
                        .collect();
                    group_services(name, listed, &self.services, state, &leaving)
                })
            });
        Basis {
            services: Cow::Owned(services),
            in_group_mode: true,
            group: listed.map(|(_, name)| name),
        }
    }
}
