//! A host group's body and policy between the proto and the stored form
//! ([`crate::host_group_proto`]): a key a request leaves out takes its default, a key it names is
//! kept, an enum value the proto does not define is refused naming its key, and the answer names
//! every key so that it reads back as the policy that applies.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::defer::DeferBackoff;
use crate::host_group_proto::{body_proto, policy_of, policy_proto};
use crate::host_groups::{HostGroup, Policy, PolicyCanary, PolicyError, PolicyStage};
use crate::reconcile::{FailThreshold, Rollback};
use crate::stages::Advance;

/// A policy with every key set to something other than its default.
fn every_key() -> Policy {
    Policy {
        stages: vec![
            PolicyStage {
                percent: 10,
                advance: Advance::Manual,
            },
            PolicyStage {
                percent: 100,
                advance: Advance::Auto,
            },
        ],
        canary: Some(PolicyCanary { count: 2 }),
        bake_beats: 3,
        rollback: Rollback::Automatic,
        rollback_after_failed: FailThreshold {
            percent: 40,
            min_hosts: 5,
        },
        defer_backoff: DeferBackoff {
            initial_secs: 60,
            factor: 3,
            max_secs: 7200,
        },
        max_updating: 4,
        min_up: 6,
        live_rollouts: Some(7),
    }
}

/// Proves: no policy, and a policy that names nothing, read as the default policy; each key
/// left out of a policy that names others takes its default, and the keys it names are kept.
///
/// Catches: a key that reads as 0 when left out (`max_updating` 0 holds the group), or a
/// default that is not the one the design names.
#[test]
fn a_key_left_out_takes_its_default() {
    assert_eq!(policy_of(None), Ok(Policy::default()));
    assert_eq!(
        policy_of(Some(&proto::RolloutPolicy::default())),
        Ok(Policy::default())
    );
    let only_min_up = proto::RolloutPolicy {
        min_up: 2,
        rollback_after_failed: Some(proto::FailThreshold {
            percent: Some(50),
            min_hosts: None,
        }),
        defer_backoff: Some(proto::DeferBackoff {
            initial_secs: None,
            factor: Some(4),
            max_secs: None,
        }),
        ..proto::RolloutPolicy::default()
    };
    assert_eq!(
        policy_of(Some(&only_min_up)),
        Ok(Policy {
            min_up: 2,
            rollback_after_failed: FailThreshold {
                percent: 50,
                min_hosts: 2,
            },
            defer_backoff: DeferBackoff {
                initial_secs: 300,
                factor: 4,
                max_secs: 3600,
            },
            ..Policy::default()
        })
    );
}

/// Proves: a policy with every key set is answered with every key named, and the answer reads
/// back as the same policy; the default policy is answered with every key at its default, not
/// left out, and reads back as the default.
///
/// Catches: a key lost in either direction, or an answer that leaves a caller guessing a
/// default.
#[test]
fn a_policy_reads_back_as_it_was_put_with_every_key_named() {
    let said = policy_proto(&every_key());
    assert_eq!(
        said,
        proto::RolloutPolicy {
            stages: vec![
                proto::PolicyStage {
                    percent: 10,
                    advance: proto::StageAdvance::Manual.into(),
                },
                proto::PolicyStage {
                    percent: 100,
                    advance: proto::StageAdvance::Auto.into(),
                },
            ],
            canary: Some(proto::Canary { count: 2 }),
            bake_beats: 3,
            rollback: proto::RollbackMode::Automatic.into(),
            rollback_after_failed: Some(proto::FailThreshold {
                percent: Some(40),
                min_hosts: Some(5),
            }),
            defer_backoff: Some(proto::DeferBackoff {
                initial_secs: Some(60),
                factor: Some(3),
                max_secs: Some(7200),
            }),
            max_updating: Some(4),
            min_up: 6,
            live_rollouts: Some(7),
        }
    );
    assert_eq!(policy_of(Some(&said)), Ok(every_key()));

    let default = policy_proto(&Policy::default());
    assert_eq!(default.max_updating, Some(1));
    assert_eq!(default.rollback, i32::from(proto::RollbackMode::Manual));
    assert_eq!(
        default.rollback_after_failed,
        Some(proto::FailThreshold {
            percent: Some(25),
            min_hosts: Some(2),
        })
    );
    assert_eq!(policy_of(Some(&default)), Ok(Policy::default()));
}

/// Proves: an unspecified stage `advance` and an unspecified `rollback` read as the automatic
/// advance and the manual rollback; a number the proto does not define is refused for its key,
/// `stages` or `rollback`.
///
/// Catches: a number outside the enum read as a mode, which would make a typo an unattended
/// rollback.
#[test]
fn an_enum_value_the_proto_does_not_define_is_refused_for_its_key() {
    let stage = |advance: i32| proto::RolloutPolicy {
        stages: vec![proto::PolicyStage {
            percent: 100,
            advance,
        }],
        ..proto::RolloutPolicy::default()
    };
    let read = policy_of(Some(&stage(0))).expect("unspecified");
    assert_eq!(read.stages[0].advance, Advance::Auto);
    let unknown = policy_of(Some(&stage(9))).expect_err("refused");
    assert_eq!(
        unknown,
        PolicyError {
            key: "stages",
            why: "advance 9 is not a way to enter a stage".to_owned(),
        }
    );

    let rollback = |mode: i32| proto::RolloutPolicy {
        rollback: mode,
        ..proto::RolloutPolicy::default()
    };
    assert_eq!(
        policy_of(Some(&rollback(0))).expect("unspecified").rollback,
        Rollback::Manual
    );
    let unknown = policy_of(Some(&rollback(9))).expect_err("refused");
    assert_eq!(
        unknown,
        PolicyError {
            key: "rollback",
            why: "9 is not a rollback mode".to_owned(),
        }
    );
}

/// Proves: the body of a group is answered with its name, its packages in order and its policy.
#[test]
fn a_body_names_the_group_its_packages_in_order_and_its_policy() {
    let group = HostGroup {
        packages: vec!["svc".to_owned(), "hostkeeper".to_owned()],
        policy: every_key(),
        generation: 2,
        by: "alice".to_owned(),
        at_unix: 5,
        desired: std::collections::BTreeMap::new(),
    };
    let name = "web".parse().expect("a group name");
    assert_eq!(
        body_proto(&name, &group),
        proto::HostGroup {
            name: "web".to_owned(),
            packages: vec!["svc".to_owned(), "hostkeeper".to_owned()],
            policy: Some(policy_proto(&every_key())),
        }
    );
}
