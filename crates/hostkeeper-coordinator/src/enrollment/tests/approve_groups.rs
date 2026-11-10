//! `Enrollment.Approve` by host group (`docs/design/hostgroups.md` sections 1.2 and 7.2): the
//! group an approval names places its host, the refusals of a coordinator that holds groups and
//! of one that holds none, a `replace` that must keep the group of the host it takes over, and a
//! coordinator without enrollment.

use hostkeeper_enrollment::enroll::{Membership, RecordedApproval, RequestState};
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::operator::v1::{
    ApproveRequest, EnrollmentRefusal, GetHostRequest,
};

use super::support::{HostKey, NOW, enroll_request, host_id};
use crate::enrollment::token_of;
use crate::host_groups_support::{SELF, World, coordinator_on, operator_entry};
use crate::marks_support::operator;
use crate::store::Fake;
use crate::testutil::Rig;

/// The reason of the approvals of the tests.
const WHY: &str = "new rack";

/// Posts a request of a new key from `machine`: the key, whose request is pending.
fn post(world: &World, machine: &str) -> HostKey {
    let key = HostKey::new();
    world
        .enrollment
        .enrollment
        .enroll(enroll_request(&key, machine))
        .expect("posted");
    key
}

/// The approval of `key`'s request as `host`, naming `role`, `host_group` and `services`.
fn ask(
    key: &HostKey,
    host: &str,
    role: &str,
    host_group: &str,
    services: &[&str],
) -> ApproveRequest {
    ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: role.to_owned(),
        services: services.iter().map(|s| (*s).to_owned()).collect(),
        replace: false,
        reason: WHY.to_owned(),
        host_group: host_group.to_owned(),
    }
}

/// `Enrollment.Approve` of `request` as the operator.
fn approve(world: &World, request: ApproveRequest) -> Result<String, EnrollmentRefusal> {
    world
        .coordinator()
        .enrollment_approve(&operator(), request)
        .map(|answer| answer.host_id)
}

/// The reason of the audit entry of an approval of `key` from `machine` into `group`.
fn group_reason(key: &HostKey, machine: &str, group: &str) -> String {
    format!(
        "request {}; group {group}; claims machine_id={machine}; {WHY}",
        key.key_hash()
    )
}

/// What a refused approval must leave as it was, but for its audit: the committed state, the ops
/// state with its audit emptied, and every host's group.
fn unchanged(world: &World) -> impl PartialEq + std::fmt::Debug {
    let (desired, mut ops, memberships) = world.state();
    ops.audit.clear();
    (desired, ops, memberships)
}

/// Whether the request of `key` is still pending and no approval or host row is written.
fn still_pending(world: &World, key: &HostKey, host: &str) {
    let store = world.enrollment.enrollment.store();
    assert_eq!(
        store
            .request(&key.key_hash())
            .expect("read")
            .expect("held")
            .state,
        RequestState::Pending
    );
    assert!(store.recorded_approvals().expect("read").is_empty());
    assert_eq!(store.host_role(&host_id(host)).expect("read"), None);
}

/// Proves: an approval that names `host_group` puts the host in it: the host's membership is the
/// group (not moving), its role in the store is the group's name, the recorded approval holds
/// the group and no services, the audit entry is `enroll_approve` by the operator about the host
/// with `host_group` web and the reason `request <id>; group web; claims ...; <reason>`, no
/// `assign` entry is written and the ops state holds no assignment, and `Hosts.Get` shows the
/// group as both `role` and `host_group`.
///
/// Catches: a host placed with no group, a group approval that also assigns services, or an
/// audit entry that names a role and services instead of the group.
#[test]
fn an_approval_naming_a_host_group_places_the_host_in_it() {
    let world = World::new("approve-group-places");
    world.put("web", &[SELF, "web"], None).expect("put");
    let key = post(&world, "machine-1");
    let audited = world.audit().len();

    let approved = approve(&world, ask(&key, "w1", "", "web", &[]));

    assert_eq!(approved, Ok("w1".to_owned()));
    assert_eq!(
        world.membership("w1"),
        Membership {
            host_group: "web".to_owned(),
            moving_from: None
        }
    );
    let store = world.enrollment.enrollment.store();
    assert_eq!(
        store.host_role(&host_id("w1")).expect("read"),
        Some(Role::parse("web").expect("a role"))
    );
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(
        recorded,
        [RecordedApproval {
            request_id: key.key_hash(),
            host_id: "w1".to_owned(),
            role: "web".to_owned(),
            host_group: "web".to_owned(),
            services: Vec::new(),
            replace: false,
            approver: "operator".to_owned(),
            approved_unix: NOW,
            reason: WHY.to_owned(),
            claims: recorded[0].claims.clone(),
            mac_nonce: false,
        }]
    );
    assert_eq!(recorded[0].claims.machine_id, "machine-1");
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve",
            Some("web"),
            Some("w1"),
            &group_reason(&key, "machine-1", "web"),
        )]
    );
    assert!(world.ops().assignments.is_empty());

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
        ("web".to_owned(), "web".to_owned())
    );
}

/// Proves: a role beside no `host_group` is read as the group's name, and a role equal to the
/// `host_group` is accepted: both place the host in the group, as the audit entry (`group web`)
/// and the host's membership show.
///
/// Catches: a client that still names a role losing its placement, or an equal role refused
/// `invalid_role`.
#[test]
fn a_role_is_read_as_the_group_and_an_equal_role_is_accepted() {
    let world = World::new("approve-group-role");
    world.put("web", &[SELF, "web"], None).expect("put");
    for (host, role, host_group) in [("w1", "web", ""), ("w2", "web", "web")] {
        let key = post(&world, &format!("machine-{host}"));
        let audited = world.audit().len();

        let approved = approve(&world, ask(&key, host, role, host_group, &[]));

        assert_eq!(approved, Ok(host.to_owned()));
        assert_eq!(
            world.membership(host),
            Membership {
                host_group: "web".to_owned(),
                moving_from: None
            }
        );
        assert_eq!(
            world.audit_after(audited),
            vec![operator_entry(
                audited as u64 + 1,
                "enroll_approve",
                Some("web"),
                Some(host),
                &group_reason(&key, &format!("machine-{host}"), "web"),
            )]
        );
    }
}

/// Proves: each approval a coordinator that holds groups cannot place is refused with its own
/// token, audited `enroll_approve_refused` with the reason `<token>; request <id>` and no host,
/// and changes nothing else: the request is still pending, no approval, host role or membership
/// is written, and the committed state and ops state are as they were: services named
/// (`services_retired`), a group not held or a name that breaks the grammar (`unknown_host_group`),
/// a role that names no held group (`unknown_host_group`), a role that breaks the grammar or
/// differs from the `host_group` (`invalid_role`), and neither named (`invalid_role`).
///
/// Catches: a refusal given another field's token, an approval written before the group is
/// checked, or a refusal that goes unaudited.
#[test]
fn each_approval_a_group_coordinator_cannot_place_is_refused_and_audited() {
    let world = World::new("approve-group-refusals");
    world.put("web", &[SELF, "web"], None).expect("put");
    let key = post(&world, "machine-1");
    let cases = [
        (
            ask(&key, "w1", "", "web", &["web"]),
            EnrollmentRefusal::ServicesRetired,
        ),
        (
            ask(&key, "w1", "web", "", &["web"]),
            EnrollmentRefusal::ServicesRetired,
        ),
        (
            ask(&key, "w1", "", "db", &[]),
            EnrollmentRefusal::UnknownHostGroup,
        ),
        (
            ask(&key, "w1", "", "Web", &[]),
            EnrollmentRefusal::UnknownHostGroup,
        ),
        (
            ask(&key, "w1", "", "-web", &[]),
            EnrollmentRefusal::UnknownHostGroup,
        ),
        (
            ask(&key, "w1", "web", "web!", &[]),
            EnrollmentRefusal::InvalidRole,
        ),
        (
            ask(&key, "w1", "db", "", &[]),
            EnrollmentRefusal::UnknownHostGroup,
        ),
        (
            ask(&key, "w1", "Web", "", &[]),
            EnrollmentRefusal::InvalidRole,
        ),
        (
            ask(&key, "w1", "1web", "", &[]),
            EnrollmentRefusal::InvalidRole,
        ),
        (
            ask(&key, "w1", "db", "web", &[]),
            EnrollmentRefusal::InvalidRole,
        ),
        (ask(&key, "w1", "", "", &[]), EnrollmentRefusal::InvalidRole),
    ];
    let before = unchanged(&world);
    let audited = world.audit().len();

    let mut wanted = Vec::new();
    for (request, refusal) in cases {
        assert_eq!(approve(&world, request), Err(refusal));
        wanted.push(operator_entry(
            (audited + wanted.len()) as u64 + 1,
            "enroll_approve_refused",
            None,
            None,
            &format!("{}; request {}", token_of(refusal), key.key_hash()),
        ));
    }

    assert_eq!(world.audit_after(audited), wanted);
    assert_eq!(unchanged(&world), before);
    still_pending(&world, &key, "w1");
}

/// Proves: a `replace` of a host that is in a group, naming another group, is refused
/// `host_group_differs`, audited with the host named (`host_group_differs; request <id>`), and
/// leaves the host, its membership and its role as they were and the new request pending; naming
/// the host's own group then succeeds, is audited `enroll_replace` with `host_group` web, and
/// keeps the host in web.
///
/// Catches: a replacement that moves a host to another group by the way, or one that loses the
/// host's group.
#[test]
fn a_replace_naming_another_group_is_refused_and_naming_the_same_keeps_it() {
    let world = World::new("approve-group-replace");
    world.put("web", &[SELF, "web"], None).expect("put");
    world.put("db", &[SELF, "db"], None).expect("put");
    let first = post(&world, "machine-1");
    approve(&world, ask(&first, "w1", "", "web", &[])).expect("approved");
    let second = post(&world, "machine-2");
    let replace = |group: &str| ApproveRequest {
        replace: true,
        ..ask(&second, "w1", "", group, &[])
    };
    let before = unchanged(&world);
    let audited = world.audit().len();

    let refused = approve(&world, replace("db"));

    assert_eq!(refused, Err(EnrollmentRefusal::HostGroupDiffers));
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve_refused",
            None,
            Some("w1"),
            &format!("host_group_differs; request {}", second.key_hash()),
        )]
    );
    assert_eq!(unchanged(&world), before);
    let store = world.enrollment.enrollment.store();
    assert_eq!(store.recorded_approvals().expect("read").len(), 1);
    assert_eq!(
        store
            .request(&second.key_hash())
            .expect("read")
            .expect("held")
            .state,
        RequestState::Pending
    );
    let audited = world.audit().len();

    let replaced = approve(&world, replace("web"));

    assert_eq!(replaced, Ok("w1".to_owned()));
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_replace",
            Some("web"),
            Some("w1"),
            &group_reason(&second, "machine-2", "web"),
        )]
    );
    assert_eq!(
        world.membership("w1"),
        Membership {
            host_group: "web".to_owned(),
            moving_from: None
        }
    );
}

/// Proves: a coordinator that holds no group refuses an approval that names a `host_group` as
/// `unknown_host_group`, audited, with the request still pending and nothing written; an
/// approval by role and services is as before groups: the host's role is the role, its services
/// are assigned, and the audit reason is `request <id>; role storage; services web; claims ...`
/// with no group on the entry.
///
/// Catches: group placement leaking into a coordinator that holds no group, or the legacy
/// approval's audit reason changed.
#[test]
fn a_coordinator_holding_no_group_refuses_a_host_group_and_approves_by_role() {
    let world = World::new("approve-group-legacy");
    let key = post(&world, "machine-1");
    let before = unchanged(&world);
    let audited = world.audit().len();

    let refused = approve(&world, ask(&key, "s1", "storage", "web", &["web"]));

    assert_eq!(refused, Err(EnrollmentRefusal::UnknownHostGroup));
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve_refused",
            None,
            None,
            &format!("unknown_host_group; request {}", key.key_hash()),
        )]
    );
    assert_eq!(unchanged(&world), before);
    still_pending(&world, &key, "s1");
    let audited = world.audit().len();

    // The group is refused before the other fields are read, as before groups.
    let malformed = ApproveRequest {
        request_id: "not a key hash".to_owned(),
        ..ask(&key, "s1", "storage", "web", &[])
    };
    assert_eq!(
        approve(&world, malformed),
        Err(EnrollmentRefusal::UnknownHostGroup)
    );
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "enroll_approve_refused",
            None,
            None,
            "unknown_host_group",
        )]
    );
    assert_eq!(unchanged(&world), before);
    let audited = world.audit().len();

    let approved = approve(&world, ask(&key, "s1", "storage", "", &["web"]));

    assert_eq!(approved, Ok("s1".to_owned()));
    let wanted = operator_entry(
        audited as u64 + 1,
        "enroll_approve",
        None,
        Some("s1"),
        &format!(
            "request {}; role storage; services web; claims machine_id=machine-1; {WHY}",
            key.key_hash()
        ),
    );
    assert_eq!(world.audit_after(audited)[0], wanted);
    let store = world.enrollment.enrollment.store();
    assert_eq!(
        store.host_role(&host_id("s1")).expect("read"),
        Some(Role::parse("storage").expect("a role"))
    );
    assert_eq!(store.recorded_approvals().expect("read")[0].host_group, "");
    assert!(world.ops().assignments.contains_key("web"));
}

/// Proves: a coordinator without enrollment answers `Enrollment.Approve` with
/// `unknown_host_group` for a `host_group` when it holds no group; once it holds `web`, naming
/// `web` or naming no group is `unknown_request`, and naming another group is still
/// `unknown_host_group`; none of the calls writes an audit entry.
///
/// Catches: a held group refused as unknown when there is no enrollment to approve in, or an
/// unheld group answered as an unknown request.
#[test]
fn without_enrollment_only_a_group_not_held_is_unknown() {
    let fake = Fake::new();
    let rig = Rig::new("approve-group-none");
    let coordinator = coordinator_on(&fake, &rig.clock, None);
    let key = HostKey::new();
    let call = |host_group: &str| {
        coordinator
            .enrollment_approve(&operator(), ask(&key, "w1", "", host_group, &[]))
            .map(|_| ())
    };
    assert_eq!(call("web"), Err(EnrollmentRefusal::UnknownHostGroup));
    assert_eq!(call(""), Err(EnrollmentRefusal::UnknownRequest));

    let put = crate::host_groups_support::put_of(
        crate::host_groups_support::body("web", &[SELF], None),
        None,
    );
    coordinator.host_groups_put(&operator(), put).expect("put");
    let audited = coordinator.ops().audit_log().expect("the audit log").len();

    assert_eq!(call("web"), Err(EnrollmentRefusal::UnknownRequest));
    assert_eq!(call("db"), Err(EnrollmentRefusal::UnknownHostGroup));
    assert_eq!(call(""), Err(EnrollmentRefusal::UnknownRequest));
    assert_eq!(
        coordinator.ops().audit_log().expect("the audit log").len(),
        audited
    );
}

/// Proves: a host approved into `web` counts as in it at once, before its first poll or
/// heartbeat: `HostGroups.Remove` of `web` is refused `not_empty` and changes nothing, while an
/// empty group beside it is removed.
///
/// Catches: a group removed between a host's approval into it and the host's first poll, which
/// would leave the host placed in a group the coordinator no longer holds.
#[test]
fn a_group_a_host_was_just_approved_into_is_not_removed() {
    let world = World::new("approve-group-remove");
    world.put("web", &[SELF], None).expect("put");
    world.put("spare", &[SELF], None).expect("put");
    let key = post(&world, "machine-1");
    approve(&world, ask(&key, "w1", "", "web", &[])).expect("approved");
    let before = world.state();

    let refused = world
        .coordinator()
        .host_groups_remove(&operator(), crate::host_groups_support::remove_of("web"));

    assert_eq!(
        refused.map(|_| ()),
        Err(hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal::NotEmpty)
    );
    assert_eq!(world.state(), before);
    world
        .coordinator()
        .host_groups_remove(&operator(), crate::host_groups_support::remove_of("spare"))
        .expect("an empty group is removed");
}
