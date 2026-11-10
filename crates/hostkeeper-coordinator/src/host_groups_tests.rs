use std::collections::BTreeMap;

use hostkeeper_wire::version::Version;
use serde_json::json;

use super::*;
use crate::ops::HaltOrigin;
use crate::rollouts::Pause;
use crate::stages::{Share, StageDef};

fn v(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

/// An edit of a policy, for a table of cases.
type PolicyEdit<'a> = &'a dyn Fn(&mut Policy);

/// An edit of a group, for a table of cases.
type GroupEdit<'a> = &'a dyn Fn(&mut HostGroup);

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

/// A group of `hostkeeper` and `tierstore` at generation 3, with the default policy.
pub(crate) fn web() -> HostGroup {
    HostGroup {
        packages: vec!["tierstore".to_owned(), "hostkeeper".to_owned()],
        policy: Policy::default(),
        generation: 3,
        by: "alice".to_owned(),
        at_unix: 1_800_000_000,
        desired: BTreeMap::from([(
            "tierstore".to_owned(),
            GroupDesired {
                version: v("2.4.0"),
                generation: 2,
                write_id: Some("ci-run-7".to_owned()),
                replaced_version: Some(v("2.3.0")),
            },
        )]),
    }
}

/// Proves: a group name is a role name, exactly: 1 to 32 bytes of `a-z`, `0-9` and `-`, a letter
/// first, no `--`, not ending in `-`; text that breaks any one rule is refused, by `FromStr` and
/// by serde alike, with the text that says the rule; a name reads back as it was written.
///
/// Catches: a group the migration names after a role being refused, or a name the database's
/// check refuses being taken.
#[test]
fn a_group_name_is_the_grammar_of_a_role_name() {
    let longest = format!("a{}", "b".repeat(31));
    for good in ["web", "web-canary", "a", "a1-b2", longest.as_str()] {
        let parsed = name(good);
        assert_eq!(
            (parsed.as_str(), parsed.to_string()),
            (good, good.to_owned())
        );
        assert_eq!(String::from(parsed), good);
    }
    let too_long = format!("a{}", "b".repeat(32));
    for bad in [
        "",
        "Web",
        "1web",
        "-web",
        "web-",
        "we--b",
        "web_1",
        "wéb",
        too_long.as_str(),
    ] {
        assert_eq!(bad.parse::<GroupName>(), Err(BadGroupName), "{bad:?}");
        let read = serde_json::from_value::<GroupName>(json!(bad)).expect_err(bad);
        assert_eq!(
            read.to_string(),
            "not a host group name: 1 to 32 bytes of a-z, 0-9 and -, starting with a letter"
        );
    }
    assert_eq!(
        serde_json::to_value(name("web")).expect("serialises"),
        json!("web")
    );
}

/// Proves: a package name is 1 to 255 bytes of `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`, and
/// nothing else.
///
/// Catches: a package with a space, a slash or a byte past 255 stored in a group's list.
#[test]
fn a_package_name_is_one_to_255_bytes_of_its_alphabet() {
    let longest = "p".repeat(MAX_PACKAGE_BYTES);
    for good in [
        "hostkeeper",
        "tierstore-config-web",
        "A.b_c-9",
        longest.as_str(),
    ] {
        assert!(is_package_name(good), "{good:?}");
    }
    let too_long = "p".repeat(MAX_PACKAGE_BYTES + 1);
    for bad in ["", "a b", "a/b", "a+b", "é", too_long.as_str()] {
        assert!(!is_package_name(bad), "{bad:?}");
    }
}

/// Proves: the default policy is the default of every key of section 3 (no stage, which is 1, 10,
/// 50 and 100 percent; no canary; 0 bake beats; manual rollback; 25 percent and 2 hosts; 300, 2
/// and 3600 seconds; `max_updating` 1; `min_up` 0; the coordinator's `live_rollouts`), it is
/// written `{}`, and `{}` reads as it.
///
/// Catches: a default written into every group's file, or a key read with another default than
/// the one the rollout file had.
#[test]
fn the_default_policy_is_written_as_nothing_and_read_back_from_nothing() {
    let policy = Policy::default();
    assert_eq!(
        (
            policy.stages.len(),
            policy.canary,
            policy.bake_beats,
            policy.rollback,
            policy.rollback_after_failed,
            policy.defer_backoff,
            policy.max_updating,
            policy.min_up,
            policy.live_rollouts,
        ),
        (
            0,
            None,
            0,
            Rollback::Manual,
            FailThreshold {
                percent: 25,
                min_hosts: 2
            },
            DeferBackoff {
                initial_secs: 300,
                factor: 2,
                max_secs: 3600
            },
            1,
            0,
            None,
        )
    );
    assert_eq!(
        serde_json::to_value(&policy).expect("serialises"),
        json!({})
    );
    assert_eq!(
        serde_json::from_value::<Policy>(json!({})).expect("reads"),
        policy
    );
    assert_eq!(policy.check(), Ok(()));
    assert_eq!(policy.plan().expect("a plan"), Plan::default());
}

/// A policy with every key set away from its default.
fn every_key() -> Policy {
    Policy {
        stages: vec![
            PolicyStage {
                percent: 20,
                advance: Advance::Manual,
            },
            PolicyStage {
                percent: 100,
                advance: Advance::Auto,
            },
        ],
        canary: Some(PolicyCanary { count: 2 }),
        bake_beats: 4,
        rollback: Rollback::Automatic,
        rollback_after_failed: FailThreshold {
            percent: 10,
            min_hosts: 3,
        },
        defer_backoff: DeferBackoff {
            initial_secs: 60,
            factor: 3,
            max_secs: 7200,
        },
        max_updating: 0,
        min_up: 5,
        live_rollouts: Some(2),
    }
}

/// Proves: a policy with every key set is written with each key under its name, as the rollout
/// file spelled it (an automatic stage's `advance` left out), is read back equal, and makes the
/// plan of a canary of 2 hosts and then its stages, the first entered by an operator.
///
/// Catches: a key that does not survive the file or the database's `jsonb`, or a canary that is
/// not the plan's first stage.
#[test]
fn a_policy_with_every_key_is_written_by_name_and_read_back() {
    let policy = every_key();
    let written = serde_json::to_value(&policy).expect("serialises");
    assert_eq!(
        written,
        json!({
            "stages": [{"percent": 20, "advance": "manual"}, {"percent": 100}],
            "canary": {"count": 2},
            "bake_beats": 4,
            "rollback": "automatic",
            "rollback_after_failed": {"percent": 10, "min_hosts": 3},
            "defer_backoff": {"initial_secs": 60, "factor": 3, "max_secs": 7200},
            "max_updating": 0,
            "min_up": 5,
            "live_rollouts": 2
        })
    );
    assert_eq!(
        serde_json::from_value::<Policy>(written).expect("reads"),
        policy
    );
    assert_eq!(policy.check(), Ok(()));
    assert_eq!(
        policy.plan().expect("a plan").defs(),
        [
            StageDef {
                share: Share::Hosts(2),
                advance: Advance::Auto
            },
            StageDef {
                share: Share::Percent(20),
                advance: Advance::Manual
            },
            StageDef {
                share: Share::Percent(100),
                advance: Advance::Auto
            },
        ]
    );
}

/// Proves: each key's range of section 3 is checked at its bounds: the last value inside it is
/// taken and the first outside is refused, naming the key with the exact reason; the first key
/// out of range, in the policy's order, is the one named.
///
/// Catches: an off-by-one at a bound (a canary of 0, 101 bake beats, a `max_updating` over
/// 10000, 17 live rollouts) stored and acted on.
#[test]
fn every_policy_key_is_checked_at_its_bounds() {
    let stage = |percent| PolicyStage {
        percent,
        advance: Advance::Auto,
    };
    let with = |edit: &dyn Fn(&mut Policy)| {
        let mut policy = Policy::default();
        edit(&mut policy);
        policy.check().map_err(|e| e.to_string())
    };
    let taken: [PolicyEdit<'_>; 9] = [
        &|p| p.stages = (0..16).map(|i| stage(100 - 15 + i)).collect(),
        &|p| p.canary = Some(PolicyCanary { count: 10_000 }),
        &|p| p.bake_beats = 100,
        &|p| p.rollback_after_failed.percent = 100,
        &|p| p.rollback_after_failed.min_hosts = 10_000,
        &|p| p.max_updating = 10_000,
        &|p| p.min_up = 10_000,
        &|p| p.live_rollouts = Some(1),
        &|p| p.live_rollouts = Some(16),
    ];
    for edit in taken {
        assert_eq!(with(edit), Ok(()));
    }
    let refused: [(PolicyEdit<'_>, &str); 15] = [
        (
            &|p| p.stages = (0..17).map(|i| stage(100 - 16 + i)).collect(),
            "stages: stages lists 1 to 16 stages",
        ),
        (
            &|p| p.stages = vec![stage(0), stage(100)],
            "stages: stages are percents from 1 to 100 that rise",
        ),
        (
            &|p| p.stages = vec![stage(50), stage(50), stage(100)],
            "stages: stages are percents from 1 to 100 that rise",
        ),
        (
            &|p| p.stages = vec![stage(101)],
            "stages: stages are percents from 1 to 100 that rise",
        ),
        (
            &|p| p.stages = vec![stage(50)],
            "stages: the last stage is 100",
        ),
        (
            &|p| p.canary = Some(PolicyCanary { count: 0 }),
            "canary: the count is not 1 to 10000",
        ),
        (
            &|p| p.canary = Some(PolicyCanary { count: 10_001 }),
            "canary: the count is not 1 to 10000",
        ),
        (&|p| p.bake_beats = 101, "bake_beats: is over 100"),
        (
            &|p| p.rollback_after_failed.percent = 0,
            "rollback_after_failed: percent is not 1 to 100",
        ),
        (
            &|p| p.rollback_after_failed.min_hosts = 10_001,
            "rollback_after_failed: min_hosts is not 1 to 10000",
        ),
        (
            &|p| p.defer_backoff.initial_secs = 0,
            "defer_backoff: initial_secs is 0",
        ),
        (&|p| p.max_updating = 10_001, "max_updating: is over 10000"),
        (&|p| p.min_up = 10_001, "min_up: is over 10000"),
        (
            &|p| p.live_rollouts = Some(0),
            "live_rollouts: is not 1 to 16",
        ),
        (
            &|p| p.live_rollouts = Some(17),
            "live_rollouts: is not 1 to 16",
        ),
    ];
    for (edit, why) in refused {
        assert_eq!(with(edit), Err(why.to_owned()));
    }
    let first = with(&|p| {
        p.bake_beats = 101;
        p.min_up = 10_001;
    });
    assert_eq!(first, Err("bake_beats: is over 100".to_owned()));
    let error = Policy {
        min_up: 10_001,
        ..Policy::default()
    }
    .check()
    .expect_err("refused");
    assert_eq!(error.key, "min_up");
}

/// Proves: a policy key this build does not know is refused, as is a stage's.
///
/// Catches: a key a newer release added read as absent, its group rolled by a default.
#[test]
fn a_policy_key_this_build_does_not_know_is_refused() {
    for policy in [
        json!({"selector": "web"}),
        json!({"stages": [{"percent": 100, "wait": 3}]}),
        json!({"canary": {"count": 1, "roles": ["web"]}}),
    ] {
        let error = serde_json::from_value::<Policy>(policy).expect_err("refused");
        assert!(error.to_string().starts_with("unknown field"), "{error}");
    }
}

/// Proves: a group that keeps every rule of section 1.1 is taken, at the bounds of its list too
/// (one package, `hostkeeper`; 64 packages); each rule broken is refused with its own reason:
/// no package, 65 packages, a bad package name, a package twice, no `hostkeeper`, a policy key
/// out of range, generation 0, nobody who changed it, and a desired version of a bad package
/// name, of generation 0 or with a bad write id.
///
/// Catches: a group stored that a host could not be placed by (no `hostkeeper`, a package twice),
/// or a group a start refuses for a rule it does not have.
#[test]
fn a_group_is_checked_against_every_rule_of_its_list_and_body() {
    assert_eq!(web().check(), Ok(()));
    let alone = HostGroup {
        packages: vec!["hostkeeper".to_owned()],
        desired: BTreeMap::new(),
        ..web()
    };
    assert_eq!(alone.check(), Ok(()));
    let mut full: Vec<String> = (1..MAX_PACKAGES).map(|i| format!("p{i}")).collect();
    full.push("hostkeeper".to_owned());
    let most = HostGroup {
        packages: full.clone(),
        ..web()
    };
    assert_eq!(most.check(), Ok(()));
    full.push("p64".to_owned());
    let with = |edit: &dyn Fn(&mut HostGroup)| {
        let mut group = web();
        edit(&mut group);
        group.check().map_err(|e| e.to_string())
    };
    let desired = |edit: fn(&mut GroupDesired)| {
        move |g: &mut HostGroup| {
            if let Some(one) = g.desired.get_mut("tierstore") {
                edit(one);
            }
        }
    };
    let refused: [(GroupEdit<'_>, &str); 10] = [
        (&|g| g.packages.clear(), "lists 0 packages, not 1 to 64"),
        (
            &|g| g.packages = full.clone(),
            "lists 65 packages, not 1 to 64",
        ),
        (
            &|g| g.packages.push("a b".to_owned()),
            "\"a b\" is not a package name",
        ),
        (
            &|g| g.packages.push("tierstore".to_owned()),
            "lists tierstore twice",
        ),
        (
            &|g| g.packages.retain(|p| p != "hostkeeper"),
            "does not list hostkeeper",
        ),
        (
            &|g| g.policy.bake_beats = 101,
            "policy bake_beats: is over 100",
        ),
        (&|g| g.generation = 0, "the group has generation 0"),
        (&|g| g.by.clear(), "names nobody who changed it"),
        (
            &desired(|d| d.generation = 0),
            "a desired version has generation 0",
        ),
        (
            &desired(|d| d.write_id = Some("has space".to_owned())),
            "the desired version of \"tierstore\": a bad write id",
        ),
    ];
    for (edit, why) in refused {
        assert_eq!(with(edit), Err(why.to_owned()));
    }
    let mut bad_key = web();
    let one = bad_key.desired.remove("tierstore").expect("held");
    bad_key.desired.insert("a/b".to_owned(), one);
    assert_eq!(
        bad_key.check().map_err(|e| e.to_string()),
        Err("the desired version of \"a/b\": not a package name".to_owned())
    );
}

/// Proves: the groups are written as a map of name to body, each body's keys by name, its default
/// policy and a desired version's absent write id and replaced version left out, and read back
/// equal by [`HostGroups::read`], which checks each group and names the one it refuses.
///
/// Catches: a group that does not survive the state file, or a refusal that does not say which
/// group to look at.
#[test]
fn the_groups_are_written_by_name_and_read_back_checked() {
    let mut canary = web();
    canary.desired = BTreeMap::from([(
        "hostkeeper".to_owned(),
        GroupDesired {
            version: v("1.0.0"),
            generation: 1,
            write_id: None,
            replaced_version: None,
        },
    )]);
    canary.policy = every_key();
    let groups = HostGroups(BTreeMap::from([
        (name("web"), web()),
        (name("canary"), canary),
    ]));
    let written = serde_json::to_value(&groups).expect("serialises");
    assert_eq!(
        written["web"],
        json!({
            "packages": ["tierstore", "hostkeeper"],
            "generation": 3,
            "by": "alice",
            "at_unix": 1_800_000_000_u64,
            "desired": {"tierstore": {
                "version": "2.4.0",
                "generation": 2,
                "write_id": "ci-run-7",
                "replaced_version": "2.3.0"
            }}
        })
    );
    assert_eq!(
        written["canary"]["desired"],
        json!({"hostkeeper": {"version": "1.0.0", "generation": 1}})
    );
    assert_eq!(HostGroups::read(written).expect("reads"), groups);
    assert!(!groups.is_empty());
    assert!(HostGroups::default().is_empty());

    let mut broken = serde_json::to_value(&groups).expect("serialises");
    broken["web"]["packages"] = json!(["tierstore"]);
    assert_eq!(
        HostGroups::read(broken),
        Err("host group web: does not list hostkeeper".to_owned())
    );
    let unknown = json!({"web": {"packages": ["hostkeeper"], "generation": 1, "by": "a",
        "at_unix": 0, "members": []}});
    let error = HostGroups::read(unknown).expect_err("refused");
    assert!(
        error.starts_with("host_groups: unknown field `members`"),
        "{error}"
    );
    let misnamed = json!({"Web": {"packages": ["hostkeeper"], "generation": 1, "by": "a",
        "at_unix": 0}});
    let error = HostGroups::read(misnamed).expect_err("refused");
    assert!(
        error.starts_with("host_groups: not a host group name"),
        "{error}"
    );
}

/// The marks of a package in a group, every field set.
pub(crate) fn every_mark() -> GroupMarks {
    let ack = HaltAck {
        node_id: "web-02".to_owned(),
        target: v("2.4.0"),
        report: None,
    };
    GroupMarks {
        min_version: Some(v("2.0.0")),
        last_good: Some(v("2.3.0")),
        halt: Some(Halt {
            node_id: "web-01".to_owned(),
            check: "health".to_owned(),
            target: v("2.4.0"),
            report: None,
            origin: HaltOrigin::Operator,
        }),
        halt_acked: vec![ack.clone()],
        halt_seen: vec![HaltAck {
            node_id: "web-03".to_owned(),
            ..ack
        }],
        halt_generation: 2,
        force: None,
        force_generation: 1,
        rollout: Rollout {
            version: Some(v("2.4.0")),
            entered: 2,
            done: ["web-01".to_owned()].into(),
            paused: Some(Pause {
                by: "alice".to_owned(),
                reason: "looking".to_owned(),
            }),
        },
    }
}

/// Proves: a (group, package)'s marks are written each under its name, every one at its default
/// left out (so marks at their default are written `{}`), and read back equal; a key this build
/// does not know is refused.
///
/// Catches: a mark lost in the ops file (a halt of a group gone after a restart), or a group's
/// halt read without its reports.
#[test]
fn the_marks_of_a_package_in_a_group_are_written_by_name_and_read_back() {
    let marks = every_mark();
    let written = serde_json::to_value(&marks).expect("serialises");
    assert_eq!(
        written,
        json!({
            "min_version": "2.0.0",
            "last_good": "2.3.0",
            "halt": {"node_id": "web-01", "check": "health", "target": "2.4.0",
                "origin": "operator"},
            "halt_acked": [{"node_id": "web-02", "target": "2.4.0"}],
            "halt_seen": [{"node_id": "web-03", "target": "2.4.0"}],
            "halt_generation": 2,
            "force_generation": 1,
            "rollout": {"version": "2.4.0", "entered": 2, "done": ["web-01"],
                "paused": {"by": "alice", "reason": "looking"}}
        })
    );
    assert_eq!(
        serde_json::from_value::<GroupMarks>(written).expect("reads"),
        marks
    );
    assert_eq!(
        serde_json::to_value(GroupMarks::default()).expect("serialises"),
        json!({})
    );
    let error =
        serde_json::from_value::<GroupMarks>(json!({"bad": ["1.0.0"]})).expect_err("refused");
    assert!(
        error.to_string().starts_with("unknown field `bad`"),
        "{error}"
    );
}

/// Proves: the groups' marks are empty when no (group, package) has one other than its default, a
/// group with no package left among them included, and are then written `{}`; `without_defaults`
/// leaves out every package whose marks are at their default and every group left with none; `check` names a (group, package) whose package is no
/// package name.
///
/// Catches: an ops file that grows a `host_groups` key for nothing (the release before then
/// refuses it), or a load that gives back a different state than the one written.
#[test]
fn the_groups_marks_leave_out_what_is_at_its_default() {
    let mut marks = GroupsMarks::default();
    assert!(marks.is_empty());
    marks.0.insert(name("web"), BTreeMap::new());
    assert!(marks.is_empty());
    marks.0.insert(
        name("db"),
        BTreeMap::from([("tierstore".to_owned(), GroupMarks::default())]),
    );
    assert!(marks.is_empty(), "marks at their default are none");
    assert_eq!(
        serde_json::to_value(&marks).expect("serialises"),
        serde_json::json!({})
    );
    assert_eq!(marks.clone().without_defaults(), GroupsMarks::default());
    marks
        .0
        .entry(name("web"))
        .or_default()
        .insert("tierstore".to_owned(), every_mark());
    assert!(!marks.is_empty());
    let kept = marks.clone().without_defaults();
    assert_eq!(
        kept.0.keys().map(GroupName::as_str).collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(kept.check(), Ok(()));
    marks
        .0
        .entry(name("web"))
        .or_default()
        .insert("a b".to_owned(), every_mark());
    assert_eq!(
        marks.check(),
        Err("host group web: \"a b\" is not a package name".to_owned())
    );
}
