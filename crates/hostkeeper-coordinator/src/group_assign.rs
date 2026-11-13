//! The calls of `hostkeeper.operator.v1.Services` that read or write by host group once the
//! coordinator holds one (`docs/design/hostgroups.md`, sections 4 and 7.2):
//!
//! - `SetMinVersion` of a role scope sets the minimum version of the package in the group of that
//!   name: `unknown_host_group` when no group has the name, `not_in_host_group` when the group
//!   does not list the package, `below_min_version` when it is above the group's desired version.
//!   It writes the group's minimum ([`crate::host_groups::GroupMarks::min_version`]), audits
//!   `set_min_version` naming the group and does not raise the group's generation. A host scope
//!   is as it was, except that its ceiling is the desired version of the host's group.
//! - `List` answers each package the rollout file defines or a group lists, with the groups that
//!   list it (`host_groups`), the hosts of those groups, the minimum version of each group as a
//!   role scope named by the group, and its desired version when every group that has one agrees.
//!   The roles and assignments, which place nothing, are empty.
//!
//! `Assign` and `Unassign` are refused `placed_by_host_group` in [`super::Coordinator::assign`] and
//! [`super::Coordinator::unassign`], before anything is read.

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::min_version;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use super::{Refusal, assigned_hosts};
use crate::assignment::Scope;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::group_reach::lists;
use crate::group_scope::package_def;
use crate::holds::marks_of;
use crate::host_groups::GroupName;
use crate::operator::{entry_proto, hold_proto};
use crate::ops::{AuditEntry, OpsState, Write};
use crate::service::Coordinator;

impl Coordinator {
    /// The desired state that bounds a minimum version set for `scope`: the package's, or, for a
    /// host of a coordinator that holds a group, the host's group's.
    pub(super) fn desired_for_scope(
        &self,
        fleet: &Fleet,
        scope: &Scope,
        package: &str,
    ) -> Option<Desired> {
        match scope {
            Scope::Host(id) if self.desired.in_group_mode() => id
                .parse::<HostId>()
                .ok()
                .and_then(|id| fleet.node(&id))
                .and_then(|node| node.role.parse::<GroupName>().ok())
                .and_then(|group| self.desired.group_desired(&group, package)),
            _ => self.desired.get(package),
        }
    }

    /// `Services.SetMinVersion` of the role scope `role`, a group's name, for a coordinator that
    /// holds a host group.
    ///
    /// # Errors
    /// `unknown_host_group`, `not_in_host_group`, `below_min_version`, `store_failed`,
    /// `audit_unavailable`.
    pub(super) fn set_group_min_version(
        &self,
        by: &str,
        package: &str,
        role: &str,
        version: Option<Version>,
    ) -> Result<proto::SetMinVersionResponse, Refusal> {
        let mut fleet = self.lock_fleet();
        let (group, listed) = role
            .parse::<GroupName>()
            .ok()
            .and_then(|name| Some((name.clone(), self.desired.host_group(&name)?)))
            .ok_or(Refusal::UnknownHostGroup)?;
        if !lists(&listed, package) {
            return Err(Refusal::NotInHostGroup);
        }
        let above = version.as_ref().is_some_and(|floor| {
            self.desired
                .group_desired(&group, package)
                .is_some_and(|desired| !desired.version.at_least(floor))
        });
        if above {
            return Err(Refusal::BelowMinVersion);
        }
        let said = version
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string);
        let mut audit = AuditEntry::new(by, "set_min_version");
        audit.service = Some(package.to_owned());
        audit.host_group = Some(group.to_string());
        audit.reason = Some(format!("host group {group}: {said}"));
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            state
                .host_groups
                .0
                .entry(group.clone())
                .or_default()
                .entry(package.to_owned())
                .or_default()
                .min_version = version;
            Ok::<(), Refusal>(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::SetMinVersionResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// Every package the rollout file defines or a group lists, in name order, as `Services.List`
    /// answers them in group mode (the module says how).
    pub(super) fn entries_in_groups(
        &self,
        fleet: &Fleet,
        state: &OpsState,
    ) -> Vec<proto::ServiceEntry> {
        let groups = self.desired.host_groups();
        let mut packages: BTreeSet<&str> = self.services.names().collect();
        packages.extend(
            groups
                .0
                .values()
                .flat_map(|listed| listed.packages.iter().map(String::as_str)),
        );
        packages
            .into_iter()
            .map(|package| {
                let reached: Vec<_> = groups
                    .0
                    .iter()
                    .filter_map(|(name, listed)| {
                        let def =
                            package_def(name, listed, &groups, &self.services, state, package)?;
                        Some((name, def))
                    })
                    .collect();
                let assigned: Vec<proto::AssignedHost> = reached
                    .iter()
                    .flat_map(|(_, def)| assigned_hosts(fleet, state, def))
                    .collect();
                let own = state.assignments.get(package);
                let host_mins = own.into_iter().flat_map(|entries| {
                    entries.host_min_versions.iter().map(|(id, version)| {
                        (min_version::Scope::Host(id.clone()), version.to_string())
                    })
                });
                let group_mins = reached.iter().filter_map(|(name, _)| {
                    let own_floor = state.group_marks(name, package).min_version?;
                    Some((
                        min_version::Scope::Role(name.to_string()),
                        own_floor.to_string(),
                    ))
                });
                let versions: BTreeSet<String> = reached
                    .iter()
                    .filter_map(|(name, _)| self.desired.group_desired(name, package))
                    .map(|desired| desired.version.to_string())
                    .collect();
                let mut found = versions.into_iter();
                let agreed = found.next().filter(|_| found.next().is_none());
                proto::ServiceEntry {
                    service: package.to_owned(),
                    hosts: assigned.iter().map(|host| host.node_id.clone()).collect(),
                    min_versions: group_mins
                        .chain(host_mins)
                        .map(|(scope, version)| proto::MinVersion {
                            scope: Some(scope),
                            version,
                        })
                        .collect(),
                    holds: state
                        .holds
                        .iter()
                        .filter(|hold| hold.service == package)
                        .map(hold_proto)
                        .collect(),
                    desired_version: agreed,
                    roles: Vec::new(),
                    assignments: Vec::new(),
                    assigned,
                    repository: state.repository_of(&self.services, package),
                    uninstalling: own
                        .into_iter()
                        .flat_map(|entries| entries.uninstall.keys().cloned())
                        .collect(),
                    host_groups: reached.iter().map(|(name, _)| name.to_string()).collect(),
                }
            })
            .collect()
    }
}
