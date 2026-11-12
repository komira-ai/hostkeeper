//! The first install of a package a host group lists and none of its hosts runs yet: the plan
//! orders it on the hosts of the group that are not excluded, the canary first and stage by
//! stage, and `Releases.Status` shows every host of the group with why it is or is not ordered,
//! also while no host runs the package. The tests drive heartbeats of enrolled hosts placed in a
//! group, over transports that carry typed orders.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::json;

use crate::decide::{Gate, Skip, Why};
use crate::group_decision_support::{Farm, policy, stages};
use crate::marks_support::operator;
use crate::orders::Order;
use crate::releases::Targets;
use crate::releases_tests::{entry, list};

/// The hosts of the group, in node-id order.
const HOSTS: [&str; 6] = [
    "node-01", "node-02", "node-03", "node-04", "node-05", "node-06",
];

/// What the hosts run before the package is added: the group's other packages, at their desired
/// versions.
const RUNS: [(&str, &str); 2] = [("js-agent", "0.1.230"), ("hello", "1.0")];

/// A policy with a canary of one host, then 50 and 100 percent, each entered by itself, one host
/// updating at a time, a bake of one beat.
fn canary_first() -> proto::RolloutPolicy {
    policy(|p| {
        p.canary = Some(proto::Canary { count: 1 });
        p.stages = stages(&[50, 100]);
        p.bake_beats = 1;
        p.max_updating = Some(1);
    })
}

/// The group `farm` listing tierstore, js-agent and hello after hostkeeper, with `policy`; its
/// six hosts run js-agent and hello at their desired versions, and an operator excluded
/// `excluded` from tierstore.
async fn farm(name: &str, policy: proto::RolloutPolicy, excluded: &[&str]) -> Farm {
    let farm = Farm::new(name);
    farm.group("farm", &["tierstore", "js-agent", "hello"], Some(policy));
    for host in HOSTS {
        farm.host(host, "farm");
    }
    farm.deploy("", "js-agent", "0.1.230", 2).await;
    farm.deploy("", "hello", "1.0", 3).await;
    for host in HOSTS {
        assert_eq!(farm.order(host, &RUNS), None, "{host} runs what it should");
    }
    for host in excluded {
        farm.world
            .coordinator()
            .exclude(
                &operator(),
                proto::ExcludeRequest {
                    service: "tierstore".to_owned(),
                    node_id: (*host).to_owned(),
                    reason: "adopted by hand later".to_owned(),
                    abandon: false,
                },
            )
            .expect("excluded");
    }
    farm
}

/// The service of an install order, if `order` is one.
fn installs(order: Option<Order>) -> Option<String> {
    match order? {
        Order::Install { service, .. } => Some(service),
        _ => None,
    }
}

/// What `host` runs once it installed tierstore at `at`.
fn with_tierstore(at: &'static str) -> [(&'static str, &'static str); 3] {
    [RUNS[0], RUNS[1], ("tierstore", at)]
}

/// The status of tierstore in the group `farm`, as `Releases.Status` answers it.
fn status(farm: &Farm) -> proto::ServiceStatus {
    let status = farm.world.coordinator().status_now();
    crate::grpc::report::status(&status, "tierstore")
        .services
        .into_iter()
        .find(|one| one.host_group == "farm")
        .expect("the group farm lists tierstore")
}

/// One host row of a status: node id, membership, gate reason, stage, excluded reason and whether
/// the row carries the host's beat of the service.
type Row<'a> = (
    &'a str,
    proto::Membership,
    proto::GateReason,
    Option<u32>,
    Option<&'a str>,
    bool,
);

/// The rows of the hosts of `status`, in its order.
fn rows(status: &proto::ServiceStatus) -> Vec<Row<'_>> {
    status
        .hosts
        .iter()
        .map(|host| {
            let view = host.view.as_ref().expect("a view");
            (
                host.node_id.as_str(),
                view.membership(),
                view.gate_reason(),
                view.stage,
                host.excluded_reason.as_deref(),
                host.beat.is_some(),
            )
        })
        .collect()
}

/// The `rollout_advance` entries of tierstore in the audit, by their reason.
fn advances(farm: &Farm) -> Vec<String> {
    farm.world
        .coordinator()
        .ops()
        .state()
        .audit
        .iter()
        .filter(|e| e.action == "rollout_advance" && e.service.as_deref() == Some("tierstore"))
        .map(|e| e.reason.clone().unwrap_or_default())
        .collect()
}

const EXCLUDED: proto::Membership = proto::Membership::Excluded;
const MISSING: proto::Membership = proto::Membership::Missing;
const PRESENT: proto::Membership = proto::Membership::Present;
const NOT_A_MEMBER: proto::GateReason = proto::GateReason::NotAMember;
const REASON: Option<&str> = Some("adopted by hand later");

/// Proves: a package a group adds that none of its hosts runs, with half of the hosts excluded
/// from it, is installed on the included hosts only, the canary first: the first included host
/// in node-id order is ordered the install while the next waits for the canary's stage
/// (`stage_wait`); once the canary runs it and has baked the plan enters its next stage with a
/// `rollout_advance` by the coordinator and the next included host is ordered; the excluded hosts
/// are never ordered (`not_a_member`) and take no place of a stage.
///
/// Catches: a plan that counts or orders only the hosts that already report the package, the
/// excluded hosts counted against a stage's size, or a canary that is not first.
#[tokio::test]
async fn a_package_new_to_a_group_is_installed_on_the_included_hosts_canary_first() {
    let excluded = ["node-01", "node-03", "node-05"];
    let farm = farm("fi-canary", canary_first(), &excluded).await;
    farm.deploy("", "tierstore", "0.3.96", 1).await;

    assert_eq!(
        installs(farm.order("node-02", &RUNS)).as_deref(),
        Some("tierstore")
    );
    assert_eq!(farm.order("node-04", &RUNS), None);
    assert_eq!(
        farm.why("node-04", "tierstore"),
        Some(Why::Gate(Gate::StageWait))
    );
    for host in excluded {
        assert_eq!(farm.order(host, &RUNS), None, "{host} is excluded");
        assert_eq!(
            farm.why(host, "tierstore"),
            Some(Why::Skip(Skip::NotAMember))
        );
    }
    let plan = status(&farm).plan.expect("a plan");
    assert_eq!(
        (plan.eligible, plan.current, plan.stages.len()),
        (3, 1, 3),
        "the canary's stage of the three included hosts: {plan:?}"
    );
    assert!(
        advances(&farm).is_empty(),
        "the canary's stage is entered by itself"
    );

    // The canary runs the package and has baked: the next stage is entered.
    assert_eq!(farm.order("node-02", &with_tierstore("0.3.96")), None);
    assert_eq!(
        advances(&farm),
        ["tierstore 0.3.96: stage 2 of 3 (2 hosts)"],
        "the coordinator enters the next stage"
    );
    assert_eq!(
        installs(farm.order("node-04", &RUNS)).as_deref(),
        Some("tierstore")
    );
    assert_eq!(farm.order("node-06", &RUNS), None);
    assert_eq!(
        farm.why("node-06", "tierstore"),
        Some(Why::Gate(Gate::StageWait))
    );

    assert_eq!(farm.order("node-04", &with_tierstore("0.3.96")), None);
    assert_eq!(
        installs(farm.order("node-06", &RUNS)).as_deref(),
        Some("tierstore")
    );
    for host in excluded {
        assert_eq!(farm.order(host, &RUNS), None, "{host} is still excluded");
    }
}

/// Targets of `service` whose every platform lists one `hostkeeper-archive` entry alone: a
/// release the coordinator picks no host an entry of.
fn archive_only(service: &str) -> Targets {
    ["linux-x86_64", "linux-arm64", "macos-arm64"]
        .into_iter()
        .map(|platform| {
            let entries = vec![entry(service, "hostkeeper-archive", '7', 1_000)];
            (platform.to_owned(), list(entries))
        })
        .collect()
}

/// Proves: `Releases.Status` of a package that no host of its group runs shows every host of the
/// group, in node-id order, with why it is not ordered: a version whose release has no entry a
/// host can install (one `hostkeeper-archive` entry per platform) shows the included host
/// `missing` with `no_build_for_host` and the excluded hosts `excluded` with `not_a_member` and
/// the operator's reason, while the plan has no eligible host and no stage. No row carries a beat.
///
/// Catches: a status with no hosts for a package no host runs yet, which says nothing of why no
/// host is ordered.
#[tokio::test]
async fn the_status_of_a_package_no_host_runs_shows_every_host_and_why_it_waits() {
    let excluded = ["node-01", "node-02", "node-03", "node-04", "node-06"];
    let farm = farm("fi-archive", canary_first(), &excluded).await;
    farm.world.coordinator().keep_komira();
    let body = json!({"service": "tierstore", "version": "0.3.96"});
    farm.set("", &body, Some(archive_only("tierstore"))).await;

    assert_eq!(farm.order("node-05", &RUNS), None);
    assert_eq!(
        farm.why("node-05", "tierstore"),
        Some(Why::Skip(Skip::NoBuildForHost))
    );
    let status = status(&farm);
    let plan = status.plan.as_ref().expect("a plan");
    assert_eq!(
        (
            plan.eligible,
            plan.current,
            plan.stages.len(),
            plan.waiting()
        ),
        (0, 0, 0, proto::PlanWait::None)
    );
    let no_build = proto::GateReason::NoBuildForHost;
    assert_eq!(
        rows(&status),
        [
            ("node-01", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
            ("node-02", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
            ("node-03", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
            ("node-04", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
            ("node-05", MISSING, no_build, None, None, false),
            ("node-06", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
        ]
    );
}

/// Proves: the status of a package some hosts of its group run and others are to install shows
/// both, in node-id order: a host that runs it with its beat and `present`; a host ordered the
/// install, before it reports, `missing` with no gate reason (`none`: it is told the same again)
/// and its stage; a host waiting for its stage `missing` with `stage_wait` and its stage; an
/// excluded host `excluded`. A host of another group, even one excluded from the package, and a
/// host whose daemon takes no installs are not shown.
///
/// Catches: a host shown twice (once heard, once not), hosts of another group in the rows, or a
/// row for a host the package could never be ordered to.
#[tokio::test]
async fn the_status_shows_hosts_that_run_the_package_and_hosts_that_do_not_once_each() {
    let farm = farm("fi-mixed", canary_first(), &["node-01"]).await;
    farm.group("other", &["tierstore"], None);
    farm.host("other-1", "other");
    farm.world
        .coordinator()
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: "tierstore".to_owned(),
                node_id: "other-1".to_owned(),
                reason: "another group".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    // A daemon that lists its services and takes no installs is not placed the package.
    assert_eq!(
        farm.tell("node-06", &RUNS),
        hostkeeper_wire::wire::ServiceReply::Nothing
    );
    farm.deploy("farm", "tierstore", "0.3.96", 1).await;
    // node-02 is the canary and runs the package; node-03 is ordered the next stage's install.
    assert_eq!(
        installs(farm.order("node-02", &RUNS)).as_deref(),
        Some("tierstore")
    );
    assert_eq!(farm.order("node-02", &with_tierstore("0.3.96")), None);
    assert_eq!(
        installs(farm.order("node-03", &RUNS)).as_deref(),
        Some("tierstore")
    );

    let status = status(&farm);
    let none = proto::GateReason::None;
    let stage_wait = proto::GateReason::StageWait;
    let up_to_date = proto::GateReason::UpToDate;
    assert_eq!(
        rows(&status),
        [
            ("node-01", EXCLUDED, NOT_A_MEMBER, None, REASON, false),
            ("node-02", PRESENT, up_to_date, Some(1), None, true),
            ("node-03", MISSING, none, Some(2), None, false),
            ("node-04", MISSING, stage_wait, Some(3), None, false),
            ("node-05", MISSING, stage_wait, Some(3), None, false),
        ]
    );
}
