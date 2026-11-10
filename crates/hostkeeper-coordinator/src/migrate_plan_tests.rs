//! The plan of `host-groups migrate` over a fleet of roles, host entries, role entries, selectors
//! and per-role limits: the groups it makes, what each carries, and what it says.

use std::collections::BTreeSet;

use crate::assignment::Entry;
use crate::migrate_support::{NOW, Store, desired, members};
use crate::ops::AuditEntry;
use crate::testutil::version;

/// A rollout file of three services: `kbf` on daemons and frontends, `tierstore` on frontends and
/// `hello` on the daemons labelled rack a.
pub(crate) const ROLLOUT: &str = r#"
[[service]]
name = "kbf"
roles = ["daemon", "frontend"]
priority = 5
bake_beats = 2

[[service]]
name = "tierstore"
roles = ["frontend"]
priority = 1

[[service]]
name = "hello"
roles = ["daemon"]
selector = { labels = { rack = "a" } }
"#;

/// Three daemons, two of them in rack a, and a frontend.
fn farm() -> Store {
    let mut store = Store::of(&[
        ("d1", "daemon"),
        ("d2", "daemon"),
        ("d3", "daemon"),
        ("f1", "frontend"),
    ]);
    store.label("d1", "rack", "a");
    store.label("d3", "rack", "a");
    store
}

/// Proves: the hosts of one role whose sets differ become one group per set, the most common set
/// named after the role and the other `<role>-2`; every host is put in the group of its set; and
/// the report says each group with its packages in the order of the rollout file's priority and
/// then by name (`hostkeeper`, which the file does not define, has priority 0).
///
/// Catches: a split that names the wrong set after the role, puts a host in a group that does not
/// list a package it runs, or orders packages by name alone.
#[test]
fn the_hosts_of_a_role_are_split_by_the_set_of_packages_placed_on_them() {
    let plan = farm().plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    assert_eq!(
        members(&plan),
        [
            ("daemon".to_owned(), vec!["d1".to_owned(), "d3".to_owned()]),
            ("daemon-2".to_owned(), vec!["d2".to_owned()]),
            ("frontend".to_owned(), vec!["f1".to_owned()]),
        ]
        .into()
    );
    let groups = &plan.after.loaded.desired.host_groups.0;
    let packages = |name: &str| groups[&name.parse().expect("a name")].packages.clone();
    assert_eq!(packages("daemon"), ["kbf", "hello", "hostkeeper"]);
    assert_eq!(packages("daemon-2"), ["kbf", "hostkeeper"]);
    assert_eq!(packages("frontend"), ["kbf", "tierstore", "hostkeeper"]);
}

/// Proves: the report of the plan, line by line, on the farm: the groups with their hosts and
/// packages, the policy of each (a merged key said with the services it was chosen of), the hosts
/// by group, and how many packages each group may roll at once.
///
/// Catches: a change of the report's words or order, which an operator reads before the window.
#[test]
fn the_report_says_the_groups_their_policies_and_the_hosts() {
    let plan = farm().plan(ROLLOUT);
    assert_eq!(
        plan.lines.join("\n"),
        "\
group daemon (from role daemon): 2 hosts; packages kbf, hello, hostkeeper
  policy: bake_beats 2, max_updating 2
  bake_beats 2: the highest of hello 0, kbf 2
group daemon-2 (from role daemon): 1 hosts; packages kbf, hostkeeper
  policy: bake_beats 2, max_updating 2
group frontend (from role frontend): 1 hosts; packages kbf, tierstore, hostkeeper
  policy: bake_beats 2
  bake_beats 2: the highest of kbf 2, tierstore 0
hosts:
  daemon: d1, d3
  daemon-2: d2
  frontend: f1
repositories:
records:
live_rollouts: 3 groups may each roll 1 packages at once where the fleet rolled 1"
    );
}

/// Proves: a host entry that includes a service a host's role does not run, and one that excludes
/// a service it does, and a role entry, change the host's set before the groups are made: the
/// hosts are grouped by what is placed on them today, and the plan's check finds every host's
/// placement unchanged.
///
/// Catches: a plan that groups by role and rollout file alone, which would drop an operator's
/// entry and change a host's placement.
#[test]
fn host_and_role_entries_decide_a_hosts_set() {
    let mut store = farm();
    store.entry("tierstore", "d2", Entry::Include);
    store.entry("kbf", "d3", Entry::Exclude);
    store.assignments("hello", |a| {
        a.roles.insert("frontend".to_owned(), Entry::Include);
    });
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    let groups = &plan.after.loaded.desired.host_groups.0;
    let at = |name: &str| groups[&name.parse().expect("a name")].packages.clone();
    assert_eq!(at("daemon"), ["kbf", "hello", "hostkeeper"]);
    assert_eq!(at("daemon-2"), ["kbf", "tierstore", "hostkeeper"]);
    assert_eq!(at("daemon-3"), ["hello", "hostkeeper"]);
    assert_eq!(at("frontend"), ["kbf", "tierstore", "hello", "hostkeeper"]);
    assert_eq!(
        members(&plan),
        [
            ("daemon".to_owned(), vec!["d1".to_owned()]),
            ("daemon-2".to_owned(), vec!["d2".to_owned()]),
            ("daemon-3".to_owned(), vec!["d3".to_owned()]),
            ("frontend".to_owned(), vec!["f1".to_owned()]),
        ]
        .into()
    );
}

/// Proves: of two sets with the same number of hosts, the one whose first host sorts first takes
/// the role's name, and the other sets are numbered in the order of their first host's node id.
///
/// Catches: a tie decided by the order the sets were found in, so that a rerun names the groups
/// differently.
#[test]
fn a_tie_goes_to_the_set_whose_first_host_sorts_first() {
    let mut store = Store::of(&[("a2", "daemon"), ("a1", "daemon"), ("a3", "daemon")]);
    store.entry("kbf", "a1", Entry::Include);
    store.entry("kbf", "a2", Entry::Include);
    store.entry("hello", "a3", Entry::Include);
    store.entry("tierstore", "a3", Entry::Include);
    let rollout = "[[service]]\nname = \"kbf\"\nroles = [\"web\"]\n\
                   [[service]]\nname = \"hello\"\nroles = [\"web\"]\n\
                   [[service]]\nname = \"tierstore\"\nroles = [\"web\"]\n";
    let plan = store.plan(rollout);
    assert_eq!(plan.errors, Vec::<String>::new());
    assert_eq!(
        members(&plan),
        [
            ("daemon".to_owned(), vec!["a1".to_owned(), "a2".to_owned()]),
            ("daemon-2".to_owned(), vec!["a3".to_owned()]),
        ]
        .into()
    );
}

/// Proves: a host that is denied is in no group and does not take part in the sets, and a host
/// whose deny row was cleared does.
///
/// Catches: a retired host that forces a group of its own, or an enrolled host left in none.
#[test]
fn a_denied_host_is_in_no_group() {
    use hostkeeper_enrollment::enroll::records::pb;
    let mut store = farm();
    let deny = |id: &str, state: pb::DenyState| pb::Deny {
        schema: 1,
        host_id: id.to_owned(),
        state: state as i32,
        cleared_by: Vec::new(),
    };
    store.moved.records.deny = vec![
        deny("d2", pb::DenyState::Retired),
        deny("d3", pb::DenyState::Cleared),
    ];
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    assert_eq!(
        members(&plan),
        [
            ("daemon".to_owned(), vec!["d1".to_owned(), "d3".to_owned()]),
            ("frontend".to_owned(), vec!["f1".to_owned()]),
        ]
        .into()
    );
}

/// Proves: each group's desired version of a package is the package's desired version today, with
/// the generation, write id and replaced version the store recorded (generation 1 where it
/// records none), for every package of the group that has one and no other.
///
/// Catches: a desired version left behind, a generation that restarts at 1 and so lets a stale
/// `if_generation` through, or a version given to a group that does not list the package.
#[test]
fn desired_versions_are_carried_with_their_generation() {
    use crate::write_record::{Replaced, WriteId, WriteRecord};
    let mut store = farm();
    store.want("kbf", desired("2.1.0"));
    store.want("tierstore", desired("0.9.0"));
    store.moved.loaded.desired.records.insert(
        "kbf".to_owned(),
        WriteRecord {
            generation: 4,
            replaced: Some(Replaced {
                version: version("2.0.0"),
                min_up: crate::desired::RoleLimits::default(),
            }),
            write_id: Some(WriteId::parse("ci-77").expect("a write id")),
        },
    );
    let plan = store.plan(ROLLOUT);
    let groups = &plan.after.loaded.desired.host_groups.0;
    let desired_of = |name: &str| groups[&name.parse().expect("a name")].desired.clone();
    let kbf = desired_of("daemon")["kbf"].clone();
    assert_eq!(kbf.version, version("2.1.0"));
    assert_eq!(kbf.generation, 4);
    assert_eq!(kbf.write_id.as_deref(), Some("ci-77"));
    assert_eq!(kbf.replaced_version, Some(version("2.0.0")));
    assert_eq!(desired_of("daemon").len(), 1);
    assert_eq!(desired_of("frontend").len(), 2);
    let tierstore = desired_of("frontend")["tierstore"].clone();
    assert_eq!((tierstore.generation, tierstore.write_id), (1, None));
    assert!(
        plan.lines
            .contains(&"  desired kbf 2.1.0 (generation 4)".to_owned())
    );
    assert!(
        plan.lines
            .contains(&"  desired tierstore 0.9.0 (generation 1)".to_owned())
    );
}

/// Proves: the limits of a group's policy come from the desired state's per-role values of the
/// group's role, the lowest `max_updating` and the highest `min_up` over the packages that have a
/// desired state, each disagreement said; with no desired state the role's defaults stand (two
/// for a daemon, so that the policy says it, one for a frontend).
///
/// Catches: a group that rolls one daemon at a time where the fleet rolled two, or limits taken
/// from the wrong package.
#[test]
fn limits_come_from_the_desired_state_of_the_role() {
    let mut store = farm();
    let mut kbf = desired("2.1.0");
    kbf.max_updating.set("daemon", 3);
    kbf.min_up.set("daemon", 1);
    let mut hello = desired("1.0.0");
    hello.max_updating.set("daemon", 1);
    hello.min_up.set("daemon", 2);
    store.want("kbf", kbf);
    store.want("hello", hello);
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    let groups = &plan.after.loaded.desired.host_groups.0;
    let policy = |name: &str| groups[&name.parse().expect("a name")].policy.clone();
    assert_eq!(
        (policy("daemon").max_updating, policy("daemon").min_up),
        (1, 2)
    );
    assert_eq!(
        (policy("daemon-2").max_updating, policy("daemon-2").min_up),
        (3, 1)
    );
    assert_eq!(
        (policy("frontend").max_updating, policy("frontend").min_up),
        (1, 0)
    );
    for line in [
        "  max_updating 1: the lowest of hello 1, kbf 3",
        "  min_up 2: the highest of hello 2, kbf 1",
    ] {
        assert!(
            plan.lines.contains(&line.to_owned()),
            "{line}\n{:#?}",
            plan.lines
        );
    }
}

/// Proves: a group whose services the file defines with different keys gets the most careful of
/// each, and the report says each choice.
///
/// Catches: a merge that takes the first service's key, which would roll a group faster than its
/// slowest service allowed.
#[test]
fn a_group_of_services_that_disagree_takes_the_most_careful_value_of_each_key() {
    let rollout = "\
[[service]]
name = \"a\"
roles = [\"web\"]
bake_beats = 5
rollback = \"automatic\"
stages = [60, 100]
rollback_after_failed = { percent = 50, min_hosts = 3 }
defer_backoff = { initial_secs = 600, factor = 2, max_secs = 3600 }
[[service]]
name = \"b\"
roles = [\"web\"]
canary = { count = 1 }
stages = [50, 100]
rollback_after_failed = { percent = 40, min_hosts = 5 }
defer_backoff = { initial_secs = 300, factor = 4, max_secs = 7200 }
";
    let plan = Store::of(&[("w1", "web"), ("w2", "web")]).plan(rollout);
    assert_eq!(plan.errors, Vec::<String>::new());
    let group = &plan.after.loaded.desired.host_groups.0[&"web".parse().expect("a name")];
    assert_eq!(group.policy.bake_beats, 5);
    assert_eq!(group.policy.canary.map(|c| c.count), Some(1));
    assert_eq!(group.policy.rollback_after_failed.percent, 40);
    assert_eq!(group.policy.rollback_after_failed.min_hosts, 3);
    assert_eq!(group.policy.defer_backoff.initial_secs, 600);
    assert_eq!(group.policy.defer_backoff.factor, 4);
    assert_eq!(group.policy.defer_backoff.max_secs, 7200);
    assert!(
        plan.lines
            .contains(&"  bake_beats 5: the highest of a 5, b 0".to_owned()),
        "{:#?}",
        plan.lines
    );
}

/// Proves: the halt, its acknowledgements and generation, the last good version and the force
/// generation of a service are copied to each group that lists it; a force that orders hosts of
/// the group stays, cut to the group's hosts, a role target now naming the group, and one that
/// orders none is left out; the services' own marks are not changed.
///
/// Catches: a halt that the migration lifts by dropping it, a force that follows hosts into a
/// group they are not in, or a generation that restarts and reuses a force order id.
#[test]
fn marks_halts_and_forces_are_carried_to_the_groups_that_hold_the_hosts() {
    use crate::force::{ForceRecord, ForceTargets};
    use crate::ops::{Halt, HaltAck, HaltOrigin};
    use crate::registry::Digest;
    let mut store = farm();
    let ops = &mut store.moved.loaded.ops;
    let force = |targets: ForceTargets, hosts: &[&str]| ForceRecord {
        version: version("2.1.0"),
        generation: 3,
        reason: "fix forward".to_owned(),
        by: "alice".to_owned(),
        digest: Digest::parse(&"ab".repeat(32)).expect("a digest"),
        release_index: None,
        halt_generation: 1,
        targets,
        include_held: false,
        hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
    };
    let marks = ops.services.entry("kbf".to_owned()).or_default();
    marks.last_good = Some(version("2.0.0"));
    marks.halt = Some(Halt {
        node_id: "d1".to_owned(),
        check: "health".to_owned(),
        target: version("2.1.0"),
        report: None,
        origin: HaltOrigin::Operator,
    });
    marks.halt_acked = vec![HaltAck {
        node_id: "d2".to_owned(),
        target: version("2.0.5"),
        report: None,
    }];
    marks.halt_generation = 2;
    marks.force_generation = 3;
    marks.force = Some(force(
        ForceTargets::Role("daemon".to_owned()),
        &["d1", "d2"],
    ));
    let tier = ops.services.entry("tierstore".to_owned()).or_default();
    tier.force = Some(force(
        ForceTargets::Hosts(vec!["d1".to_owned(), "f1".to_owned()]),
        &["d1", "f1"],
    ));
    tier.force_generation = 1;
    let hello = ops.services.entry("hello".to_owned()).or_default();
    hello.force = Some(force(ForceTargets::Fleet, &["d2", "d3"]));
    hello.halt_generation = 4;
    let before = store.moved.loaded.ops.services.clone();
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    let marks = &plan.after.loaded.ops.host_groups.0;
    let at = |group: &str, package: &str| marks[&group.parse().expect("a name")][package].clone();
    for group in ["daemon", "daemon-2", "frontend"] {
        let kbf = at(group, "kbf");
        assert_eq!(kbf.last_good, Some(version("2.0.0")), "{group}");
        assert_eq!(
            kbf.halt.as_ref().map(|h| h.node_id.as_str()),
            Some("d1"),
            "{group}"
        );
        assert_eq!(kbf.halt_acked.len(), 1, "{group}");
        assert_eq!(
            (kbf.halt_generation, kbf.force_generation),
            (2, 3),
            "{group}"
        );
    }
    let daemon_force = at("daemon", "kbf").force.expect("a force");
    assert_eq!(daemon_force.hosts, ["d1"]);
    assert_eq!(
        daemon_force.targets,
        ForceTargets::Role("daemon".to_owned())
    );
    let second = at("daemon-2", "kbf").force.expect("a force");
    assert_eq!(second.hosts, ["d2"]);
    assert_eq!(second.targets, ForceTargets::Role("daemon-2".to_owned()));
    assert_eq!(at("frontend", "kbf").force, None);
    let tier_force = at("frontend", "tierstore").force.expect("a force");
    assert_eq!(tier_force.hosts, ["f1"]);
    assert_eq!(
        tier_force.targets,
        ForceTargets::Hosts(vec!["f1".to_owned()])
    );
    let fleet = at("daemon", "hello").force.expect("a force");
    assert_eq!(
        (fleet.hosts, fleet.targets),
        (vec!["d3".to_owned()], ForceTargets::Fleet)
    );
    assert_eq!(at("daemon-2", "kbf").force_generation, 3);
    assert_eq!(plan.after.loaded.ops.services, before);
    for line in [
        "  last_good kbf 2.0.0",
        "  halt kbf: 2.1.0 failed health on d1, generation 2",
        "  force kbf: none standing, generation 3",
        "  halt hello: none standing, generation 4",
        "  force hello 2.1.0 (fleet, generation 3, hosts d3)",
        "  force kbf 2.1.0 (role daemon, generation 3, hosts d1)",
        "  force tierstore 2.1.0 (hosts f1, generation 3, hosts f1)",
    ] {
        assert!(
            plan.lines.contains(&line.to_owned()),
            "{line}\n{:#?}",
            plan.lines
        );
    }
}

/// Proves: the plan's position of a service is copied to each group that lists it, with the done
/// hosts split by the group each host is in, the stage entered no further than the group's plan
/// has, the version and the pause kept.
///
/// Catches: a done host counted in a group it is not in, which would end a stage early, or a
/// position past the last stage of a group with fewer stages.
#[test]
fn a_plan_position_is_copied_with_its_done_hosts_split_by_group() {
    use crate::rollouts::{Pause, Rollout};
    let mut store = farm();
    store.moved.loaded.ops.rollouts.insert(
        "kbf".to_owned(),
        Rollout {
            version: Some(version("2.1.0")),
            entered: 9,
            done: ["d1", "d2", "f1"].iter().map(|h| (*h).to_owned()).collect(),
            paused: Some(Pause {
                by: "alice".to_owned(),
                reason: "wait".to_owned(),
            }),
        },
    );
    let plan = store.plan(ROLLOUT);
    let marks = &plan.after.loaded.ops.host_groups.0;
    let position = |group: &str| {
        marks[&group.parse().expect("a name")]["kbf"]
            .rollout
            .clone()
    };
    for (group, done) in [
        ("daemon", vec!["d1"]),
        ("daemon-2", vec!["d2"]),
        ("frontend", vec!["f1"]),
    ] {
        let at = position(group);
        assert_eq!(at.version, Some(version("2.1.0")), "{group}");
        assert_eq!(at.entered, 4, "{group}: clamped to the four default stages");
        assert_eq!(
            at.done,
            done.iter()
                .map(|h| (*h).to_owned())
                .collect::<BTreeSet<_>>(),
            "{group}"
        );
        assert_eq!(
            at.paused.as_ref().map(|p| p.by.as_str()),
            Some("alice"),
            "{group}"
        );
    }
    assert!(plan.lines.contains(
        &"  plan position kbf: 4 stages entered, 1 hosts done, paused by alice: wait".to_owned()
    ));
}

/// Proves: a hold of a role is kept and copied, with the scope of the group, to each other group
/// of that role; holds of a host and of the fleet are not copied.
///
/// Catches: a hold that stops covering the hosts split off into `<role>-2`.
#[test]
fn a_hold_of_a_role_is_copied_to_the_other_groups_of_the_role() {
    use crate::holds::{HoldMark, HoldScope};
    let mut store = farm();
    let hold = |scope: HoldScope| HoldMark {
        service: "kbf".to_owned(),
        scope,
        reason: "freeze".to_owned(),
        by: "alice".to_owned(),
        at_unix: 5,
    };
    store.moved.loaded.ops.holds = vec![
        hold(HoldScope::Role("daemon".to_owned())),
        hold(HoldScope::Host("d2".to_owned())),
        hold(HoldScope::Fleet),
    ];
    let plan = store.plan(ROLLOUT);
    let scopes: Vec<HoldScope> = plan
        .after
        .loaded
        .ops
        .holds
        .iter()
        .map(|h| h.scope.clone())
        .collect();
    assert_eq!(
        scopes,
        [
            HoldScope::Role("daemon".to_owned()),
            HoldScope::Host("d2".to_owned()),
            HoldScope::Fleet,
            HoldScope::Role("daemon-2".to_owned()),
        ]
    );
}

/// Proves: a group's minimum version of a package is the higher of the role's minimum version and
/// the rollout file's `min_version`, each set one audit entry by `migration` that says where it
/// came from; host minimum versions and uninstall intents are left as they are.
///
/// Catches: a floor of the rollout file that is lost with the file, or a role minimum that the
/// file's lower floor overwrites.
#[test]
fn minimum_versions_are_the_higher_of_the_role_and_the_file() {
    let rollout = "\
[[service]]
name = \"kbf\"
roles = [\"daemon\", \"frontend\"]
min_version = \"1.5.0\"
[[service]]
name = \"hello\"
roles = [\"daemon\"]
selector = { labels = { rack = \"a\" } }
min_version = \"1.0.0\"
";
    let mut store = farm();
    store.assignments("kbf", |a| {
        a.role_min_versions
            .insert("daemon".to_owned(), version("2.0.0"));
        a.host_min_versions
            .insert("d2".to_owned(), version("2.5.0"));
        a.uninstall.insert("d3".to_owned(), 7);
        a.uninstall_generation = 7;
    });
    store.assignments("hello", |a| {
        a.role_min_versions
            .insert("daemon".to_owned(), version("0.5.0"));
    });
    let before = store.moved.loaded.ops.assignments.clone();
    let plan = store.plan(rollout);
    assert_eq!(plan.errors, Vec::<String>::new());
    let marks = &plan.after.loaded.ops.host_groups.0;
    let minimum = |group: &str, package: &str| {
        marks[&group.parse().expect("a name")][package]
            .min_version
            .clone()
    };
    assert_eq!(minimum("daemon", "kbf"), Some(version("2.0.0")));
    assert_eq!(minimum("daemon-2", "kbf"), Some(version("2.0.0")));
    assert_eq!(minimum("frontend", "kbf"), Some(version("1.5.0")));
    assert_eq!(minimum("daemon", "hello"), Some(version("1.0.0")));
    let said: Vec<&str> = plan
        .audit
        .iter()
        .filter(|line| line.action == "set_min_version")
        .map(|line| line.detail.reason.as_deref().expect("a reason"))
        .collect();
    assert_eq!(
        said,
        [
            "min_version 1.0.0: from the rollout file",
            "min_version 2.0.0: from role daemon",
            "min_version 2.0.0: from role daemon",
            "min_version 1.5.0: from the rollout file",
        ]
    );
    assert!(plan.audit.iter().all(|line| line.by == "migration"));
    assert_eq!(plan.after.loaded.ops.assignments, before);
}

/// Proves: the audit lines are numbered after the newest line of the store (the ring's or the
/// audit history's, whichever is higher), `host_group_put` of a group before that group's
/// minimum versions and the `set_repository` entries last, each by `migration` with the group or
/// service it concerns, and the ring of the ops state ends with them.
///
/// Catches: a number that reuses one the store gave, or entries out of the order the design gives.
#[test]
fn audit_entries_follow_the_stores_newest_line() {
    let mut store = Store::of(&[("w1", "web")]);
    store.moved.loaded.ops.audit = vec![AuditEntry {
        seq: 40,
        ..AuditEntry::new("alice", "hold")
    }];
    store.moved.audit = vec![crate::audit_file::AuditLine::of(
        &AuditEntry {
            seq: 41,
            ..AuditEntry::new("alice", "hold")
        },
        1,
    )];
    let rollout = "[[service]]\nname = \"kbf\"\nroles = [\"web\"]\nmin_version = \"1.0.0\"\n";
    let plan = store.plan(rollout);
    let lines: Vec<(u64, &str, Option<&str>, Option<&str>)> = plan
        .audit
        .iter()
        .map(|l| {
            (
                l.seq,
                l.action.as_str(),
                l.detail.host_group.as_deref(),
                l.detail.service.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            (42, "host_group_put", Some("web"), None),
            (43, "set_min_version", Some("web"), Some("kbf")),
        ]
    );
    assert_eq!(plan.audit[0].at_unix, NOW);
    assert_eq!(
        plan.audit[0].detail.reason.as_deref(),
        Some("migrated from role web: 1 hosts")
    );
    let ring: Vec<u64> = plan.after.loaded.ops.audit.iter().map(|e| e.seq).collect();
    assert_eq!(ring, [40, 42, 43]);
    assert_eq!(plan.after.audit.len(), 3);
}

/// Proves: with 70 entries in the ring the ring keeps its last 64 once the migration's entries are
/// added.
///
/// Catches: a ring that grows past the size every other writer holds it to.
#[test]
fn the_ring_keeps_its_size() {
    let mut store = Store::of(&[("w1", "web")]);
    store.moved.loaded.ops.audit = (1..=70)
        .map(|seq| AuditEntry {
            seq,
            ..AuditEntry::new("alice", "hold")
        })
        .collect();
    let plan = store.plan("[[service]]\nname = \"kbf\"\nroles = [\"web\"]\n");
    let ring = &plan.after.loaded.ops.audit;
    assert_eq!(ring.len(), crate::ops::AUDIT_RING);
    assert_eq!(ring.last().map(|e| e.seq), Some(71));
}

/// Proves: a service that names a repository other than `komira` is recorded on the service with
/// one `set_repository` entry by `migration`, a service on the default is not, and a repository
/// the registry does not hold is an error naming the service.
///
/// Catches: a private config package that would deploy from `komira` after the migration, or a
/// repository the registry cannot serve written without a word.
#[test]
fn the_repository_of_a_service_is_carried_and_must_be_in_the_registry() {
    let rollout = "\
[[service]]
name = \"kbf\"
roles = [\"web\"]
[[service]]
name = \"kbf-config\"
roles = [\"web\"]
repository = \"private\"
";
    let mut store = Store::of(&[("w1", "web")]);
    let plan = store.plan(rollout);
    assert_eq!(
        plan.errors,
        ["service kbf-config: its repository private is not in the registry"]
    );
    store.moved.loaded.desired.repositories = crate::repositories::Registry::default()
        .put(crate::repositories_support::repository(
            "private",
            "https://private.example.com/apt",
            &["testing"],
            2,
        ))
        .expect("a registry");
    let plan = store.plan(rollout);
    assert_eq!(plan.errors, Vec::<String>::new());
    let services = &plan.after.loaded.ops.services;
    assert_eq!(
        services["kbf-config"].repository.as_deref(),
        Some("private")
    );
    assert!(!services.contains_key("kbf"));
    let last = plan.audit.last().expect("an entry");
    assert_eq!(
        (
            last.action.as_str(),
            last.by.as_str(),
            last.detail.service.as_deref(),
            last.detail.host_group.as_deref(),
            last.detail.reason.as_deref()
        ),
        (
            "set_repository",
            "migration",
            Some("kbf-config"),
            None,
            Some("repository private: from the rollout file")
        )
    );
    assert!(plan.lines.contains(&"  kbf-config: private".to_owned()));
}

/// Proves: every enrolled host's record names its group (a record that names it already is not
/// rewritten), an approved request not yet polled names the group of its role, an inventory record
/// that names a role and no group names that role's group, and a role only a record names gets a
/// group of its own with the set the file and the role's entries place on a host of that role.
///
/// Catches: a record left naming no group, which the coordinator would then place nowhere, or a
/// record rewritten that did not change.
#[test]
fn records_name_the_group_of_their_role() {
    use crate::inventory::InventoryRecord;
    use hostkeeper_enrollment::enroll::records::pb;
    let mut store = farm();
    store.moved.records.hosts[0].host_group = "daemon".to_owned();
    store.moved.records.hosts[1].moving_from = Some("old".to_owned());
    let approved = |id: u8, role: &str, state: pb::RequestState| pb::Request {
        request_id: vec![id; 32],
        state: state as i32,
        approved: Some(pb::Approval {
            host_id: format!("n{id}"),
            role: role.to_owned(),
            ..pb::Approval::default()
        }),
        ..pb::Request::default()
    };
    store.moved.records.requests = vec![
        approved(1, "frontend", pb::RequestState::Approved),
        approved(2, "frontend", pb::RequestState::Issued),
        approved(3, "mac", pb::RequestState::Approved),
    ];
    let record = |host: &str, role: &str, group: &str| InventoryRecord {
        host_id: host.to_owned(),
        role: role.to_owned(),
        host_group: group.to_owned(),
        data_macs: Vec::new(),
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "alice".to_owned(),
        at_unix: 1,
        approve_until_unix: 0,
        used: None,
        platform: None,
    };
    store.moved.loaded.ops.inventory = vec![
        record("n10", "daemon", ""),
        record("n11", "storage", ""),
        record("n12", "", ""),
        record("n13", "daemon", "daemon-2"),
    ];
    store.assignments("hello", |a| {
        a.roles.insert("storage".to_owned(), Entry::Include);
    });
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    let hosts: Vec<(&str, &str)> = plan
        .changed
        .hosts
        .iter()
        .map(|h| (h.host_id.as_str(), h.host_group.as_str()))
        .collect();
    assert_eq!(
        hosts,
        [("d2", "daemon-2"), ("d3", "daemon"), ("f1", "frontend")]
    );
    assert!(plan.changed.hosts.iter().all(|h| h.moving_from.is_none()));
    let requests: Vec<(u8, String)> = plan
        .changed
        .requests
        .iter()
        .map(|r| {
            (
                r.request_id[0],
                r.approved.clone().expect("approved").host_group,
            )
        })
        .collect();
    assert_eq!(
        requests,
        [(1, "frontend".to_owned()), (3, "mac".to_owned())]
    );
    let groups = &plan.after.loaded.desired.host_groups.0;
    assert_eq!(
        groups[&"storage".parse().expect("a name")].packages,
        ["hello", "hostkeeper"]
    );
    assert_eq!(
        groups[&"mac".parse().expect("a name")].packages,
        ["hostkeeper"]
    );
    let inventory: Vec<&str> = plan
        .after
        .loaded
        .ops
        .inventory
        .iter()
        .map(|r| r.host_group.as_str())
        .collect();
    assert_eq!(inventory, ["daemon", "storage", "", "daemon-2"]);
    for line in [
        "  approval of n1: host_group frontend",
        "  approval of n3: host_group mac",
        "  inventory record n10: host_group daemon",
        "  inventory record n11: host_group storage",
    ] {
        assert!(
            plan.lines.contains(&line.to_owned()),
            "{line}\n{:#?}",
            plan.lines
        );
    }
    assert_eq!(
        plan.after.records.requests[0]
            .approved
            .as_ref()
            .map(|a| a.host_group.as_str()),
        Some("frontend")
    );
    assert_eq!(plan.after.records.hosts[1].host_group, "daemon-2");
}

/// Proves: a Mac allowlist entry is said with the `host_group` it needs and the services that go;
/// an entry whose services its group does not list is an error naming its serial; an entry
/// naming a role no host has makes that role a group.
///
/// Catches: an allowlist the new coordinator refuses at start with no warning before the window.
#[test]
fn the_allowlist_entries_say_what_they_need() {
    let list = "\
[[mac]]
serial = \"C02AAA\"
host_id_prefix = \"mac-\"
role = \"frontend\"
services = [\"kbf\"]
[[mac]]
serial = \"C02BBB\"
host_id_prefix = \"mac-\"
host_group = \"frontend\"
[[mac]]
serial = \"C02CCC\"
host_id_prefix = \"mac-\"
role = \"laptop\"
services = [\"hello\"]
";
    let plan = farm().plan_with(ROLLOUT, Some(list));
    assert_eq!(
        plan.errors,
        ["allowlist entry C02CCC: its services hello are not packages of group laptop"]
    );
    for line in [
        "  allowlist entry C02AAA: host_group = \"frontend\", and its services kbf go",
        "  allowlist entry C02BBB: host_group = \"frontend\"",
        "  allowlist entry C02CCC: host_group = \"laptop\", and its services hello go",
    ] {
        assert!(
            plan.lines.contains(&line.to_owned()),
            "{line}\n{:#?}",
            plan.lines
        );
    }
    assert!(
        plan.after
            .loaded
            .desired
            .host_groups
            .0
            .contains_key(&"laptop".parse().expect("a name"))
    );
}

/// Proves: the store's other state is not touched: the exclusions, labels, assignments, the
/// services' own marks and desired states, and the releases stay as they were; the groups,
/// marks, minimum versions, repositories, holds, inventory and the audit are the changes.
///
/// Catches: a migration that drops the state it only reads, which a later release is to take away.
#[test]
fn what_the_migration_only_reads_is_left_as_it_was() {
    let mut store = farm();
    store.want("kbf", desired("2.1.0"));
    store.entry("kbf", "d2", Entry::Exclude);
    store.moved.loaded.ops.excluded = vec![crate::ops::Exclusion {
        service: "kbf".to_owned(),
        node_id: "d1".to_owned(),
        reason: "bad disk".to_owned(),
    }];
    let plan = store.plan(ROLLOUT);
    let (before, after) = (&store.moved.loaded, &plan.after.loaded);
    assert_eq!(after.ops.excluded, before.ops.excluded);
    assert_eq!(after.ops.hosts, before.ops.hosts);
    assert_eq!(after.ops.assignments, before.ops.assignments);
    assert_eq!(after.desired.desired, before.desired.desired);
    assert_eq!(after.desired.releases, before.desired.releases);
    assert_eq!(plan.after.records.requests, store.moved.records.requests);
    assert_eq!(plan.after.records.deny, store.moved.records.deny);
}
