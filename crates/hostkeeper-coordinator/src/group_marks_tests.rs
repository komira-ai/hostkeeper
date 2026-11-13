//! The marks of a (group, package) and the versions of a package ([`crate::group_scope`]): what an
//! automatic rollback, a version marked bad, a minimum version, a standing force, the plan's
//! position and an operator's assignments do in group mode, and what stays one package's in every
//! group.

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Phase, ServiceReply};

use crate::assignment::Entry;
use crate::force::{ForceRecord, ForceTargets};
use crate::group_decision_support::{Farm, failed, policy, stages};
use crate::group_scope::higher;
use crate::host_groups::GroupName;
use crate::marks_support::{
    INSTALLER, NEW_DAEMON, ask, ask_typed, beat_of, digest, force_only, runs, update,
};
use crate::ops::{AuditEntry, Write};
use crate::orders::Order;
use crate::registry::Digest;
use crate::store::store_failure;
use crate::testutil::version;

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn group(name: &str) -> GroupName {
    name.parse().expect("a group name")
}

fn automatic() -> hostkeeper_proto::hostkeeper::operator::v1::RolloutPolicy {
    policy(|p| {
        p.rollback = hostkeeper_proto::hostkeeper::operator::v1::RollbackMode::Automatic.into();
        p.rollback_after_failed = Some(hostkeeper_proto::hostkeeper::operator::v1::FailThreshold {
            percent: Some(1),
            min_hosts: Some(1),
        });
        p.stages = stages(&[100]);
        p.max_updating = Some(10);
    })
}

/// Proves: an automatic rollback in a group sets that group's desired version back to that group's
/// last good version and no other group's: a release that fails on a host of `alpha` is marked bad
/// (for the package, audited for the group), `alpha` goes back to the version it recorded as good,
/// `beta`, which wants the same bad version, keeps the desired version and the last good version it
/// had, and orders its host nothing of the version marked bad.
///
/// Catches: a rollback that sets the version of every group that lists the package, a last good
/// version shared by the groups, or a version marked bad in one group ordered in another.
#[test]
fn a_rollback_sets_back_the_group_that_failed_and_the_bad_version_stays_bad_everywhere() {
    let farm = Farm::new("gm-rollback");
    farm.group("alpha", &["web"], Some(automatic()));
    farm.group("beta", &["web"], Some(automatic()));
    for host in ["a1", "a2"] {
        farm.host(host, "alpha");
    }
    farm.host("b1", "beta");
    farm.want("alpha", "web", "1.0");
    farm.want("beta", "web", "1.5");
    for _ in 0..2 {
        for host in ["a1", "a2"] {
            assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
        }
        assert_eq!(farm.tell("b1", &[("web", "1.5")]), NOTHING);
    }
    let ops = farm.world.ops();
    assert_eq!(
        ops.group_marks(&group("alpha"), "web").last_good,
        Some(version("1.0"))
    );
    assert_eq!(
        ops.group_marks(&group("beta"), "web").last_good,
        Some(version("1.5"))
    );
    assert!(
        ops.services
            .get("web")
            .is_none_or(|marks| marks.last_good.is_none())
    );

    farm.want("alpha", "web", "2.0");
    farm.want("beta", "web", "2.0");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    let audited = farm.world.audit().len();
    assert_eq!(
        farm.tell_with(
            "a1",
            vec![
                runs("hostkeeper", "1.0"),
                failed("web", "1.0", "2.0", Phase::AfterCommit)
            ]
        ),
        NOTHING
    );

    assert_eq!(farm.wanted("alpha", "web"), Some(version("1.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("2.0")));
    let ops = farm.world.ops();
    assert_eq!(
        ops.services.get("web").map(|marks| marks.bad.clone()),
        Some(vec![version("2.0")])
    );
    assert_eq!(
        ops.group_marks(&group("beta"), "web").last_good,
        Some(version("1.5"))
    );
    let marked: Vec<AuditEntry> = farm
        .world
        .audit_after(audited)
        .into_iter()
        .filter(|entry| entry.action == "mark_bad")
        .collect();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].host_group.as_deref(), Some("alpha"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("web", "1.5")]), NOTHING);
}

/// Proves: the last good version of a package in one group is recorded for that group alone, and
/// a rollback to a version needs the registry to keep it: the version a group rolled back to is
/// read from its desired record, whose generation was raised by the rollback and which names the
/// version it replaced.
///
/// Catches: a rollback that rewrites the desired state without the record of the change.
#[test]
fn a_rollback_is_recorded_as_a_write_of_the_groups_desired_version() {
    let farm = Farm::new("gm-rollback-record");
    farm.group("alpha", &["web"], Some(automatic()));
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.want("alpha", "web", "1.0");
    for _ in 0..2 {
        for host in ["a1", "a2"] {
            farm.tell(host, &[("web", "1.0")]);
        }
    }
    farm.want("alpha", "web", "2.0");
    let before = farm
        .world
        .coordinator()
        .desired
        .group_record(&group("alpha"), "web")
        .expect("a record");
    farm.tell("a1", &[("web", "1.0")]);
    farm.tell_with(
        "a1",
        vec![
            runs("hostkeeper", "1.0"),
            failed("web", "1.0", "2.0", Phase::AfterCommit),
        ],
    );

    let after = farm
        .world
        .coordinator()
        .desired
        .group_record(&group("alpha"), "web")
        .expect("a record");
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(
        after.replaced.map(|replaced| replaced.version),
        Some(version("2.0"))
    );
}

/// Proves: a minimum version of a package in a group bounds that group's hosts: a group whose
/// minimum is above its desired version orders its host nothing, and another group at the same
/// version is ordered.
///
/// Catches: a minimum version of a role read in place of the group's.
#[test]
fn a_groups_minimum_version_bounds_its_hosts() {
    let farm = Farm::new("gm-min-version");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    farm.want("alpha", "web", "2.0");
    farm.want("beta", "web", "2.0");
    farm.world.seed_marks("alpha", "web", |marks| {
        marks.min_version = Some(version("3.0"))
    });

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: the higher of two minimum versions is the one the definition takes: either alone, the
/// later of both, and none of none.
#[test]
fn the_higher_minimum_version_is_taken() {
    let (low, high): (Option<Version>, Option<Version>) =
        (Some(version("1.0")), Some(version("2.0")));
    assert_eq!(higher(low.clone(), high.clone()), high);
    assert_eq!(higher(high.clone(), low.clone()), high);
    assert_eq!(higher(low.clone(), None), low);
    assert_eq!(higher(None, high.clone()), high);
    assert_eq!(higher(None, None), None);
}

/// Proves: what an operator's assignments say places nothing in group mode: a role entry that
/// excludes the group's name from the package, and a host entry that includes another package on a
/// host, change no order; the group's hosts are ordered what the group lists.
///
/// Catches: the rollout file's assignment path still placing hosts beside the groups.
#[test]
fn assignments_place_nothing_in_group_mode() {
    let farm = Farm::new("gm-assignments");
    farm.group("alpha", &["web"], None);
    farm.host("a1", "alpha");
    farm.want("alpha", "web", "2.0");
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                let web = state.assignments.entry("web".to_owned()).or_default();
                web.roles.insert("alpha".to_owned(), Entry::Exclude);
                web.hosts.insert("a1".to_owned(), Entry::Exclude);
                web.role_min_versions
                    .insert("alpha".to_owned(), version("9.0"));
            },
        )
        .expect("seeded");

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: the plan of a group's package enters its stages by itself and keeps its position for
/// the group: the position is written to the group's mark with an audit entry that names the
/// group, and not to the package's, and a second group's plan of the package is its own.
///
/// Catches: plan positions shared by the groups of a package.
#[test]
fn the_plan_position_is_the_groups() {
    let farm = Farm::new("gm-rollout");
    let plan = policy(|p| {
        p.stages = stages(&[50, 100]);
        p.max_updating = Some(4);
    });
    farm.group("alpha", &["web"], Some(plan));
    farm.group("beta", &["web"], None);
    for host in ["a1", "a2"] {
        farm.host(host, "alpha");
        farm.tell(host, &[("web", "1.0")]);
    }
    farm.host("b1", "beta");
    farm.tell("b1", &[("web", "1.0")]);
    farm.want("alpha", "web", "2.0");
    let audited = farm.world.audit().len();
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));

    let ops = farm.world.ops();
    assert!(!ops.group_marks(&group("alpha"), "web").rollout.is_empty());
    assert!(ops.group_marks(&group("beta"), "web").rollout.is_empty());
    assert!(ops.rollouts.is_empty());
    let advanced: Vec<AuditEntry> = farm
        .world
        .audit_after(audited)
        .into_iter()
        .filter(|entry| entry.action == "rollout_advance")
        .collect();
    assert!(!advanced.is_empty());
    assert!(
        advanced
            .iter()
            .all(|entry| entry.host_group.as_deref() == Some("alpha"))
    );
}

/// Proves: a host a group's standing force orders is no host of the group's plan: with
/// `max_updating` at 0 the group's plan orders nobody, and the forced host is ordered all the
/// same, as a forced host is in a plan of none.
///
/// Catches: a standing force read for the package alone, which would leave the forced host held by
/// the plan of the group.
#[test]
fn a_hosts_a_groups_force_orders_are_no_hosts_of_its_plan() {
    let farm = Farm::new("gm-force");
    farm.group(
        "alpha",
        &["web"],
        Some(policy(|p| p.max_updating = Some(0))),
    );
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.want("alpha", "web", "2.0");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    farm.world.seed_marks("alpha", "web", |marks| {
        marks.force = Some(ForceRecord {
            version: version("2.0"),
            generation: 1,
            reason: "for the tests".to_owned(),
            by: "operator".to_owned(),
            digest: Digest::parse(&digest(1)).expect("a digest"),
            release_index: None,
            halt_generation: 0,
            targets: ForceTargets::Hosts(vec!["a1".to_owned()]),
            include_held: false,
            hosts: vec!["a1".to_owned()],
        });
    });

    let forced = farm.order("a1", &[("web", "1.0")]);
    assert!(
        matches!(
            forced,
            Some(Order::Force { ref service, version: ref to, .. })
                if service == "web" && *to == version("2.0")
        ),
        "{forced:?}"
    );
    assert_eq!(force_only(farm.order("a2", &[("web", "1.0")])), None);
}

/// Proves: the packages a group no longer lists are uninstalled from its hosts by the intents
/// `Put` wrote, one at a time, the one listed last first, the rollout file's packages and those it
/// does not define alike, until the host lists none and the intents are settled; a host of another
/// group that lists them is ordered nothing.
///
/// Catches: an intent the decision cannot read for a package the group's list no longer has, a
/// config package uninstalled after the service it configures, or an intent that outlives the
/// package.
#[test]
fn the_packages_taken_out_of_a_group_are_uninstalled_last_listed_first() {
    let farm = Farm::new("gm-uninstall");
    farm.group("alpha", &["web", "cfg", "db"], None);
    farm.group("beta", &["web", "cfg", "db"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    let running = |gone: &[&str]| {
        let kept = ["hostkeeper", "web", "cfg", "db"]
            .into_iter()
            .filter(|name| !gone.contains(name))
            .map(|name| runs(name, "1.0"))
            .collect();
        beat_of(INSTALLER, kept)
    };
    for host in ["a1", "b1"] {
        let (reply, order) = ask_typed(&farm.world.rig, host, "storage", running(&[]));
        assert_eq!((reply, order), (NOTHING, None));
    }
    farm.world
        .put("alpha", &["hostkeeper", "web"], Some(1))
        .expect("put");
    let uninstalls = |gone: &[&str]| {
        let (reply, order) = ask_typed(&farm.world.rig, "a1", "storage", running(gone));
        assert_eq!(reply, NOTHING);
        match order {
            Some(Order::Uninstall { service, .. }) => Some(service),
            other => {
                assert!(other.is_none(), "{other:?}");
                None
            }
        }
    };

    assert_eq!(uninstalls(&[]), Some("db".to_owned()));
    assert_eq!(uninstalls(&["db"]), Some("cfg".to_owned()));
    assert_eq!(uninstalls(&["db", "cfg"]), None);
    assert_eq!(farm.world.intents(), []);
    let (reply, order) = ask_typed(&farm.world.rig, "b1", "storage", running(&[]));
    assert_eq!((reply, order), (NOTHING, None));
}

/// Proves: a package a group lists and a host does not run is installed from the group's desired
/// version and release, one host at a time by the group's slot, and the slot an install order
/// holds counts for the group alone: the second host of the group waits and a host of another group
/// is ordered its install.
///
/// Catches: the slot of an install order, which no beat of the host shows yet, counted for hosts
/// of every group.
#[tokio::test]
async fn an_install_holds_a_slot_of_its_group_alone() {
    let farm = Farm::new("gm-install");
    farm.group("alpha", &["db"], None);
    farm.group("beta", &["db"], None);
    for host in ["a1", "a2"] {
        farm.host(host, "alpha");
    }
    farm.host("b1", "beta");
    farm.deploy("", "db", "2.0", 7).await;
    let beat = || beat_of(INSTALLER, vec![runs("hostkeeper", "1.0")]);

    let (_, order) = ask_typed(&farm.world.rig, "a1", "storage", beat());
    assert!(matches!(order, Some(Order::Install { ref service, .. }) if service == "db"));
    let (_, order) = ask_typed(&farm.world.rig, "a2", "storage", beat());
    assert!(order.is_none());
    let (_, order) = ask_typed(&farm.world.rig, "b1", "storage", beat());
    assert!(matches!(order, Some(Order::Install { ref service, .. }) if service == "db"));
}

/// Proves: the verdicts of a release counted for a group's hosts are forgotten for that group's
/// desired version alone: forgetting those about versions the group does not want leaves the
/// verdicts of another group's hosts, which want another version.
///
/// Catches: a prune of the verdicts of a package that drops a second group's.
#[test]
fn forgetting_verdicts_leaves_another_groups_hosts() {
    use hostkeeper_wire::wire::{Class, Outcome, UpdateResult};

    use crate::verdicts::Verdicts;

    let failure = |target: &str| {
        let mut beat = crate::testutil::beat("1.0");
        beat.last_update = Some(UpdateResult {
            target: version(target),
            outcome: Outcome::Failed,
            phase: Some(Phase::AfterCommit),
            class: Some(Class::Release),
            reason: None,
            order_id: None,
            order_try: None,
        });
        beat
    };
    let mut verdicts = Verdicts::default();
    verdicts.observe("web", &crate::testutil::id("a1"), &failure("2.0"));
    verdicts.observe("web", &crate::testutil::id("a1"), &failure("3.0"));
    verdicts.observe("web", &crate::testutil::id("b1"), &failure("3.0"));

    let alpha = |id: &hostkeeper_wire::HostId| id.as_str() == "a1";
    verdicts.keep_only_of("web", &version("2.0"), &[], &alpha);

    let hosts = |at: &str| -> Vec<String> {
        verdicts
            .of("web", &version(at))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    assert_eq!(hosts("2.0"), ["a1"]);
    assert_eq!(hosts("3.0"), ["b1"]);
}

/// Proves: a host whose group cannot be read is in no group for that heartbeat: the read fails,
/// is said in the log, and the host is ordered nothing; and a record that names a group the
/// coordinator does not hold places the host in none either. Both come back to the group once the
/// record reads and the group exists.
///
/// Catches: a host with an unreadable record falling back to the rollout file, or ordered by a
/// group that is gone.
#[test]
fn a_host_whose_group_cannot_be_read_or_is_not_held_is_in_none() {
    use hostkeeper_enrollment::enroll::Membership;
    use hostkeeper_enrollment::names::HostId as EnrolledId;

    let farm = Farm::new("gm-group-unread");
    farm.group("alpha", &["web"], None);
    farm.host("a1", "alpha");
    farm.want("alpha", "web", "2.0");
    let host = crate::testutil::id("a1");
    let enrolled = EnrolledId::parse("a1").expect("a host id");
    let coordinator = farm.world.coordinator();
    assert_eq!(coordinator.group_of(&host), Some(group("alpha")));

    farm.world.records.fail_at(farm.world.records.count() + 1);
    assert_eq!(coordinator.group_of(&host), None);
    farm.world.records.fail_at(0);

    let store = farm.world.enrollment.enrollment.store();
    let ghost = Membership {
        host_group: "ghost".to_owned(),
        moving_from: None,
    };
    let held = store.set_membership(&enrolled, ghost).expect("written");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), NOTHING);
    store.set_membership(&enrolled, held).expect("written back");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: a verdict acknowledged for a host of a group is superseded against the group's desired
/// version: once the host runs the version its group wants, the verdict of an older target no
/// longer stands in the file; while the host is in no group no version is wanted, and the verdict
/// stands.
///
/// Catches: the version wanted read from the package alone, which no group has.
#[test]
fn a_verdict_is_superseded_by_the_version_the_hosts_group_wants() {
    use hostkeeper_enrollment::enroll::Membership;
    use hostkeeper_enrollment::names::HostId as EnrolledId;
    use hostkeeper_wire::wire::OrderId;

    let farm = Farm::new("gm-supersede");
    farm.group("alpha", &["web"], None);
    farm.host("a1", "alpha");
    farm.want("alpha", "web", "2.0");
    let feature = crate::verdict_file::FEATURE;
    let acking = |running: &str, verdict: bool| {
        let mut one = if verdict {
            failed("web", running, "1.5", Phase::AfterCommit)
        } else {
            runs("web", running)
        };
        if let Some(last) = one.last_update.as_mut() {
            last.order_id = OrderId::parse("o-1");
            last.order_try = Some(1);
        }
        let mut beat = beat_of(NEW_DAEMON, vec![runs("hostkeeper", "1.0"), one]);
        beat.agent_features.push(feature.to_owned());
        beat
    };
    let standing = || {
        farm.world
            .coordinator()
            .verdicts
            .standing("web", &crate::testutil::id("a1"))
    };
    ask(&farm.world.rig, "a1", "storage", acking("1.0", true));
    assert!(standing().is_some());

    let store = farm.world.enrollment.enrollment.store();
    let enrolled = EnrolledId::parse("a1").expect("a host id");
    let ghost = Membership {
        host_group: "ghost".to_owned(),
        moving_from: None,
    };
    let held = store.set_membership(&enrolled, ghost).expect("written");
    ask(&farm.world.rig, "a1", "storage", acking("2.0", false));
    assert!(standing().is_some());

    store.set_membership(&enrolled, held).expect("written back");
    ask(&farm.world.rig, "a1", "storage", acking("2.0", false));
    assert_eq!(standing(), None);
}

/// Proves: a package a group lists and a host does not run is installed from the release a set
/// with targets kept for the group's version: a host that lists `apt` is ordered the install of
/// the `apt` entry its platform lists.
///
/// Catches: the releases a group's versions were set with missing from the picks, which would
/// leave a version set with targets with no package to install.
#[tokio::test]
async fn a_version_set_with_targets_is_installed_from_the_release_kept() {
    let farm = Farm::new("gm-install-targets");
    farm.group("alpha", &["db"], None);
    farm.host("a1", "alpha");
    let native = crate::marks_support::native_targets("db", 1);
    farm.world
        .coordinator()
        .set_desired_in(
            &crate::service::ci_calls::Setter::Operator("operator"),
            br#"{"service":"db","version":"2.0"}"#,
            &crate::write_record::WriteAsk::default(),
            Some(native),
            "alpha",
        )
        .await
        .expect("set");

    let (_, order) = ask_typed(
        &farm.world.rig,
        "a1",
        "storage",
        beat_of(INSTALLER, vec![runs("hostkeeper", "1.0")]),
    );

    let Some(Order::Install {
        service, digest, ..
    }) = order
    else {
        panic!("an install is ordered");
    };
    assert_eq!(service, "db");
    assert_eq!(digest.to_string(), crate::marks_support::digest(1));
}

/// Proves: a rollback whose write of the group's desired version fails is logged and left for the
/// next heartbeat: the version stays marked bad, the group still wants it until the store heals,
/// and the heartbeat after that sets the group back to its last good version.
///
/// Catches: a rollback lost for good when the store failed the moment it was made.
#[test]
fn a_rollback_the_store_refuses_is_made_by_a_later_heartbeat() {
    let farm = Farm::new("gm-rollback-refused");
    farm.group("alpha", &["web"], Some(automatic()));
    for host in ["a1", "a2"] {
        farm.host(host, "alpha");
    }
    farm.want("alpha", "web", "1.0");
    for _ in 0..2 {
        for host in ["a1", "a2"] {
            farm.tell(host, &[("web", "1.0")]);
        }
    }
    farm.want("alpha", "web", "2.0");
    farm.tell("a1", &[("web", "1.0")]);
    farm.world
        .fake
        .refuse(|commit| (commit.desired.is_some() && commit.ops.is_none()).then(store_failure));

    farm.tell_with(
        "a1",
        vec![
            runs("hostkeeper", "1.0"),
            failed("web", "1.0", "2.0", Phase::AfterCommit),
        ],
    );

    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    assert_eq!(
        farm.world
            .ops()
            .services
            .get("web")
            .map(|marks| marks.bad.clone()),
        Some(vec![version("2.0")])
    );
    farm.world.fake.heal();
    farm.tell("a2", &[("web", "1.0")]);
    assert_eq!(farm.wanted("alpha", "web"), Some(version("1.0")));
}
