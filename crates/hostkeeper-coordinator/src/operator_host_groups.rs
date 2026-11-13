//! `HostGroups` of `hostkeeper.operator.v1`, as plain functions over its messages, as the other
//! operator calls are ([`crate::operator`]): `List`, `Get`, `Put` and `Remove`; `Move` is in
//! [`crate::host_move`]. The groups themselves are in [`crate::host_groups`].
//!
//! Every call takes the operator token only: a valid CI token is `forbidden`, and no token or
//! any other `unauthorized`, before the request is looked at. `List` and `Get` change nothing.
//! `Put`, `Remove` and `Move` take a reason and answer the audit entry they wrote; the change and
//! its entry are one commit ([`crate::ops::OpsStore::decide_changes_with`]), so a change whose
//! audit line cannot be appended is refused `audit_unavailable` and not made, and one whose
//! commit fails `store_failed`. They run under the fleet's lock, so no heartbeat is decided
//! between a change and the marks it makes.
//!
//! A group is in a host's reach through the host's record in the enrollment store: a host is in
//! the group its record names ([`hostkeeper_enrollment::enroll::Membership`]). A coordinator
//! without enrollment holds no host, so every count is 0.
//!
//! `Put` creates a group or replaces one whole. A replacement keeps the group's desired version
//! of each package it still lists and the marks of those packages; it drops both for a package it
//! no longer lists, but the minimum version of that package, which only `Services.SetMinVersion`
//! changes. The group's generation is raised by one (1 for a new group). A package taken out of
//! the list is uninstalled from the group's hosts that listed it at their last heartbeat: one
//! uninstall intent for each, in the reverse of the old list's order ([`crate::group_uninstall`]).
//! A package put back cancels the intents of the group's hosts for it.
//!
//! The audit entry of a `Put` is `host_group_put` about the group, and its reason names what
//! changed, then the operator's reason: `new group` for a group that did not exist, `packages`
//! followed by `+<package>` for each added and `-<package>` for each taken out, `reordered` when
//! the packages that stayed changed their order, `policy` followed by the keys that changed, in
//! the policy's order, or `no change`; the parts are joined by `; ` and followed by `: ` and the
//! operator's reason. Each intent a `Put` cancels is an `uninstall_cancelled` entry after it.
//! The entry of a `Remove` is `host_group_remove` about the group, with the operator's reason.

use std::collections::BTreeMap;

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::group_scope::group_services;
use crate::group_uninstall::{cancel_uninstalls, lost_by, order_uninstalls, runs};
use crate::grpc::report;
use crate::holds::marks_of;
use crate::host_group_proto::{body_proto, policy_of};
use crate::host_groups::{GroupName, HostGroup, ListError, check_packages_for};
use crate::inventory::unreadable;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsState};
use crate::service::Coordinator;

/// The most groups or hosts one page of `List` or `Get` holds.
pub(crate) const MAX_PAGE: usize = 256;

/// How many items a request for `limit` gets: `limit`, at most [`MAX_PAGE`]; 0 is the most.
pub(crate) fn page_size(limit: u32) -> usize {
    match usize::try_from(limit).unwrap_or(MAX_PAGE).min(MAX_PAGE) {
        0 => MAX_PAGE,
        n => n,
    }
}

/// The refusal a list of packages makes: an empty list does not list `hostkeeper`.
fn list_refusal(why: ListError) -> Refusal {
    match why {
        ListError::Count(0) | ListError::HostkeeperNotListed => Refusal::HostkeeperNotListed,
        ListError::Count(_) => Refusal::TooManyPackages,
        ListError::Package(_) | ListError::Twice(_) => Refusal::InvalidPackage,
    }
}

/// An audit entry by `by` for `action` about the group `name`, with `reason`.
fn entry(by: &str, action: &str, name: &GroupName, reason: String) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.host_group = Some(name.to_string());
    entry.reason = Some(reason);
    entry
}

/// What a `Put` changed, as its audit entry says it, then `why`.
fn put_reason(old: Option<&HostGroup>, new: &HostGroup, why: &str) -> String {
    let before = old.map_or(&[][..], |group| group.packages.as_slice());
    let mut parts = Vec::new();
    if old.is_none() {
        parts.push("new group".to_owned());
    }
    let added = new.packages.iter().filter(|p| !before.contains(p));
    let taken = before.iter().filter(|p| !new.packages.contains(p));
    let moved: Vec<String> = added
        .map(|p| format!("+{p}"))
        .chain(taken.map(|p| format!("-{p}")))
        .collect();
    if !moved.is_empty() {
        parts.push(format!("packages {}", moved.join(" ")));
    }
    let stayed_before = before.iter().filter(|p| new.packages.contains(p));
    let stayed_after = new.packages.iter().filter(|p| before.contains(p));
    if !stayed_before.eq(stayed_after) {
        parts.push("reordered".to_owned());
    }
    let base = old.map(|group| group.policy.clone()).unwrap_or_default();
    let keys = new.policy.changed_keys(&base);
    if !keys.is_empty() {
        parts.push(format!("policy {}", keys.join(" ")));
    }
    if parts.is_empty() {
        parts.push("no change".to_owned());
    }
    format!("{}: {why}", parts.join("; "))
}

impl Coordinator {
    /// The group of every enrolled host: node id and membership, by node id; none without
    /// enrollment.
    fn memberships(&self) -> Result<Vec<(String, Membership)>, Refusal> {
        let Some(enrollment) = self.enrollment() else {
            return Ok(Vec::new());
        };
        enrollment
            .store()
            .memberships()
            .map(|all| all.into_iter().map(|(id, m)| (id.to_string(), m)).collect())
            .map_err(unreadable(Refusal::StoreFailed))
    }

    /// The enrolled hosts in the group `name` (those moving into it among them), each with the
    /// specific group it is in, if it is in one. Every enrolled host is in `all`.
    fn members_of(&self, name: &GroupName) -> Result<Vec<(HostId, Option<GroupName>)>, Refusal> {
        Ok(self
            .memberships()?
            .into_iter()
            .filter(|(_, membership)| name.is_all() || membership.host_group == name.as_str())
            .filter_map(|(id, membership)| {
                Some((id.parse().ok()?, membership.host_group.parse().ok()))
            })
            .collect())
    }

    /// The enrolled hosts in the group `name` (those moving into it among them), by node id.
    pub(crate) fn hosts_in(&self, name: &GroupName) -> Result<Vec<HostId>, Refusal> {
        Ok(self
            .members_of(name)?
            .into_iter()
            .map(|(host, _)| host)
            .collect())
    }

    /// `group` as `List` and `Get` say it: `hosts` hosts, and, with `detail`, each package's
    /// minimum version, from the ops state, and convergence, by package, which `Get` answers and
    /// `List` does not.
    fn state_of(
        name: &GroupName,
        group: &HostGroup,
        hosts: u32,
        detail: Option<(&OpsState, &BTreeMap<String, proto::Convergence>)>,
    ) -> proto::HostGroupState {
        proto::HostGroupState {
            group: Some(body_proto(name, group)),
            generation: group.generation,
            hosts,
            packages: group
                .packages
                .iter()
                .map(|package| proto::GroupPackage {
                    package: package.clone(),
                    desired_version: group
                        .desired
                        .get(package)
                        .map(|desired| desired.version.as_str().to_owned()),
                    min_version: detail.and_then(|(state, _)| {
                        let marks = state.group_marks(name, package);
                        marks.min_version.map(|version| version.as_str().to_owned())
                    }),
                    convergence: detail.and_then(|(_, converged)| converged.get(package).cloned()),
                })
                .collect(),
        }
    }

    /// `HostGroups.List`: the groups by name after the page token's, each with its packages,
    /// policy, generation, host count and each package's desired version. At most `limit`
    /// (capped at [`MAX_PAGE`]; 0 is the cap); the token of the next page is the last name of
    /// this one, and empty on the last page. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no group name),
    /// `store_failed` (the enrolled hosts cannot be read).
    pub fn host_groups_list(
        &self,
        caller: &Caller,
        request: &proto::ListHostGroupsRequest,
    ) -> Result<proto::ListHostGroupsResponse, Refusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(
                token
                    .parse::<GroupName>()
                    .map_err(|_| Refusal::InvalidRequest)?,
            ),
        };
        let limit = page_size(request.limit);
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for (_, membership) in self.memberships()? {
            *counts.entry(membership.host_group).or_default() += 1;
            *counts
                .entry(crate::host_groups::ALL.to_owned())
                .or_default() += 1;
        }
        let groups = self.desired.host_groups();
        let mut page: Vec<_> = groups
            .0
            .iter()
            .filter(|(name, _)| after.as_ref().is_none_or(|last| *name > last))
            .take(limit + 1)
            .collect();
        let more = page.len() > limit;
        page.truncate(limit);
        let next_page_token = match (more, page.last()) {
            (true, Some((name, _))) => name.to_string(),
            _ => String::new(),
        };
        Ok(proto::ListHostGroupsResponse {
            host_groups: page
                .into_iter()
                .map(|(name, group)| {
                    let hosts = counts.get(name.as_str()).copied().unwrap_or_default();
                    Self::state_of(name, group, hosts, None)
                })
                .collect(),
            next_page_token,
        })
    }

    /// `HostGroups.Get`: one group as `List` says it, with each package's minimum version and
    /// convergence, and a page of its hosts by node id, those moving into it among them: at most
    /// `limit` (capped at [`MAX_PAGE`]; 0 is the cap) after the page token's, which is the last
    /// node id of the page before. The convergence of a package is that of the package in the
    /// group, as `Releases.Converge` answers it for the group, without its write record. Changes
    /// nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the name, or a page token that is no node
    /// id), `unknown_host_group`, `store_failed` (the enrolled hosts cannot be read).
    pub fn host_groups_get(
        &self,
        caller: &Caller,
        request: &proto::GetHostGroupRequest,
    ) -> Result<proto::GetHostGroupResponse, Refusal> {
        caller.operator()?;
        let name: GroupName = request
            .host_group
            .parse()
            .map_err(|_| Refusal::InvalidRequest)?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(
                token
                    .parse::<HostId>()
                    .map_err(|_| Refusal::InvalidRequest)?,
            ),
        };
        let limit = page_size(request.limit);
        let group = self
            .desired
            .host_group(&name)
            .ok_or(Refusal::UnknownHostGroup)?;
        let hosts = self.hosts_in(&name)?;
        let mut page: Vec<&HostId> = hosts
            .iter()
            .filter(|id| after.as_ref().is_none_or(|last| *id > last))
            .take(limit + 1)
            .collect();
        let more = page.len() > limit;
        page.truncate(limit);
        let next_page_token = match (more, page.last()) {
            (true, Some(last)) => last.to_string(),
            _ => String::new(),
        };
        let count = u32::try_from(hosts.len()).unwrap_or(u32::MAX);
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        let ops = self.ops.state();
        let services = group_services(
            &name,
            &group,
            &self.desired.host_groups(),
            &self.services,
            &ops,
            &[],
        );
        let desired = self.desired.group_desireds(&name);
        let converged: BTreeMap<String, proto::Convergence> = services
            .defs()
            .map(|def| {
                let found = self.pair_convergence(def, desired.get(&def.name), &fleet, &ops, now);
                (def.name.clone(), report::convergence(&found))
            })
            .collect();
        drop(fleet);
        let state = Self::state_of(&name, &group, count, Some((&ops, &converged)));
        Ok(proto::GetHostGroupResponse {
            host_group: Some(state),
            node_ids: page.into_iter().map(ToString::to_string).collect(),
            next_page_token,
        })
    }

    /// `HostGroups.Put`: creates the group, or replaces the one of its name whole, as the module
    /// says. A replacement names the generation of the group it replaces (`if_generation`), and a
    /// new group names none. Audited as `host_group_put`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the reason, or a body that is absent),
    /// `invalid_host_group`, `hostkeeper_not_listed` (an empty list included), `too_many_packages`,
    /// `invalid_package` (a name that breaks its rule, or one listed twice), `invalid_policy` (a
    /// key out of its range; the log names the key), `generation_mismatch`, `store_failed`,
    /// `audit_unavailable`.
    pub fn host_groups_put(
        &self,
        caller: &Caller,
        request: proto::PutHostGroupRequest,
    ) -> Result<proto::PutHostGroupResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let body = request.host_group.ok_or(Refusal::InvalidRequest)?;
        let name: GroupName = body.name.parse().map_err(|_| Refusal::InvalidHostGroup)?;
        check_packages_for(&name, &body.packages).map_err(list_refusal)?;
        let policy = policy_of(body.policy.as_ref())
            .and_then(|policy| policy.check().map(|()| policy))
            .map_err(|refused| {
                tracing::warn!(key = refused.key, why = %refused.why, "a host group's policy is refused");
                Refusal::InvalidPolicy
            })?;
        let mut fleet = self.lock_fleet();
        let members = self.members_of(&name)?;
        let now = self.ops.unix_now();
        let (staged, (old, next, before, after)) = self.desired.stage_host_groups(|held| {
            let old = held.0.get(&name).cloned();
            if old.is_none() && name.is_all() {
                return Err(Refusal::ReservedHostGroup);
            }
            if old.as_ref().map(|group| group.generation) != request.if_generation {
                return Err(Refusal::GenerationMismatch);
            }
            let kept = |package: &String| body.packages.contains(package);
            let next = HostGroup {
                generation: old
                    .as_ref()
                    .map_or(1, |group| group.generation.saturating_add(1)),
                by: by.to_owned(),
                at_unix: now,
                desired: old
                    .iter()
                    .flat_map(|group| group.desired.iter())
                    .filter(|(package, _)| kept(package))
                    .map(|(package, desired)| (package.clone(), desired.clone()))
                    .collect(),
                packages: body.packages.clone(),
                policy,
            };
            let mut groups = held.clone();
            groups.0.insert(name.clone(), next.clone());
            Ok((groups.clone(), (old, next, held.clone(), groups)))
        })?;
        let placed: Vec<_> = members
            .iter()
            .map(|(host, own)| (host.clone(), own.clone(), own.clone()))
            .collect();
        let wanted = lost_by(&fleet, &before, &after, &placed);
        let mut buckets: BTreeMap<Option<GroupName>, Vec<String>> = BTreeMap::new();
        for (host, own) in &members {
            buckets
                .entry(own.clone())
                .or_default()
                .push(host.to_string());
        }
        let audit = entry(
            by,
            "host_group_put",
            &name,
            put_reason(old.as_ref(), &next, &why),
        );
        let done = self.ops.decide_changes_with(staged, |state| {
            state.drop_group_packages(&name, &next.packages);
            let mut entries = vec![audit];
            for (own, nodes) in &buckets {
                let runs = runs(&after, own.as_ref());
                entries.extend(cancel_uninstalls(state, &name, &runs, nodes));
            }
            order_uninstalls(state, &wanted);
            entries
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::PutHostGroupResponse {
            entry: done.first().map(entry_proto),
            generation: next.generation,
        })
    }

    /// `HostGroups.Remove`: removes the group, its desired versions and its marks. No enrolled
    /// host may be in it or moving into it (an approval puts its host in its group as it is
    /// written, so an approved request whose host has not yet polled counts), no inventory
    /// record may name it, by its group or its role ([`InventoryRecord::group`]), and no entry
    /// of the Mac allowlist. Checked under the fleet's lock, which `Enrollment.Approve`,
    /// `Enrollment.AddRecord` and an approval by a record hold while they check and write the
    /// group they name, so none lands in a group being removed. Audited as `host_group_remove`.
    ///
    /// [`InventoryRecord::group`]: crate::inventory::InventoryRecord::group
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the name or the reason),
    /// `unknown_host_group`, `not_empty`, `store_failed`, `audit_unavailable`.
    pub fn host_groups_remove(
        &self,
        caller: &Caller,
        request: proto::RemoveHostGroupRequest,
    ) -> Result<proto::RemoveHostGroupResponse, Refusal> {
        let by = caller.operator()?;
        let name: GroupName = request
            .host_group
            .parse()
            .map_err(|_| Refusal::InvalidRequest)?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        if name.is_all() {
            return Err(Refusal::ReservedHostGroup);
        }
        let mut fleet = self.lock_fleet();
        let occupied = self
            .memberships()?
            .iter()
            .any(|(_, m)| m.host_group == name.as_str());
        let named = self.ops.read(|state| {
            state
                .inventory
                .iter()
                .any(|record| record.group() == name.as_str())
        }) || self
            .enrollment()
            .is_some_and(|enrollment| enrollment.mac_names_group(name.as_str()));
        let (staged, ()) = self.desired.stage_host_groups(|held| {
            if !held.0.contains_key(&name) {
                return Err(Refusal::UnknownHostGroup);
            }
            if occupied || named {
                return Err(Refusal::NotEmpty);
            }
            let mut groups = held.clone();
            groups.0.remove(&name);
            Ok((groups, ()))
        })?;
        let audit = entry(by, "host_group_remove", &name, why);
        let done = self.ops.decide_changes_with(staged, |state| {
            state.drop_group(&name);
            vec![audit]
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::RemoveHostGroupResponse {
            entry: done.first().map(entry_proto),
        })
    }
}
