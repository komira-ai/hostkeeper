//! A host group's body and policy between `hostkeeper.operator.v1` and the coordinator's stored
//! form ([`crate::host_groups`]). Reading a body takes the policy's keys as the request names
//! them and fills in each key it leaves out with its default; writing one names every key, so a
//! caller reads the policy that applies and not only the keys it set.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::defer::DeferBackoff;
use crate::host_groups::{
    DEFAULT_MAX_UPDATING, GroupName, HostGroup, Policy, PolicyCanary, PolicyError, PolicyStage,
};
use crate::reconcile::{FailThreshold, Rollback};
use crate::stages::Advance;

/// The policy `policy` says: each key it leaves out takes its default, and no key is checked
/// ([`Policy::check`] checks them).
///
/// # Errors
/// [`PolicyError`] for a stage's `advance` or the `rollback` mode that is a number the proto does
/// not define.
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
        defer_backoff: DeferBackoff {
            initial_secs: spaced.initial_secs.unwrap_or(backoff.initial_secs),
            factor: spaced.factor.unwrap_or(backoff.factor),
            max_secs: spaced.max_secs.unwrap_or(backoff.max_secs),
        },
        max_updating: policy.max_updating.unwrap_or(DEFAULT_MAX_UPDATING),
        min_up: policy.min_up,
        live_rollouts: policy.live_rollouts,
    })
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
        defer_backoff: Some(proto::DeferBackoff {
            initial_secs: Some(policy.defer_backoff.initial_secs),
            factor: Some(policy.defer_backoff.factor),
            max_secs: Some(policy.defer_backoff.max_secs),
        }),
        max_updating: Some(policy.max_updating),
        min_up: policy.min_up,
        live_rollouts: policy.live_rollouts,
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
