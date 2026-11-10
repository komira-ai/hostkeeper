//! The keys of a group's rollout policy at their bounds, through `HostGroups.Put`
//! ([`crate::operator_host_groups`]): each key's last good value is taken and the first value
//! past it is `invalid_policy`, and a refusal changes nothing. A policy is read back whole.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups_support::{SELF, World};
use crate::marks_support::operator;

/// A change of a policy, for a table of cases.
type Change = fn(&mut proto::RolloutPolicy);

fn stages(percents: &[u32]) -> Vec<proto::PolicyStage> {
    percents
        .iter()
        .map(|percent| proto::PolicyStage {
            percent: *percent,
            advance: proto::StageAdvance::Auto.into(),
        })
        .collect()
}

/// Proves: every key of the policy is held to its range: a stage list of 1 to 16 percents from 1
/// to 100 that rise to 100, a canary of 1 to 10000 hosts, 0 to 100 bake beats, a failure
/// threshold of 1 to 100 percent and 1 to 10000 hosts, a deferral backoff of an initial wait of at
/// least 1, a factor of 1 to 16 and a cap from the initial wait to 604800 seconds, 0 to 10000 for
/// `max_updating` and for `min_up`, 1 to 16 live rollouts; the last good value of each is taken and
/// the first past it is `invalid_policy`, and an enum value the proto does not define is too.
///
/// Catches: a bound that is off by one in either direction, or a key that is not checked at all.
#[test]
fn every_key_of_the_policy_is_held_to_its_range() {
    let cases: Vec<(&str, Change, bool)> = vec![
        (
            "16 stages",
            |p| p.stages = stages(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 100]),
            true,
        ),
        ("one stage of 100", |p| p.stages = stages(&[100]), true),
        (
            "17 stages",
            |p| p.stages = stages(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 100]),
            false,
        ),
        ("stage of 0", |p| p.stages = stages(&[0, 100]), false),
        ("stage of 101", |p| p.stages = stages(&[101]), false),
        (
            "stages that do not rise",
            |p| p.stages = stages(&[50, 50, 100]),
            false,
        ),
        (
            "stages that do not reach 100",
            |p| p.stages = stages(&[50]),
            false,
        ),
        (
            "canary of 1",
            |p| p.canary = Some(proto::Canary { count: 1 }),
            true,
        ),
        (
            "canary of 10000",
            |p| p.canary = Some(proto::Canary { count: 10_000 }),
            true,
        ),
        (
            "canary of 0",
            |p| p.canary = Some(proto::Canary { count: 0 }),
            false,
        ),
        (
            "canary of 10001",
            |p| p.canary = Some(proto::Canary { count: 10_001 }),
            false,
        ),
        ("100 bake beats", |p| p.bake_beats = 100, true),
        ("101 bake beats", |p| p.bake_beats = 101, false),
        (
            "threshold of 100 percent",
            |p| {
                p.rollback_after_failed = Some(proto::FailThreshold {
                    percent: Some(100),
                    min_hosts: Some(10_000),
                })
            },
            true,
        ),
        (
            "threshold of 0 percent",
            |p| {
                p.rollback_after_failed = Some(proto::FailThreshold {
                    percent: Some(0),
                    min_hosts: None,
                })
            },
            false,
        ),
        (
            "threshold of 101 percent",
            |p| {
                p.rollback_after_failed = Some(proto::FailThreshold {
                    percent: Some(101),
                    min_hosts: None,
                })
            },
            false,
        ),
        (
            "threshold of 0 hosts",
            |p| {
                p.rollback_after_failed = Some(proto::FailThreshold {
                    percent: None,
                    min_hosts: Some(0),
                })
            },
            false,
        ),
        (
            "threshold of 10001 hosts",
            |p| {
                p.rollback_after_failed = Some(proto::FailThreshold {
                    percent: None,
                    min_hosts: Some(10_001),
                })
            },
            false,
        ),
        (
            "backoff to the week",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: Some(1),
                    factor: Some(16),
                    max_secs: Some(604_800),
                })
            },
            true,
        ),
        (
            "backoff with no wait",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: Some(0),
                    factor: None,
                    max_secs: None,
                })
            },
            false,
        ),
        (
            "backoff factor 0",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: None,
                    factor: Some(0),
                    max_secs: None,
                })
            },
            false,
        ),
        (
            "backoff factor 17",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: None,
                    factor: Some(17),
                    max_secs: None,
                })
            },
            false,
        ),
        (
            "backoff cap under the wait",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: Some(400),
                    factor: None,
                    max_secs: Some(399),
                })
            },
            false,
        ),
        (
            "backoff cap past a week",
            |p| {
                p.defer_backoff = Some(proto::DeferBackoff {
                    initial_secs: None,
                    factor: None,
                    max_secs: Some(604_801),
                })
            },
            false,
        ),
        (
            "max_updating 10000",
            |p| p.max_updating = Some(10_000),
            true,
        ),
        ("max_updating 0", |p| p.max_updating = Some(0), true),
        (
            "max_updating 10001",
            |p| p.max_updating = Some(10_001),
            false,
        ),
        ("min_up 10000", |p| p.min_up = 10_000, true),
        ("min_up 10001", |p| p.min_up = 10_001, false),
        ("1 live rollout", |p| p.live_rollouts = Some(1), true),
        ("16 live rollouts", |p| p.live_rollouts = Some(16), true),
        ("0 live rollouts", |p| p.live_rollouts = Some(0), false),
        ("17 live rollouts", |p| p.live_rollouts = Some(17), false),
        (
            "a stage advance the proto lacks",
            |p| {
                p.stages = vec![proto::PolicyStage {
                    percent: 100,
                    advance: 9,
                }]
            },
            false,
        ),
        ("a rollback mode the proto lacks", |p| p.rollback = 9, false),
        (
            "automatic rollback",
            |p| p.rollback = proto::RollbackMode::Automatic.into(),
            true,
        ),
    ];
    let world = World::new("hg-put-policy-bounds");
    let before = world.state();
    for (label, change, accepted) in cases {
        let mut policy = proto::RolloutPolicy::default();
        change(&mut policy);
        let put = world.put_with("alpha", &[SELF], Some(policy), None);
        if accepted {
            assert!(put.is_ok(), "{label}: {put:?}");
            world
                .coordinator()
                .host_groups_remove(&operator(), crate::host_groups_support::remove_of("alpha"))
                .expect("removed");
        } else {
            assert_eq!(put.expect_err(label), Refusal::InvalidPolicy, "{label}");
        }
    }
    assert_eq!(world.state().0, before.0);
}

/// Proves: a policy put reads back whole: `Get` answers every key as it was put, and a group put
/// with no policy answers every key at its default.
///
/// Catches: a key that is stored and not answered, or a default the answer does not name.
#[test]
fn a_policy_is_read_back_as_it_was_put() {
    let world = World::new("hg-put-policy-readback");
    let policy = proto::RolloutPolicy {
        stages: vec![
            proto::PolicyStage {
                percent: 20,
                advance: proto::StageAdvance::Manual.into(),
            },
            proto::PolicyStage {
                percent: 100,
                advance: proto::StageAdvance::Auto.into(),
            },
        ],
        canary: Some(proto::Canary { count: 2 }),
        bake_beats: 4,
        rollback: proto::RollbackMode::Automatic.into(),
        rollback_after_failed: Some(proto::FailThreshold {
            percent: Some(30),
            min_hosts: Some(3),
        }),
        defer_backoff: Some(proto::DeferBackoff {
            initial_secs: Some(10),
            factor: Some(5),
            max_secs: Some(5000),
        }),
        max_updating: Some(0),
        min_up: 2,
        live_rollouts: Some(4),
    };
    world
        .put_with("alpha", &[SELF], Some(policy.clone()), None)
        .expect("put");
    world.put("beta", &[SELF], None).expect("put");
    let get = |group: &str| {
        world
            .coordinator()
            .host_groups_get(
                &operator(),
                &proto::GetHostGroupRequest {
                    host_group: group.to_owned(),
                    ..proto::GetHostGroupRequest::default()
                },
            )
            .expect("got")
            .host_group
            .expect("the group")
            .group
            .expect("the body")
            .policy
            .expect("the policy")
    };
    assert_eq!(get("alpha"), policy);
    let default = get("beta");
    assert_eq!(default.max_updating, Some(1));
    assert_eq!(default.stages, Vec::new());
    assert_eq!(default.live_rollouts, None);
}
