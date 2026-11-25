//! A host group's body and policy between `hostkeeper.operator.v1` and the coordinator's stored
//! form ([`crate::host_groups`]). Reading a body takes the policy's keys as the request names
//! them and fills in each key it leaves out with its default; writing one names every key, so a
//! caller reads the policy that applies and not only the keys it set.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::defer::DeferBackoff;
use crate::failed_hosts::FailedHosts;
use crate::host_groups::{
    DEFAULT_MAX_UPDATING, GroupName, HostGroup, Policy, PolicyCanary, PolicyError, PolicyStage,
};
use crate::maintenance_policy::{LinuxScope, Maintenance, XcodeDefault};
use crate::reconcile::{FailThreshold, Rollback};
use crate::stages::Advance;

/// The policy `policy` says: each key it leaves out takes its default, and no key is checked
/// ([`Policy::check`] checks them).
///
/// # Errors
/// [`PolicyError`] for a stage's `advance`, the `rollback` mode or the `failed_hosts` mode that is
/// a number the proto does not define, and of `maintenance` for its `linux_scope` that is one, or an `xcode_default` that
/// is neither `latest` nor a version.
pub(crate) fn policy_of(policy: Option<&proto::RolloutPolicy>) -> Result<Policy, PolicyError> {
    let Some(policy) = policy else {
        return Ok(Policy::default());
    };
    let stages = policy
        .stages
        .iter()
        .map(|stage| {
            Ok(PolicyStage {
                percent: stage.percent,
                advance: advance_of(stage.advance)?,
            })
        })
        .collect::<Result<Vec<_>, PolicyError>>()?;
    let threshold = FailThreshold::default();
    let backoff = DeferBackoff::default();
    let given = policy.rollback_after_failed.unwrap_or_default();
    let spaced = policy.defer_backoff.unwrap_or_default();
    Ok(Policy {
        stages,
        canary: policy.canary.map(|canary| PolicyCanary {
            count: canary.count,
        }),
        bake_beats: policy.bake_beats,
        rollback: rollback_of(policy.rollback)?,
        rollback_after_failed: FailThreshold {
            percent: given.percent.unwrap_or(threshold.percent),
            min_hosts: given.min_hosts.unwrap_or(threshold.min_hosts),
        },
        failed_hosts: failed_hosts_of(policy.failed_hosts)?,
        defer_backoff: DeferBackoff {
            initial_secs: spaced.initial_secs.unwrap_or(backoff.initial_secs),
            factor: spaced.factor.unwrap_or(backoff.factor),
            max_secs: spaced.max_secs.unwrap_or(backoff.max_secs),
        },
        max_updating: policy.max_updating.unwrap_or(DEFAULT_MAX_UPDATING),
        min_up: policy.min_up,
        live_rollouts: policy.live_rollouts,
        maintenance: maintenance_of(policy.maintenance.as_ref())?,
    })
}

/// The maintenance table `table` says: each key it leaves out takes its default; no range is
/// checked ([`Maintenance::check`] checks them).
fn maintenance_of(table: Option<&proto::MaintenancePolicy>) -> Result<Maintenance, PolicyError> {
    let Some(table) = table else {
        return Ok(Maintenance::default());
    };
    let refused = |why: String| PolicyError {
        key: "maintenance",
        why,
    };
    let defaults = Maintenance::default();
    let linux_scope = match proto::LinuxScope::try_from(table.linux_scope) {
        Ok(proto::LinuxScope::Unspecified | proto::LinuxScope::Security) => LinuxScope::Security,
        Ok(proto::LinuxScope::All) => LinuxScope::All,
        Err(_) => {
            return Err(refused(format!(
                "linux_scope {} is not a Linux scope",
                table.linux_scope
            )));
        }
    };
    Ok(Maintenance {
        xcode_keep: table.xcode_keep,
        xcode_default: match table.xcode_default.as_deref() {
            None => defaults.xcode_default,
            Some(text) => text
                .parse::<XcodeDefault>()
                .map_err(|bad| refused(bad.to_string()))?,
        },
        macos_major: table.macos_major,
        linux_scope,
        batch: table.batch.unwrap_or(defaults.batch),
        min_serving: table.min_serving.unwrap_or(defaults.min_serving),
        drain_deadline_secs: table.drain_deadline_secs,
        return_secs: table.return_secs,
    })
}

/// `table` as the proto says it, every key named: `return_secs` of 0 stays 0, as its default is
/// the platform's.
fn maintenance_proto(table: &Maintenance) -> proto::MaintenancePolicy {
    proto::MaintenancePolicy {
        xcode_keep: table.xcode_keep,
        xcode_default: Some(table.xcode_default.to_string()),
        macos_major: table.macos_major,
        linux_scope: match table.linux_scope {
            LinuxScope::Security => proto::LinuxScope::Security,
            LinuxScope::All => proto::LinuxScope::All,
        }
        .into(),
        batch: Some(table.batch),
        min_serving: Some(table.min_serving),
        drain_deadline_secs: table.drain_deadline_secs,
        return_secs: table.return_secs,
    }
}

fn advance_of(number: i32) -> Result<Advance, PolicyError> {
    match proto::StageAdvance::try_from(number) {
        Ok(proto::StageAdvance::Unspecified | proto::StageAdvance::Auto) => Ok(Advance::Auto),
        Ok(proto::StageAdvance::Manual) => Ok(Advance::Manual),
        Err(_) => Err(PolicyError {
            key: "stages",
            why: format!("advance {number} is not a way to enter a stage"),
        }),
    }
}

fn rollback_of(number: i32) -> Result<Rollback, PolicyError> {
    match proto::RollbackMode::try_from(number) {
        Ok(proto::RollbackMode::Unspecified | proto::RollbackMode::Manual) => Ok(Rollback::Manual),
        Ok(proto::RollbackMode::Automatic) => Ok(Rollback::Automatic),
        Err(_) => Err(PolicyError {
            key: "rollback",
            why: format!("{number} is not a rollback mode"),
        }),
    }
}

fn failed_hosts_of(number: i32) -> Result<FailedHosts, PolicyError> {
    match proto::FailedHosts::try_from(number) {
        Ok(proto::FailedHosts::Unspecified | proto::FailedHosts::Continue) => {
            Ok(FailedHosts::Continue)
        }
        Ok(proto::FailedHosts::Stop) => Ok(FailedHosts::Stop),
        Err(_) => Err(PolicyError {
            key: "failed_hosts",
            why: format!("{number} is not a failed-hosts mode"),
        }),
    }
}

/// `policy` as the proto says it, every key named.
pub(crate) fn policy_proto(policy: &Policy) -> proto::RolloutPolicy {
    proto::RolloutPolicy {
        stages: policy
            .stages
            .iter()
            .map(|stage| proto::PolicyStage {
                percent: stage.percent,
                advance: match stage.advance {
                    Advance::Auto => proto::StageAdvance::Auto,
                    Advance::Manual => proto::StageAdvance::Manual,
                }
                .into(),
            })
            .collect(),
        canary: policy.canary.map(|canary| proto::Canary {
            count: canary.count,
        }),
        bake_beats: policy.bake_beats,
        rollback: match policy.rollback {
            Rollback::Manual => proto::RollbackMode::Manual,
            Rollback::Automatic => proto::RollbackMode::Automatic,
        }
        .into(),
        rollback_after_failed: Some(proto::FailThreshold {
            percent: Some(policy.rollback_after_failed.percent),
            min_hosts: Some(policy.rollback_after_failed.min_hosts),
        }),
        failed_hosts: match policy.failed_hosts {
            FailedHosts::Stop => proto::FailedHosts::Stop,
            FailedHosts::Continue => proto::FailedHosts::Continue,
        }
        .into(),
        defer_backoff: Some(proto::DeferBackoff {
            initial_secs: Some(policy.defer_backoff.initial_secs),
            factor: Some(policy.defer_backoff.factor),
            max_secs: Some(policy.defer_backoff.max_secs),
        }),
        max_updating: (policy.max_updating != DEFAULT_MAX_UPDATING).then_some(policy.max_updating),
        min_up: policy.min_up,
        live_rollouts: policy.live_rollouts,
        maintenance: Some(maintenance_proto(&policy.maintenance)),
    }
}

/// The group `name` as the proto says its body.
pub(crate) fn body_proto(name: &GroupName, group: &HostGroup) -> proto::HostGroup {
    proto::HostGroup {
        name: name.to_string(),
        packages: group.packages.clone(),
        policy: Some(policy_proto(&group.policy)),
    }
}
