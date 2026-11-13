//! `Enrollment.Approve` that names no host group, and none or an empty reason, from a coordinator
//! that holds groups: the host is enrolled in no specific group, so it runs the packages of `all`
//! alone until `HostGroups.Move` puts it in one; the approval is audited with the approver and the group, and
//! an empty reason is fine.

use hostkeeper_enrollment::enroll::{Membership, RecordedApproval};
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::{ApproveRequest, GetHostRequest};

use super::approve_groups::{WHY, approve, ask, post};
use super::support::{NOW, host_id};
use crate::facts::FEATURE;
use crate::group_decision_support::Farm;
use crate::host_groups_support::{SELF, operator_entry};
use crate::marks_support::{operator, who};
use crate::orders::Order;

/// The group the tests list one package in.
const GROUP: &str = "web";

/// A farm whose group `web` lists tierstore, wanted at 1.0.
fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group(GROUP, &["tierstore"], None);
    farm.want(GROUP, "tierstore", "1.0");
    farm
}

/// Proves: an approval that names neither a `host_group` nor a role is accepted: the host's
/// membership is no group (not moving), its role in the store is the placeholder, the recorded
/// approval holds no group and no services, the audit entry is `enroll_approve` by the operator
/// about the host with no `host_group` and the reason `request <id>; no group; claims ...;
/// <reason>`, and `Hosts.Get` shows the host with no role and no group.
///
/// Catches: an approval with no group refused `invalid_role`, a host put in a group nobody named,
/// or a placeholder role shown to the operator.
#[test]
fn an_approval_naming_no_group_enrolls_the_host_in_none() {
    let farm = farm("approve-ungrouped-places");
    let world = &farm.world;
    let key = post(world, "machine-1");
    let audited = world.audit().len();

    let approved = approve(world, ask(&key, "w1", "", "", &[]));

    assert_eq!(approved, Ok("w1".to_owned()));
    assert_eq!(
        world.membership("w1"),
        Membership {
            host_group: String::new(),
            moving_from: None
        }
    );
    let store = world.enrollment.enrollment.store();
    assert_eq!(
        store.host_role(&host_id("w1")).expect("read"),
        Some(Role::parse("ungrouped").expect("a role"))
    );
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(
        recorded,
        [RecordedApproval {
            request_id: key.key_hash(),
            host_id: "w1".to_owned(),
            role: "ungrouped".to_owned(),
            host_group: String::new(),
            services: Vec::new(),
            replace: false,
            approver: "operator".to_owned(),
            approved_unix: NOW,
            reason: WHY.to_owned(),
            claims: recorded[0].claims.clone(),
            mac_nonce: false,
        }]
    );
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve",
            None,
            Some("w1"),
            &format!(
                "request {}; no group; claims machine_id=machine-1; {WHY}",
                key.key_hash()
            ),
        )]
    );
    world.beat("w1", &[SELF]);
    let shown = world
        .coordinator()
        .get_host(
            &operator(),
            &GetHostRequest {
                node_id: "w1".to_owned(),
            },
        )
        .expect("shown")
        .host
        .expect("a host");
    assert_eq!(
        (shown.role, shown.host_group),
        (String::new(), String::new())
    );
}

/// Proves: a host approved with no group is in the built-in group `all` alone: it is ordered the
/// package `all` lists and none of the group `web`'s, while a host moved into `web` is ordered
/// `all`'s package and then `web`'s; the approved host is ordered `web`'s once it is moved.
///
/// Catches: a host with no group that runs nothing, or one ordered the packages of a group it was
/// never put in, or a group that orders nothing to its members.
#[tokio::test]
async fn a_host_approved_with_no_group_runs_the_packages_of_all_until_it_is_moved() {
    let farm = farm("approve-ungrouped-orders");
    let world = &farm.world;
    world.hold_all();
    world
        .put("all", &[SELF, "base"], Some(1))
        .expect("all lists base");
    farm.deploy("all", "base", "1.0", 1).await;
    let key = post(world, "machine-1");
    approve(world, ask(&key, "w1", "", "", &[])).expect("approved");

    let installs = |host: &str, runs: &[(&str, &str)]| match farm.order(host, runs) {
        Some(Order::Install { service, .. }) => Some(service),
        _ => None,
    };
    assert_eq!(installs("w1", &[]).as_deref(), Some("base"));
    assert_eq!(installs("w1", &[("base", "1.0")]), None);
    assert_eq!(farm.order("w1", &[("base", "1.0")]), None);

    world.move_to("w1", GROUP).expect("moved");
    farm.world.enroll("w2");
    farm.world.move_to("w2", GROUP).expect("moved");
    assert_eq!(world.membership("w1").host_group, GROUP);
    assert_eq!(installs("w2", &[]).as_deref(), Some("base"));
    assert_eq!(
        installs("w2", &[("base", "1.0")]).as_deref(),
        Some("tierstore"),
        "a host in the group is ordered the install after all's"
    );
}

/// Proves: an approval with an empty reason is accepted, records an empty reason, and is audited
/// with the approver and the group and no trailing reason: `request <id>; group web; claims ...`.
///
/// Catches: an empty reason refused `invalid_request`, or an audit reason that ends with a
/// dangling separator.
#[test]
fn an_approval_with_no_reason_is_audited_with_the_approver_and_the_group() {
    let farm = farm("approve-no-reason");
    let world = &farm.world;
    let key = post(world, "machine-1");
    let audited = world.audit().len();
    let request = ApproveRequest {
        reason: String::new(),
        ..ask(&key, "w1", "", GROUP, &[])
    };

    let approved = approve(world, request);

    assert_eq!(approved, Ok("w1".to_owned()));
    let store = world.enrollment.enrollment.store();
    assert_eq!(
        store.recorded_approvals().expect("read")[0].reason,
        String::new()
    );
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve",
            Some(GROUP),
            Some("w1"),
            &format!(
                "request {}; group {GROUP}; claims machine_id=machine-1",
                key.key_hash()
            ),
        )]
    );
}

/// Proves: a `replace` that names no group of a host that is in one keeps the host in that group
/// (`HostGroups.Move` is the only way out), and one that names the host's own group does the
/// same.
///
/// Catches: a replacement with no group silently taking the host out of its group.
#[test]
fn a_replace_naming_no_group_keeps_the_hosts_group() {
    let farm = farm("approve-ungrouped-replace");
    let world = &farm.world;
    let first = post(world, "machine-1");
    approve(world, ask(&first, "w1", "", GROUP, &[])).expect("approved");
    let second = post(world, "machine-2");

    let replaced = approve(
        world,
        ApproveRequest {
            replace: true,
            ..ask(&second, "w1", "", "", &[])
        },
    );

    assert_eq!(replaced, Ok("w1".to_owned()));
    assert_eq!(
        world.membership("w1"),
        Membership {
            host_group: GROUP.to_owned(),
            moving_from: None
        }
    );
}

/// Proves: the facts a daemon of a host approved with no group is sent name no group (an empty
/// `host_group`), not the placeholder role its record holds.
///
/// Catches: the placeholder role sent to a daemon as the name of its group.
#[test]
fn the_facts_of_a_host_approved_with_no_group_name_none() {
    let farm = farm("approve-ungrouped-facts");
    let world = &farm.world;
    let key = post(world, "machine-1");
    approve(world, ask(&key, "w1", "", "", &[])).expect("approved");
    let said = agent::HeartbeatRequest {
        features: vec![FEATURE.to_owned()],
        ..agent::HeartbeatRequest::default()
    };

    let facts = world
        .coordinator()
        .host_facts(&who("w1", "host"), &said)
        .expect("facts");

    assert_eq!(facts.host_group, "");
}
