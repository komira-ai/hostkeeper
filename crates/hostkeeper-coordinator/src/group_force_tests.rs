//! `Releases.Force` once the coordinator holds host groups (`docs/design/hostgroups.md`, section
//! 4): a force is a deployment to a group, so it writes the group's desired version and marks,
//! reaches the groups its targets name, and is one change with one audit entry per group.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_wire::wire::ServiceReply;

use crate::force::ForceTargets;
use crate::group_decision_support::Farm;
use crate::host_groups::GroupName;
use crate::marks_support::{
    digest, fleet_target, force_only, hold, hosts_target, native_target, operator, role_target,
    scope_role,
};
use crate::ops::OpsState;
use crate::orders::Order;
use crate::registry::Digest;
use crate::testutil::version;
use crate::write_record::WriteError;

type Refusal = proto::ReleasesRefusal;

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn group(name: &str) -> GroupName {
    name.parse().expect("a group name")
}

/// Alpha (a1) and beta (b1, b2) list `web`; gamma (c1) lists `db` alone. 1.0 and 2.0 of `web` are
/// registered in alpha and beta, 2.0 desired, and every host of them runs 2.0.
async fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.group("gamma", &["db"], None);
    for (host, group) in [
        ("a1", "alpha"),
        ("b1", "beta"),
        ("b2", "beta"),
        ("c1", "gamma"),
    ] {
        farm.host(host, group);
    }
    farm.deploy("", "web", "1.0", 1).await;
    farm.deploy("", "web", "2.0", 2).await;
    for host in ["a1", "b1", "b2"] {
        assert_eq!(farm.tell(host, &[("web", "2.0")]), NOTHING);
    }
    assert_eq!(farm.tell("c1", &[("db", "1.0")]), NOTHING);
    farm
}

fn request(host_group: &str, version: &str, targets: Targets) -> proto::ForceRequest {
    proto::ForceRequest {
        service: "web".to_owned(),
        version: version.to_owned(),
        reason: "fix the farm".to_owned(),
        targets: Some(targets),
        include_held: false,
        host_group: host_group.to_owned(),
    }
}

fn force(
    farm: &Farm,
    host_group: &str,
    version: &str,
    targets: Targets,
) -> Result<proto::ForceResponse, Refusal> {
    farm.world
        .coordinator()
        .force(&operator(), request(host_group, version, targets))
}

fn forced_hosts(ops: &OpsState, group_name: &str) -> Option<Vec<String>> {
    let force = ops.group_marks(&group(group_name), "web").force?;
    Some(force.hosts)
}

fn force_order(version_text: &str, n: u8, order_id: &str, halt_generation: u64) -> Order {
    Order::Force {
        target: Some(native_target("web", n)),
        service: "web".to_owned(),
        version: version(version_text),
        digest: Digest::parse(&digest(n)).expect("a digest"),
        index_digest: None,
        halt_generation,
        order_id: order_id.to_owned(),
        order_try: 1,
    }
}

/// Proves: a force without `host_group` that names hosts of two groups is one force for each
/// group: each group's desired version is the version forced, each group's marks hold a force of
/// its own hosts, the answer holds one `force` entry for each group (naming it) and the sum of the
/// hosts ordered, the entries are the whole of what was audited, and each host is sent the order
/// of its own group's force (numbered by that group's force).
///
/// Catches: a force that is written to one group, to the package alone, or to every group that
/// lists the package.
#[tokio::test]
async fn a_force_naming_hosts_of_two_groups_is_one_force_for_each_group() {
    let farm = farm("gf-hosts").await;
    let audited = farm.world.audit().len();

    let answer = force(&farm, "", "1.0", hosts_target(&["a1", "b1"])).expect("a force");

    assert_eq!(answer.targeted, 2);
    let entries: Vec<_> = answer
        .entries
        .iter()
        .map(|entry| (entry.action.as_str(), entry.host_group.as_deref()))
        .collect();
    assert_eq!(
        entries,
        [
            ("force", Some("alpha")),
            ("mark_bad", Some("alpha")),
            ("force", Some("beta"))
        ]
    );
    assert_eq!(farm.world.audit_after(audited).len(), 3);
    assert_eq!(farm.wanted("alpha", "web"), Some(version("1.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("1.0")));
    let ops = farm.world.ops();
    assert_eq!(forced_hosts(&ops, "alpha"), Some(vec!["a1".to_owned()]));
    assert_eq!(forced_hosts(&ops, "beta"), Some(vec!["b1".to_owned()]));
    assert_eq!(
        ops.services
            .get("web")
            .and_then(|marks| marks.force.clone()),
        None
    );
    assert_eq!(
        farm.order("a1", &[("web", "2.0")]),
        Some(force_order("1.0", 1, "force-1", 1))
    );
    assert_eq!(
        farm.order("b1", &[("web", "2.0")]),
        Some(force_order("1.0", 1, "force-1", 1))
    );
}

/// Proves: a force that names a host the coordinator has not heard from, a host of no group or a
/// host of a group that does not list the package, together with a host that is fine, is refused
/// `unknown_target` and changes nothing: not the desired versions, not the marks, not the audit.
///
/// Catches: a force that is made for the hosts it can place before it finds one it cannot.
#[tokio::test]
async fn a_force_naming_a_host_no_group_places_refuses_all_and_changes_nothing() {
    let farm = farm("gf-unknown").await;
    farm.world.enroll("never-heard");
    farm.lone("lone");
    assert_eq!(farm.tell_with("lone", vec![]), NOTHING);
    let before = farm.world.state();
    for stranger in ["never-heard", "lone", "c1", "nobody"] {
        assert_eq!(
            force(&farm, "", "1.0", hosts_target(&["a1", stranger])).err(),
            Some(Refusal::UnknownTarget),
            "{stranger}"
        );
    }
    assert_eq!(farm.world.state(), before);
}

/// Proves: with `host_group` the `hosts` must each be in that group and `fleet` is that group
/// alone: a host of another group is `unknown_target` and nothing changes, and a fleet force of
/// alpha orders alpha's hosts and leaves beta's desired version, marks and hosts as they were.
///
/// Catches: a `host_group` that is read as a filter of the answer rather than the group written.
#[tokio::test]
async fn a_force_naming_a_group_reaches_that_group_alone() {
    let farm = farm("gf-named").await;
    let before = farm.world.state();
    assert_eq!(
        force(&farm, "alpha", "1.0", hosts_target(&["a1", "b1"])).err(),
        Some(Refusal::UnknownTarget)
    );
    assert_eq!(farm.world.state(), before);

    let answer = force(&farm, "alpha", "1.0", fleet_target("web")).expect("a force");

    assert_eq!(answer.targeted, 1);
    assert!(
        answer
            .entries
            .iter()
            .all(|entry| entry.host_group.as_deref() == Some("alpha"))
    );
    assert_eq!(farm.wanted("alpha", "web"), Some(version("1.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("2.0")));
    let ops = farm.world.ops();
    assert_eq!(forced_hosts(&ops, "alpha"), Some(vec!["a1".to_owned()]));
    assert_eq!(forced_hosts(&ops, "beta"), None);
    assert_eq!(farm.order("b1", &[("web", "2.0")]), None);
}

/// Proves: a force by `fleet` without `host_group` is one force for each group that lists the
/// package, and one by `role` is the force of the group of that name: gamma, which does not list
/// `web`, is never reached, and a role naming it is `unknown_target`.
///
/// Catches: a fleet force that reaches a group that does not run the package, or a role force that
/// reaches every group.
#[tokio::test]
async fn a_fleet_or_role_force_without_a_group_reaches_each_listing_group_or_the_named_one() {
    let farm = farm("gf-fleet").await;
    let before = farm.world.state();
    assert_eq!(
        force(&farm, "", "1.0", role_target("gamma")).err(),
        Some(Refusal::UnknownTarget)
    );
    assert_eq!(
        force(&farm, "", "1.0", role_target("nothing")).err(),
        Some(Refusal::UnknownTarget)
    );
    assert_eq!(farm.world.state(), before);

    let by_role = force(&farm, "", "1.0", role_target("beta")).expect("a force");
    assert_eq!(by_role.targeted, 2);
    let ops = farm.world.ops();
    assert_eq!(forced_hosts(&ops, "alpha"), None);
    assert_eq!(
        forced_hosts(&ops, "beta"),
        Some(vec!["b1".to_owned(), "b2".to_owned()])
    );

    let everywhere = force(&farm, "", "1.0", fleet_target("web")).expect("a force");
    assert_eq!(everywhere.targeted, 3);
    assert_eq!(
        everywhere
            .entries
            .iter()
            .filter(|entry| entry.action == "force")
            .map(|entry| entry.host_group.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(forced_hosts(&farm.world.ops(), "gamma"), None);
}

/// Proves: a `role` target that names another group than `host_group` is refused
/// `unknown_target` and changes nothing, and one that names the same group is the force of it.
///
/// Catches: a role target that is read for the group named in `host_group`, or the reverse.
#[tokio::test]
async fn a_role_naming_another_group_than_host_group_is_unknown_target() {
    let farm = farm("gf-role").await;
    let before = farm.world.state();
    assert_eq!(
        force(&farm, "alpha", "1.0", role_target("beta")).err(),
        Some(Refusal::UnknownTarget)
    );
    assert_eq!(farm.world.state(), before);

    let same = force(&farm, "beta", "1.0", role_target("beta")).expect("a force");

    assert_eq!(same.targeted, 2);
    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
}

/// Proves: a `host_group` that is no group name is `invalid_request`, one no group has the name of
/// or that does not list the package is `not_in_host_group`, and a package nothing defines is
/// `unknown_service`; nothing changes.
///
/// Catches: a group that is not found read as naming none, which would force every group.
#[tokio::test]
async fn a_force_naming_a_group_that_does_not_list_the_package_is_refused() {
    let farm = farm("gf-not-listed").await;
    farm.deploy("gamma", "db", "1.0", 4).await;
    let before = farm.world.state();
    for (named, refusal) in [
        ("Not A Group", Refusal::InvalidRequest),
        ("nothing", Refusal::NotInHostGroup),
        ("gamma", Refusal::NotInHostGroup),
    ] {
        assert_eq!(
            force(&farm, named, "1.0", fleet_target("web")).err(),
            Some(refusal),
            "{named}"
        );
    }
    let mut db = request("alpha", "1.0", fleet_target("db"));
    db.service = "db".to_owned();
    assert_eq!(
        farm.world.coordinator().force(&operator(), db).err(),
        Some(Refusal::NotInHostGroup)
    );
    let mut unlisted = request("", "1.0", fleet_target("nothing"));
    unlisted.service = "nothing".to_owned();
    assert_eq!(
        farm.world.coordinator().force(&operator(), unlisted).err(),
        Some(Refusal::UnknownService)
    );
    assert_eq!(farm.world.state(), before);
}

/// Proves: the refusals a force makes before it reads a group are the ones it makes without
/// groups: hostkeeper itself, a version that does not parse, a reason that is empty, a version
/// with no registered digest, a fleet target that does not confirm the package, and no targets;
/// and a version below the minimum version of one group refuses the whole force, though the other
/// groups would take it. Nothing changes.
///
/// Catches: a refusal for the first group reached that leaves the forces of the groups before it
/// made.
#[tokio::test]
async fn a_force_is_refused_for_every_group_when_it_is_refused_for_one() {
    let farm = farm("gf-refused").await;
    farm.world.seed_marks("beta", "web", |marks| {
        marks.min_version = Some(version("1.5"));
    });
    let before = farm.world.state();
    let with = |change: &dyn Fn(&mut proto::ForceRequest)| {
        let mut asked = request("", "1.0", fleet_target("web"));
        change(&mut asked);
        farm.world.coordinator().force(&operator(), asked).err()
    };
    assert_eq!(with(&|_| ()), Some(Refusal::BelowMinVersion));
    assert_eq!(
        with(&|asked| asked.service = "hostkeeper".to_owned()),
        Some(Refusal::SelfService)
    );
    assert_eq!(
        with(&|asked| asked.version = "x.y".to_owned()),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        with(&|asked| asked.reason = " ".to_owned()),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        with(&|asked| asked.version = "1.7".to_owned()),
        Some(Refusal::NoDigest)
    );
    assert_eq!(
        with(&|asked| asked.targets = Some(fleet_target("db"))),
        Some(Refusal::ConfirmMismatch)
    );
    assert_eq!(
        with(&|asked| asked.targets = None),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(farm.world.state(), before);
}

/// Proves: a standing force of a group orders its host past the group's plan, and setting another
/// version in that group ends it, audited `force_ended` naming the group, while the force of the
/// other group stands.
///
/// Catches: a force that is ended by a set in another group, or that stands for ever.
#[tokio::test]
async fn a_set_in_a_group_ends_the_force_of_that_group_only() {
    let farm = farm("gf-ends").await;
    force(&farm, "", "1.0", fleet_target("web")).expect("a force");
    assert!(forced_hosts(&farm.world.ops(), "alpha").is_some());
    assert!(forced_hosts(&farm.world.ops(), "beta").is_some());
    let audited = farm.world.audit().len();

    farm.deploy("alpha", "web", "3.0", 3).await;

    let ops = farm.world.ops();
    assert_eq!(forced_hosts(&ops, "alpha"), None);
    assert!(forced_hosts(&ops, "beta").is_some());
    let ended: Vec<_> = farm
        .world
        .audit_after(audited)
        .into_iter()
        .filter(|entry| entry.action == "force_ended")
        .map(|entry| entry.host_group)
        .collect();
    assert_eq!(ended, [Some("alpha".to_owned())]);
    assert_eq!(force_only(farm.order("a1", &[("web", "1.0")])), None);
    assert!(farm.order("b1", &[("web", "2.0")]).is_some());
}

/// Proves: a force skips the hosts a hold of the group's role holds, unless the call includes the
/// held, and a force of a version below what a host runs marks the version left bad for every
/// group while the group's halt is cleared with its own entry.
///
/// Catches: a hold of a group that a force passes without being asked, or a halt of one group that
/// a force of another clears.
#[tokio::test]
async fn a_force_obeys_the_holds_of_a_group_and_clears_only_its_halt() {
    let farm = farm("gf-held").await;
    hold(&farm.world.rig, "web", scope_role("beta")).expect("a hold");
    let skipped = force(&farm, "beta", "1.0", fleet_target("web")).expect("a force");
    assert_eq!(skipped.targeted, 0);
    let mut held = skipped.held.clone();
    held.sort();
    assert_eq!(held, ["b1", "b2"]);

    let mut included = request("beta", "1.0", fleet_target("web"));
    included.include_held = true;
    let answer = farm
        .world
        .coordinator()
        .force(&operator(), included)
        .expect("a force");
    assert_eq!(answer.targeted, 2);
    assert!(
        answer
            .entries
            .iter()
            .any(|entry| entry.action == "mark_bad" && entry.host_group.as_deref() == Some("beta"))
    );

    farm.world.seed_marks("alpha", "web", |marks| {
        marks.halt = Some(crate::ops::Halt {
            node_id: String::new(),
            check: "operator".to_owned(),
            target: version("2.0"),
            report: None,
            origin: crate::ops::HaltOrigin::Operator,
        });
    });
    let cleared = force(&farm, "beta", "1.0", fleet_target("web")).expect("a force");
    assert!(
        cleared
            .entries
            .iter()
            .all(|entry| entry.action != "clear_halt")
    );
    assert!(
        farm.world
            .ops()
            .group_marks(&group("alpha"), "web")
            .halt
            .is_some()
    );
    let cleared = force(&farm, "alpha", "1.0", fleet_target("web")).expect("a force");
    assert_eq!(
        cleared
            .entries
            .iter()
            .filter(|entry| entry.action == "clear_halt")
            .map(|entry| entry.host_group.clone())
            .collect::<Vec<_>>(),
        [Some("alpha".to_owned())]
    );
}

/// Proves: a force of a package the rollout file does not define, which a group lists, is made as
/// any other: its digest is the registered one and its hosts are those of the group that run it;
/// and a target host of the group that does not run the package is skipped `unknown_target`.
///
/// Catches: a force that needs the rollout file to know the package.
#[tokio::test]
async fn a_force_of_a_package_only_a_group_lists_is_made() {
    let farm = Farm::new("gf-cfg");
    farm.group("alpha", &["cfg"], None);
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.deploy("", "cfg", "1.0", 1).await;
    farm.deploy("", "cfg", "2.0", 2).await;
    assert_eq!(farm.tell("a1", &[("cfg", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[]), NOTHING);
    let mut asked = request("", "1.0", hosts_target(&["a1", "a2"]));
    asked.service = "cfg".to_owned();

    let answer = farm
        .world
        .coordinator()
        .force(&operator(), asked)
        .expect("a force");

    assert_eq!(answer.targeted, 1);
    assert_eq!(
        answer
            .skipped
            .iter()
            .map(|host| (host.node_id.as_str(), host.reason.as_str()))
            .collect::<Vec<_>>(),
        [("a2", "unknown_target")]
    );
    let marks = farm.world.ops().group_marks(&group("alpha"), "cfg");
    assert_eq!(
        marks.force.map(|force| force.targets),
        Some(ForceTargets::Hosts(vec!["a1".to_owned(), "a2".to_owned()]))
    );
}

/// Proves: a write that conflicts is `generation_conflict` and one the store failed or does not
/// keep is `store_failed`, the refusals of a force whose desired state could not be staged.
///
/// Catches: a failed write answered as a refusal of the request.
#[test]
fn a_failed_write_of_a_force_is_a_conflict_or_a_store_failure() {
    assert_eq!(
        Refusal::from(WriteError::Conflict { asked: 1, now: 2 }),
        Refusal::GenerationConflict
    );
    assert_eq!(Refusal::from(WriteError::Unrecorded), Refusal::StoreFailed);
    assert_eq!(
        Refusal::from(WriteError::Store(crate::desired::StoreError::Io {
            path: "fake".into(),
            source: std::io::Error::other("the store refuses"),
        })),
        Refusal::StoreFailed
    );
}

/// Proves: a force that cannot be written is refused `store_failed` and changes nothing: not the
/// desired versions, not the marks of the groups, not the audit.
///
/// Catches: a force made in memory that storage does not hold.
#[tokio::test]
async fn a_force_that_cannot_be_written_changes_nothing() {
    let farm = farm("gf-unwritable").await;
    let before = farm.world.state();
    farm.world.fake.fail();

    assert_eq!(
        force(&farm, "", "1.0", fleet_target("web")).err(),
        Some(Refusal::StoreFailed)
    );

    farm.world.fake.heal();
    assert_eq!(farm.world.state(), before);
    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    force(&farm, "", "1.0", fleet_target("web")).expect("a force once it can be written");
}

/// Proves: a force without `host_group` that names the hosts of one group forces that group alone,
/// though another group lists the package; a group that holds marks of another package only is
/// left as it is.
///
/// Catches: a force of every group that lists the package, for the hosts of one.
#[tokio::test]
async fn a_force_naming_hosts_of_one_group_forces_that_group_alone() {
    let farm = farm("gf-one-group").await;
    farm.world.seed_marks("gamma", "db", |marks| {
        marks.min_version = Some(version("0.1"));
    });

    let answer = force(&farm, "", "1.0", hosts_target(&["b1", "b2"])).expect("a force");

    assert_eq!(answer.targeted, 2);
    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("1.0")));
    let ops = farm.world.ops();
    assert_eq!(forced_hosts(&ops, "alpha"), None);
    assert_eq!(
        forced_hosts(&ops, "beta"),
        Some(vec!["b1".to_owned(), "b2".to_owned()])
    );
}

/// Proves: a version set with targets is forced as its release: the force of a version no digest
/// is registered for, which a set with targets kept the release of, is made and stands in the
/// group's marks; a host whose daemon's installers are not read is skipped, as the picks say.
///
/// Catches: a force of a version set with targets that is refused `no_digest`.
#[tokio::test]
async fn a_version_set_with_targets_is_forced_as_its_release() {
    let farm = Farm::new("gf-targets");
    farm.group("alpha", &["db"], None);
    farm.host("a1", "alpha");
    let body = serde_json::json!({"service": "db", "version": "2.0"});
    farm.world
        .coordinator()
        .set_desired_in(
            &crate::service::ci_calls::Setter::Operator("operator"),
            body.to_string().as_bytes(),
            &crate::write_record::WriteAsk::default(),
            Some(crate::releases_tests::targets("db")),
            "alpha",
        )
        .await
        .expect("set with targets");
    assert_eq!(farm.tell("a1", &[("db", "1.0")]), NOTHING);
    let mut asked = request("alpha", "2.0", fleet_target("db"));
    asked.service = "db".to_owned();

    let answer = farm
        .world
        .coordinator()
        .force(&operator(), asked)
        .expect("a force of the release");

    assert_eq!(
        answer
            .skipped
            .iter()
            .map(|host| (host.node_id.as_str(), host.reason.as_str()))
            .collect::<Vec<_>>(),
        [("a1", "installer_dropped")]
    );
    let marks = farm.world.ops().group_marks(&group("alpha"), "db");
    assert_eq!(marks.force.map(|force| force.version), Some(version("2.0")));
}

/// Proves: a force back to a version an earlier force left marks it good again, audited
/// `unmark_bad` naming the group, and the group's force is the later one with a generation of its
/// own.
///
/// Catches: a version marked bad by a force that no later force can order again.
#[tokio::test]
async fn a_force_back_to_the_version_a_force_left_unmarks_it() {
    let farm = farm("gf-unmark").await;
    force(&farm, "alpha", "1.0", fleet_target("web")).expect("a force down");
    assert_eq!(
        farm.world
            .ops()
            .services
            .get("web")
            .map(|marks| marks.bad.clone()),
        Some(vec![version("2.0")])
    );

    let back = force(&farm, "alpha", "2.0", fleet_target("web")).expect("a force up");

    let entries: Vec<_> = back
        .entries
        .iter()
        .map(|entry| (entry.action.as_str(), entry.host_group.as_deref()))
        .collect();
    assert_eq!(
        entries,
        [("force", Some("alpha")), ("unmark_bad", Some("alpha"))]
    );
    let ops = farm.world.ops();
    assert_eq!(
        ops.services.get("web").map(|marks| marks.bad.clone()),
        Some(vec![])
    );
    assert_eq!(
        ops.group_marks(&group("alpha"), "web")
            .force
            .map(|force| force.generation),
        Some(2)
    );
}
