//! `Releases.SetDesired` for a coordinator that holds a host group (`docs/design/hostgroups.md`,
//! section 2.1): the desired version of a package is set in a group.
//!
//! A set that names a group deploys the package to that group alone: one deployment, one audit
//! entry `set_desired` naming the group. A set that names none is read as one deployment to each
//! group that lists the package, written in one change, with one audit entry for each. A CI token
//! whose allow-list entry names `host_groups` deploys to those groups alone: a set naming another
//! group is refused `forbidden` before the group is looked up, a set naming none deploys to each
//! of those groups that lists the package, and is `forbidden` when only other groups list it. The
//! package must be listed by the group (`not_in_host_group`); `if_generation` with no group is
//! accepted only when exactly one group (of the token's, for a limited token) lists the package
//! (`ambiguous_host_group`); the limits of the desired state are a group's policy
//! (`limits_in_host_group`). A set refused for any group writes nothing for any: the refusals of
//! a set of a service (the version's scheme, the minimum version of the package and of the group,
//! a version marked bad, a digest or a release set before as another, a repository the registry
//! lacks) are decided for every group before the change is made.
//!
//! The change is one commit: each group's desired version and generation, the version's release,
//! the audit entries, and for each group the force of another version that ends and the halt the
//! updater raised that lifts, both audited by the setter. The answer is the desired state and the
//! write record of the group, or, when the set made more than one deployment, of the first group by
//! name, with no record.
//!
//! The set is decided on the desired state in memory and committed with the operations state in
//! one transaction, which refuses it when the desired state it reads under its locks is not the
//! copy the set was decided on (a commit came between: another coordinator's, or this one's own
//! set just before it, whose queue number only the rows hold). Memory then holds the rows as read,
//! and the set is decided again on them, refusals and all: [`SET_ATTEMPTS`] times in all, and then
//! it is refused `state_changed` with nothing changed.

use std::sync::Arc;

use hostkeeper_wire::version::Version;

use super::ci_calls::{Setter, release_of};
use super::{Coordinator, Set, SetError};
use crate::desired::{Desired, DesiredError};
use crate::force;
use crate::group_scope::higher;
use crate::halt;
use crate::host_groups::{GroupName, HostGroup, HostGroups};
use crate::ops::{AuditEntry, OpsError};
use crate::releases::{self, Release, Targets};
use crate::services::{ServiceDef, Services};
use crate::write_record::{WriteAsk, WriteRecord};

/// The groups a set of `package` that names `host_group` (none: empty) deploys to, in name order.
/// `permitted` is the `host_groups` of the CI token's allow-list entry, none for an operator or an
/// entry that names none: the set may deploy to those groups alone.
fn deployment_targets(
    groups: &HostGroups,
    package: &str,
    host_group: &str,
    ask: &WriteAsk,
    permitted: Option<&[String]>,
) -> Result<Vec<GroupName>, SetError> {
    let lists = |listed: &HostGroup| listed.packages.iter().any(|listed| listed == package);
    let allowed = |name: &str| permitted.is_none_or(|names| names.iter().any(|n| n == name));
    if !host_group.is_empty() {
        if !allowed(host_group) {
            return Err(SetError::Forbidden);
        }
        let name: GroupName = host_group.parse().map_err(|_| {
            SetError::Invalid(DesiredError::Malformed(
                "host_group is not a host group name".to_owned(),
            ))
        })?;
        return match groups.0.get(&name) {
            Some(listed) if lists(listed) => Ok(vec![name]),
            _ => Err(SetError::NotInHostGroup),
        };
    }
    let listing: Vec<GroupName> = groups
        .0
        .iter()
        .filter(|(_, listed)| lists(listed))
        .map(|(name, _)| name.clone())
        .collect();
    let names: Vec<GroupName> = listing
        .iter()
        .filter(|name| allowed(name.as_str()))
        .cloned()
        .collect();
    match names.len() {
        0 if listing.is_empty() => Err(SetError::NotInHostGroup),
        0 => Err(SetError::Forbidden),
        1 => Ok(names),
        _ if ask.if_generation.is_some() => Err(SetError::AmbiguousHostGroup),
        _ => Ok(names),
    }
}

/// The reason of the audit entry of a set of `desired`: the version, with the release it was set
/// with, and for CI the workflow that set it.
fn reason_of(setter: &Setter<'_>, desired: &Desired, release: Option<&Release>) -> String {
    let workflow = match setter {
        Setter::Ci(verified) => Some(verified.job_workflow_ref.as_str()),
        Setter::Operator(_) => None,
    };
    if let Some(release) = release {
        return releases::audit_reason(&desired.version, release, workflow);
    }
    let text = match (&desired.release_digest, &desired.release_index) {
        (Some(digest), _) => format!("{} with package digest {digest}", desired.version),
        (None, Some(index)) => format!("{} with release index {}", desired.version, index.digest),
        (None, None) => desired.version.to_string(),
    };
    match workflow {
        Some(workflow) => format!("{text} set by {workflow}"),
        None => text,
    }
}

/// Whether the body's `max_updating` or `min_up` names a limit.
fn names_limits(body: &[u8]) -> bool {
    let fields: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    ["max_updating", "min_up"].iter().any(|key| {
        fields
            .get(key)
            .and_then(serde_json::Value::as_object)
            .is_some_and(|limits| !limits.is_empty())
    })
}

impl Coordinator {
    /// Sets the desired version of the package `body` names in the group `host_group`, or, with
    /// none, in each group that lists it (the module says how).
    ///
    /// # Errors
    /// [`SetError`], as [`Self::set_desired_with`], and [`SetError::NotInHostGroup`],
    /// [`SetError::AmbiguousHostGroup`] and [`SetError::LimitsInHostGroup`];
    /// [`SetError::Forbidden`] also when the token's entry names `host_groups` that exclude the
    /// group the set names, or, with none named, every group that lists the package. The audit
    /// entries are written with the desired state: every [`SetError::Ops`] means nothing is set;
    /// [`OpsError::DesiredChanged`] means the desired state changed in the store each of the
    /// [`SET_ATTEMPTS`] times the set was decided.
    pub(super) async fn set_desired_by_group(
        &self,
        setter: &Setter<'_>,
        body: &[u8],
        ask: &WriteAsk,
        targets: Option<Targets>,
        host_group: &str,
    ) -> Result<(Set, Option<WriteRecord>), SetError> {
        if names_limits(body) {
            return Err(SetError::LimitsInHostGroup);
        }
        // The package need not be defined by the rollout file: what a package has alone (its
        // version scheme, its repository, the floor of its minimum version) is the file's when it
        // defines the package, else the defaults.
        let service = package_of(body, self.legacy_service())?;
        let base = self
            .services
            .get(&service)
            .cloned()
            .unwrap_or_else(|| ServiceDef::new(&service, &[]));
        let known = Services::from_defs(vec![base.clone()]);
        let (_, mut desired, dropped_roles) =
            Desired::parse_request_noting(body, &known, self.legacy_service())?;
        let named = desired.clone();
        let release = match targets {
            None => None,
            Some(targets) => {
                let release = Arc::new(release_of(&service, &desired, targets)?);
                desired.set_release(None);
                Some(release)
            }
        };
        let permitted = match setter {
            Setter::Ci(verified) => self.ci_admits(verified, &service)?,
            Setter::Operator(_) => None,
        };
        let actor = setter;
        let set_by = match setter {
            Setter::Ci(verified) => verified.job_workflow_ref.clone(),
            Setter::Operator(operator) => format!("operator token {}", operator.name()),
        };
        let reason = reason_of(setter, &desired, release.as_deref());
        let set = GroupSet {
            service: &service,
            base: &base,
            desired: &desired,
            release: release.as_ref(),
            names_release: named.release().is_some(),
            host_group,
            ask,
            permitted: permitted.as_deref(),
            actor,
            set_by: &set_by,
            reason: &reason,
        };
        // The fleet's lock is held to the end of the set, as for a set of a service: the groups
        // and the marks the refusals are decided on do not change under it. A set staged on a
        // copy of the desired state older than the rows is decided again on the rows the edit
        // read, which memory took: what a commit just before it on this coordinator (whose queue
        // number memory does not hold) or on another one leaves behind.
        let _fleet = self.lock_fleet_for_set();
        let mut attempt = 1;
        let groups = loop {
            match self.deploy_group_set(&set) {
                Err(SetError::Ops(OpsError::DesiredChanged)) if attempt < SET_ATTEMPTS => {
                    tracing::info!(
                        %service,
                        attempt,
                        "a set in host groups was staged on a desired state older than the store's: it is decided again on the state as stored"
                    );
                    attempt += 1;
                }
                Err(SetError::Ops(why)) => {
                    tracing::error!(
                        %service,
                        %host_group,
                        attempt,
                        %why,
                        "a set in host groups was refused at its commit of the groups' desired versions, the release and the audit entries: nothing changed"
                    );
                    return Err(SetError::Ops(why));
                }
                Err(why) => return Err(why),
                Ok(groups) => break groups,
            }
        };
        match setter {
            Setter::Ci(verified) => tracing::info!(
                workflow = %verified.job_workflow_ref,
                sha = ?verified.sha,
                %service,
                groups = groups.len(),
                "desired state set by CI"
            ),
            Setter::Operator(operator) => tracing::info!(
                operator = operator.name(),
                token = operator.api_token(),
                %service,
                groups = groups.len(),
                "desired state set by an operator"
            ),
        }
        let first = groups.first();
        let record = first
            .filter(|_| groups.len() == 1)
            .and_then(|group| self.desired.group_record(group, &service));
        let shown = first
            .and_then(|group| self.desired.group_desired(group, &service))
            .unwrap_or(desired);
        Ok((
            Set {
                desired: shown,
                dropped_roles,
            },
            record,
        ))
    }
}

/// How many times a set in host groups is decided and staged before the store's answer that the
/// desired state changed after it was staged ([`OpsError::DesiredChanged`]) refuses it: once, and
/// again after each such answer. Every attempt after the first follows a commit that came between
/// the copy the set was decided on and its edit's locks; only the first can follow this
/// coordinator's own commit, since memory then holds the rows that edit read.
const SET_ATTEMPTS: u32 = 3;

/// What a set in host groups is decided from, read from its request before the fleet's lock.
struct GroupSet<'a> {
    service: &'a str,
    /// What the package has alone: the rollout file's definition, else the defaults.
    base: &'a ServiceDef,
    desired: &'a Desired,
    release: Option<&'a Arc<Release>>,
    /// Whether the body named a package digest or a release index.
    names_release: bool,
    host_group: &'a str,
    ask: &'a WriteAsk,
    permitted: Option<&'a [String]>,
    actor: &'a Setter<'a>,
    set_by: &'a str,
    reason: &'a str,
}

impl Coordinator {
    /// Decides `set` on the state in memory and commits it: the groups it deploys to, the
    /// refusals of the version in each of them, then the groups' desired versions, the release
    /// and the audit entries in one commit. Answers the groups, in name order. Called under the
    /// fleet's lock.
    ///
    /// # Errors
    /// [`SetError`]: a refusal of the set, decided before anything changes, or of its commit
    /// ([`SetError::Ops`]), nothing changed; [`OpsError::DesiredChanged`] means the set is to be
    /// decided again, on the desired state memory now holds.
    fn deploy_group_set(&self, set: &GroupSet<'_>) -> Result<Vec<GroupName>, SetError> {
        let (service, desired, actor) = (set.service, set.desired, set.actor);
        let groups = deployment_targets(
            &self.desired.host_groups(),
            service,
            set.host_group,
            set.ask,
            set.permitted,
        )?;
        let state = self.ops.state();
        for group in &groups {
            let floor = state.group_marks(group, service).min_version;
            let def = ServiceDef {
                min_version: higher(set.base.min_version.clone(), floor),
                ..set.base.clone()
            };
            self.refuse(&def, desired)?;
        }
        self.refuse_release(
            service,
            &desired.version,
            set.release.map(Arc::as_ref),
            set.names_release,
        )?;
        self.refuse_repository(set.base, set.release.map(Arc::as_ref))?;
        // The versions the registry keeps at its cap whatever their age: those a group recorded
        // as good, and those marked bad.
        let kept: Vec<Version> = state
            .host_groups
            .0
            .values()
            .filter_map(|packages| packages.get(service)?.last_good.clone())
            .chain(state.marks_in(None, service).bad)
            .collect();
        let staged = self.desired.stage_group_set(
            &groups,
            service,
            desired,
            set.release.cloned(),
            &kept,
            set.ask,
        )?;
        self.ops.decide_changes_with(staged, |state| {
            let mut entries = Vec::new();
            for group in &groups {
                let mut entry = AuditEntry::new(actor, "set_desired");
                entry.service = Some(service.to_owned());
                entry.host_group = Some(group.to_string());
                entry.reason = Some(set.reason.to_owned());
                entries.push(entry);
                let ended = force::end(
                    state,
                    Some(group),
                    service,
                    &desired.version,
                    actor,
                    set.set_by,
                );
                entries.extend(ended);
                let lifted = halt::lift(
                    state,
                    Some(group),
                    service,
                    &desired.version,
                    actor,
                    set.set_by,
                );
                entries.extend(lifted);
            }
            entries
        })?;
        Ok(groups)
    }
}

/// The package a set names: its `service`, else the legacy service.
fn package_of(body: &[u8], legacy: Option<&str>) -> Result<String, SetError> {
    let malformed = |why: &str| SetError::Invalid(DesiredError::Malformed(why.to_owned()));
    let fields: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| malformed(&e.to_string()))?;
    match fields.get("service") {
        None => legacy
            .map(str::to_owned)
            .ok_or_else(|| malformed("service is required")),
        Some(serde_json::Value::String(name)) => Ok(name.clone()),
        Some(_) => Err(malformed("service is not text")),
    }
}
