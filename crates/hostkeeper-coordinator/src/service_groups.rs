//! `Releases.SetDesired` for a coordinator that holds a host group (`docs/design/hostgroups.md`,
//! section 2.1): the desired version of a package is set in a group.
//!
//! A set that names a group deploys the package to that group alone: one deployment, one audit
//! entry `set_desired` naming the group. A set that names none is read as one deployment to each
//! group that lists the package, written in one change, with one audit entry for each. The
//! package must be listed by the group (`not_in_host_group`); `if_generation` with no group is
//! accepted only when exactly one group lists the package (`ambiguous_host_group`); the limits of
//! the desired state are a group's policy (`limits_in_host_group`). A set refused for any group
//! writes nothing for any: the refusals of a set of a service (the version's scheme, the minimum
//! version of the package and of the group, a version marked bad, a digest or a release set
//! before as another, a repository the registry lacks) are decided for every group before the
//! change is made.
//!
//! The change is one commit: each group's desired version and generation, the version's release,
//! the audit entries, and for each group the force of another version that ends and the halt the
//! updater raised that lifts, both audited by the setter. The answer is the desired state and the
//! write record of the group, or, when the set made more than one deployment, of the first group by
//! name, with no record.

use std::sync::Arc;

use hostkeeper_wire::version::Version;

use super::ci_calls::{Setter, release_of};
use super::{Coordinator, Set, SetError};
use crate::desired::{Desired, DesiredError};
use crate::force;
use crate::group_scope::higher;
use crate::halt;
use crate::host_groups::{GroupName, HostGroup, HostGroups};
use crate::ops::AuditEntry;
use crate::releases::{self, Release, Targets};
use crate::services::{ServiceDef, Services};
use crate::write_record::{WriteAsk, WriteRecord};

/// The groups a set of `package` that names `host_group` (none: empty) deploys to, in name order.
fn deployment_targets(
    groups: &HostGroups,
    package: &str,
    host_group: &str,
    ask: &WriteAsk,
) -> Result<Vec<GroupName>, SetError> {
    let lists = |listed: &HostGroup| listed.packages.iter().any(|listed| listed == package);
    if !host_group.is_empty() {
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
    let names: Vec<GroupName> = groups
        .0
        .iter()
        .filter(|(_, listed)| lists(listed))
        .map(|(name, _)| name.clone())
        .collect();
    match names.len() {
        0 => Err(SetError::NotInHostGroup),
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
    /// [`SetError::AmbiguousHostGroup`] and [`SetError::LimitsInHostGroup`]. The audit entries are
    /// written with the desired state: [`SetError::Ops`] of
    /// [`crate::ops::OpsError::AuditUnavailable`] means nothing is set.
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
                let release = Arc::new(release_of(body, &service, &desired, targets)?);
                desired.set_release(None);
                Some(release)
            }
        };
        if let Setter::Ci(verified) = setter {
            self.ci_admits(verified, &service)?;
        }
        self.check_channel(&service, &named, release.as_deref())
            .await?;
        let (actor, set_by) = match setter {
            Setter::Ci(verified) => (crate::reconcile::CI, verified.job_workflow_ref.clone()),
            Setter::Operator(name) => (*name, format!("operator token {name}")),
        };
        let reason = reason_of(setter, &desired, release.as_deref());
        // The fleet's lock is held to the end of the set, as for a set of a service: the groups
        // and the marks the refusals are decided on do not change under it.
        let _fleet = self.lock_fleet_for_set();
        let groups = deployment_targets(&self.desired.host_groups(), &service, host_group, ask)?;
        let state = self.ops.state();
        for group in &groups {
            let floor = state.group_marks(group, &service).min_version;
            let def = ServiceDef {
                min_version: higher(base.min_version.clone(), floor),
                ..base.clone()
            };
            self.refuse(&def, &desired)?;
        }
        self.refuse_release(
            &service,
            &desired.version,
            release.as_deref(),
            named.release().is_some(),
        )?;
        self.refuse_repository(&base, release.as_deref())?;
        // The versions the registry keeps at its cap whatever their age: those a group recorded
        // as good, and those marked bad.
        let kept: Vec<Version> = state
            .host_groups
            .0
            .values()
            .filter_map(|packages| packages.get(&service)?.last_good.clone())
            .chain(state.marks_in(None, &service).bad)
            .collect();
        let staged = self
            .desired
            .stage_group_set(&groups, &service, &desired, release, &kept, ask)?;
        self.ops.decide_changes_with(staged, |state| {
            let mut entries = Vec::new();
            for group in &groups {
                let mut entry = AuditEntry::new(actor, "set_desired");
                entry.service = Some(service.clone());
                entry.host_group = Some(group.to_string());
                entry.reason = Some(reason.clone());
                entries.push(entry);
                let ended = force::end(
                    state,
                    Some(group),
                    &service,
                    &desired.version,
                    actor,
                    &set_by,
                );
                entries.extend(ended);
                let lifted = halt::lift(
                    state,
                    Some(group),
                    &service,
                    &desired.version,
                    actor,
                    &set_by,
                );
                entries.extend(lifted);
            }
            entries
        })?;
        match setter {
            Setter::Ci(verified) => tracing::info!(
                workflow = %verified.job_workflow_ref,
                sha = ?verified.sha,
                %service,
                groups = groups.len(),
                "desired state set by CI"
            ),
            Setter::Operator(name) => tracing::info!(
                token = %name,
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
