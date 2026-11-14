//! `Releases.Force` once the coordinator holds a host group (`docs/design/hostgroups.md`, section
//! 4): a force sets a group's desired version of a package, so it is a deployment to that group,
//! with the force, its halt generation and its marks kept for the group.
//!
//! The request names a group in `host_group`, which must list the package (`not_in_host_group`),
//! or names none. Its targets then mean:
//!
//! - `fleet` is the whole group (still confirmed by naming the package twice); with no group
//!   named, each group that lists the package;
//! - `role` names a group, which must list the package, and must be the group named when one is
//!   (`unknown_target` otherwise); with no group named it is the one force of that group;
//! - `hosts` are each in a group the call reaches (`unknown_target` for a host the coordinator has
//!   not heard from, or one in another group, or in none); with no group named, one force for each
//!   distinct group of the hosts.
//!
//! A force that reaches more than one group is one change: every group's desired version, the
//! force's marks and one `force` audit entry for each group (naming it), with the entries
//! `unmark_bad`, `mark_bad` and `clear_halt` that group's force makes after its own. A refusal for
//! any group changes nothing for any: the minimum version of the group, and the hosts the force
//! names, are decided for every group before the change is made. The answer holds the entries of
//! every group, the held and skipped hosts of every group, and the sum of the hosts ordered.
//!
//! Forced past the plan of its group, the force stands in the group's marks until another version
//! is set in the group ([`super::end`]) or another force replaces it; the order its hosts are sent
//! is read from there ([`crate::orders`]).

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use super::{ForceTargets, Forcing, Plan, SELF_SERVICE, plan, recorded, targets_of};
use crate::desired::{Desired, RoleLimits};
use crate::fleet::Fleet;
use crate::group_reach::Reached;
use crate::group_scope::group_services;
use crate::holds::marks_of;
use crate::operator::{entry_proto, valid_reason};
use crate::service::Coordinator;
use crate::write_record::{WriteAsk, WriteError};

type Refusal = proto::ReleasesRefusal;

impl From<WriteError> for Refusal {
    fn from(why: WriteError) -> Self {
        match why {
            WriteError::Conflict { .. } => Self::GenerationConflict,
            WriteError::Unrecorded | WriteError::Store(_) => Self::StoreFailed,
        }
    }
}

/// The groups of `reached` that the force `targets` reaches, each with the targets it makes
/// there, in name order.
///
/// # Errors
/// `unknown_target`: a `role` that is no group of `reached`, or a host the fleet has not heard
/// from that is in none of them.
fn split<'a>(
    reached: &'a [Reached],
    fleet: &Fleet,
    targets: &ForceTargets,
) -> Result<Vec<(&'a Reached, ForceTargets)>, Refusal> {
    let group = |name: &str| reached.iter().find(|one| one.name.as_str() == name);
    match targets {
        ForceTargets::Fleet => Ok(reached
            .iter()
            .map(|one| (one, ForceTargets::Fleet))
            .collect()),
        ForceTargets::Role(role) => group(role)
            .map(|one| vec![(one, targets.clone())])
            .ok_or(Refusal::UnknownTarget),
        ForceTargets::Hosts(names) => {
            let mut hosts: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            for name in names {
                let found = name
                    .parse::<HostId>()
                    .ok()
                    .and_then(|id| fleet.node(&id))
                    .and_then(|node| group(&node.role))
                    .ok_or(Refusal::UnknownTarget)?;
                hosts
                    .entry(found.name.as_str())
                    .or_default()
                    .push(name.clone());
            }
            Ok(reached
                .iter()
                .filter_map(|one| {
                    let named = hosts.remove(one.name.as_str())?;
                    Some((one, ForceTargets::Hosts(named)))
                })
                .collect())
        }
    }
}

impl Coordinator {
    /// `Releases.Force` for a coordinator that holds a host group (the module says how).
    ///
    /// # Errors
    /// As [`Self::force`], and `not_in_host_group`, `invalid_request` for a `host_group` that is
    /// no group name, `unknown_target` as the module says.
    pub(super) fn force_in_groups(
        &self,
        by: &str,
        request: proto::ForceRequest,
    ) -> Result<proto::ForceResponse, Refusal> {
        if request.service == SELF_SERVICE {
            return Err(Refusal::SelfService);
        }
        let base = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?;
        let version = Version::parse(&request.version)
            .ok()
            .filter(|version| base.version_scheme.parses(version))
            .ok_or(Refusal::InvalidRequest)?;
        let reason = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let targets = targets_of(&base.name, request.targets)?;
        // A version set with targets is forced as its release, as for a service of the rollout
        // file ([`super::Coordinator::force`]).
        let set_with = self.desired.release(&base.name, version.as_str());
        let registered = self.desired.digests(&base.name).get(&version).cloned();
        let release = registered
            .clone()
            .or_else(|| set_with.as_deref().and_then(recorded))
            .ok_or(Refusal::NoDigest)?;
        let now = (self.clock)();
        // The fleet's lock is held to the end, as for a force of a service of the file.
        let mut fleet = self.lock_fleet();
        if let Some(set_with) = set_with {
            fleet.picks.keep_one(&base.name, &version, set_with);
        }
        let groups = self.desired.host_groups();
        let state = self.ops.state();
        let reached = self.reach(&groups, &state, &base.name, &request.host_group)?;
        let mut made: Vec<(&Reached, ForceTargets, Plan)> = Vec::new();
        for (one, targets) in split(&reached, &fleet, &targets)? {
            if one
                .def
                .min_version
                .as_ref()
                .is_some_and(|floor| !version.at_least(floor))
            {
                return Err(Refusal::BelowMinVersion);
            }
            let services = group_services(&one.name, &one.listed, &self.services, &state, &[]);
            let planned = plan(
                &one.def,
                &services,
                &version,
                &targets,
                request.include_held,
                &fleet,
                now,
            )?;
            made.push((one, targets, planned));
        }
        // The versions the registry keeps at its cap whatever their age: those a group recorded
        // as good, and those marked bad.
        let kept: Vec<Version> = state
            .host_groups
            .0
            .values()
            .filter_map(|packages| packages.get(&base.name)?.last_good.clone())
            .chain(state.marks_in(None, &base.name).bad)
            .collect();
        let mut wanted = Desired {
            version: version.clone(),
            max_updating: RoleLimits::default(),
            min_up: RoleLimits::default(),
            release_digest: None,
            release_index: None,
        };
        wanted.set_release(registered);
        let names: Vec<_> = made.iter().map(|(one, _, _)| one.name.clone()).collect();
        let staged = self.desired.stage_group_set(
            &names,
            &base.name,
            &wanted,
            None,
            &kept,
            &WriteAsk::default(),
        )?;
        let entries = self.ops.decide_changes_with(staged, |held| {
            let mut entries = Vec::new();
            for (one, targets, planned) in &made {
                let forcing = Forcing {
                    group: Some(&one.name),
                    service: &base.name,
                    version: &version,
                    release: &release,
                    by,
                    reason: &reason,
                    targets,
                    include_held: request.include_held,
                    plan: planned,
                };
                entries.extend(forcing.apply(held));
            }
            entries
        })?;
        fleet.set_marks(marks_of(&self.ops.state()));
        let ordered: usize = made
            .iter()
            .map(|(_, _, planned)| planned.ordered.len())
            .sum();
        tracing::warn!(
            service = %base.name,
            %version,
            by,
            groups = made.len(),
            ordered,
            "a force was ordered: it passes the update plan and the bake"
        );
        let mut response = proto::ForceResponse {
            entries: entries.iter().map(entry_proto).collect(),
            targeted: u32::try_from(ordered).unwrap_or(u32::MAX),
            ..proto::ForceResponse::default()
        };
        for (_, _, planned) in made {
            response.held.extend(planned.held);
            response
                .skipped
                .extend(
                    planned
                        .skipped
                        .into_iter()
                        .map(|(node_id, reason)| proto::SkippedHost {
                            node_id,
                            reason: reason.to_owned(),
                        }),
                );
        }
        Ok(response)
    }
}
