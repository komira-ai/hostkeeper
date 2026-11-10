//! `HostGroups.Move` ([`crate::host_move`]): the host's record, the uninstalls ordered and the
//! intents cancelled, the audit entry, the host's node in the fleet, a second move, and each
//! refusal and failure with nothing changed.

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_enrollment::enroll::records::Records as _;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups_support::{
    REASON, SELF, World, body, cancelled, coordinator_on, move_of, operator_entry, put_of,
};
use crate::marks_support::operator;
use crate::operator::{Caller, entry_proto};
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::{Rig, id};

fn membership(group: &str, moving_from: Option<&str>) -> Membership {
    Membership {
        host_group: group.to_owned(),
        moving_from: moving_from.map(str::to_owned),
    }
}

/// Proves: a host in no group moves into one: the answer names no group it came from, the audit
/// entry is `host_move` about the host and the new group with the reason `into alpha: ...`, the
/// host's record names the new group and the empty group it moves from (so it is moving), and the
/// rest of the record is as it was, its `facts_generation` field included (the generation of a
/// host's facts is the number of the audit entry that raised it, `moving_facts_tests.rs`); no
/// intent is written for a host with no group to leave.
///
/// Catches: a record whose move is not marked, or a record field written that nothing reads.
#[test]
fn a_host_in_no_group_moves_into_its_first() {
    let world = World::new("hg-move-first");
    world.enroll("d1");
    world.put("alpha", &[SELF, "svc"], None).expect("put");
    world.beat("d1", &[SELF, "svc"]);
    let id = hostkeeper_enrollment::names::HostId::parse("d1").expect("a host id");
    let record = world.records.host(&id).expect("read").expect("a record");
    let audited = world.audit().len() as u64;

    let moved = world.move_to("d1", "alpha").expect("moved");

    let wanted = operator_entry(
        audited + 1,
        "host_move",
        Some("alpha"),
        Some("d1"),
        &format!("into alpha: {REASON}"),
    );
    assert_eq!(moved.entry, Some(entry_proto(&wanted)));
    assert_eq!(moved.from_host_group, "");
    assert_eq!(world.audit_after(audited as usize), vec![wanted]);
    assert_eq!(world.membership("d1"), membership("alpha", Some("")));
    let after = world.records.host(&id).expect("read").expect("a record");
    assert_eq!(
        after,
        hostkeeper_enrollment::enroll::records::pb::Host {
            host_group: "alpha".to_owned(),
            moving_from: Some(String::new()),
            ..record
        }
    );
    assert_eq!(world.intents(), []);
}

/// Proves: a host moved from one group to another is ordered to uninstall the packages of the old
/// group it listed at its last heartbeat that the new group does not list, the one listed last
/// first, none of a package both groups list nor of one it ran that the old group never listed;
/// the entry says `from alpha to beta`, the answer names `alpha`, and the host's record names
/// `alpha` as the group it moves from; the host's node in the fleet is placed in `beta`.
///
/// Catches: a package both groups list uninstalled and installed again, a hand-installed package
/// ordered off the host, or a config package uninstalled after the service it configures.
#[test]
fn a_move_orders_the_uninstall_of_what_the_new_group_does_not_list() {
    let world = World::new("hg-move-uninstalls");
    world.enroll("d1");
    world
        .put("alpha", &[SELF, "svc", "cfg", "shared"], None)
        .expect("put");
    world
        .put("beta", &[SELF, "shared", "fresh"], None)
        .expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.beat("d1", &[SELF, "svc", "cfg", "shared", "extra"]);
    let audited = world.audit().len();

    let moved = world.move_to("d1", "beta").expect("moved");

    assert_eq!(moved.from_host_group, "alpha");
    assert_eq!(
        world.audit_after(audited),
        vec![operator_entry(
            audited as u64 + 1,
            "host_move",
            Some("beta"),
            Some("d1"),
            &format!("from alpha to beta: {REASON}"),
        )]
    );
    assert_eq!(world.membership("d1"), membership("beta", Some("alpha")));
    assert_eq!(
        world.intents(),
        [
            ("cfg".to_owned(), "d1".to_owned(), 1),
            ("svc".to_owned(), "d1".to_owned(), 2),
        ]
    );
    let fleet = world.coordinator().lock_fleet();
    assert_eq!(fleet.node(&id("d1")).expect("a node").role, "beta");
}

/// Proves: a second move replaces the target: the host's record names the group it was moved
/// into by the first as the group it moves from, the intents the first wrote for packages the
/// third group lists are cancelled (one `uninstall_cancelled` entry each, after the move's), the
/// intents for packages of the second group the third does not list are ordered above every
/// generation given, and the ones for packages neither lists stay.
///
/// Catches: a host moved on that still uninstalls what its new group lists.
#[test]
fn a_second_move_cancels_the_intents_of_what_the_new_group_lists() {
    let world = World::new("hg-move-second");
    world.enroll("d1");
    world
        .put("alpha", &[SELF, "svc", "cfg", "shared"], None)
        .expect("put");
    world
        .put("beta", &[SELF, "shared", "fresh"], None)
        .expect("put");
    world.put("gamma", &[SELF, "svc"], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.beat("d1", &[SELF, "svc", "cfg", "shared", "fresh"]);
    world.move_to("d1", "beta").expect("moved");
    assert_eq!(
        world.intents(),
        [
            ("cfg".to_owned(), "d1".to_owned(), 1),
            ("svc".to_owned(), "d1".to_owned(), 2),
        ]
    );
    let audited = world.audit().len() as u64;

    let moved = world.move_to("d1", "gamma").expect("moved");

    assert_eq!(moved.from_host_group, "beta");
    assert_eq!(world.membership("d1"), membership("gamma", Some("beta")));
    assert_eq!(
        world.audit_after(audited as usize),
        vec![
            operator_entry(
                audited + 1,
                "host_move",
                Some("gamma"),
                Some("d1"),
                &format!("from beta to gamma: {REASON}"),
            ),
            cancelled(audited + 2, "gamma", "svc", "d1"),
        ]
    );
    assert_eq!(
        world.intents(),
        [
            ("cfg".to_owned(), "d1".to_owned(), 1),
            ("fresh".to_owned(), "d1".to_owned(), 3),
            ("shared".to_owned(), "d1".to_owned(), 4),
        ]
    );
}

/// Proves: a host the fleet has not heard from is moved all the same (its record changes, no
/// intent is written, no node is made), and a host that is moved while another is not heard from
/// leaves the other's group alone.
///
/// Catches: a move that needs a heartbeat, or one that makes a node of a host that never spoke.
#[test]
fn a_host_never_heard_from_is_moved_without_a_node() {
    let world = World::new("hg-move-unheard");
    world.enroll("d1");
    world.enroll("d2");
    world.put("alpha", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    assert_eq!(world.membership("d1"), membership("alpha", Some("")));
    assert_eq!(world.membership("d2"), membership("", None));
    assert!(world.coordinator().lock_fleet().node(&id("d1")).is_none());
}

/// Proves: every refusal of `Move` is its token and changes nothing (the store, the audit and
/// every host's group are as they were): CI's token `forbidden` and none `unauthorized`; a node id
/// or a group name that breaks its grammar and a reason that is empty, too long or holds a
/// control character `invalid_request`; a host never enrolled, a host denied and any host of a
/// coordinator without enrollment `unknown_host`; a group not held `unknown_host_group`; the group
/// the host is in `same_host_group`.
///
/// Catches: a refusal that writes the host's record, or reports another token.
#[test]
fn every_refusal_of_a_move_changes_nothing() {
    let world = World::new("hg-move-refusals");
    for host in ["d1", "d2"] {
        world.enroll(host);
    }
    world.put("alpha", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world
        .enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("d2").expect("a host id"))
        .expect("denied");
    let before = world.state();
    let coordinator = world.coordinator();
    let long = "x".repeat(257);
    let refused = |caller: &Caller, request: proto::MoveHostRequest| {
        coordinator
            .host_groups_move(caller, request)
            .expect_err("refused")
    };
    assert_eq!(
        refused(&Caller::Ci, move_of("d1", "alpha")),
        Refusal::Forbidden
    );
    assert_eq!(
        refused(&Caller::Anonymous, move_of("d1", "alpha")),
        Refusal::Unauthorized
    );
    for request in [
        move_of("", "alpha"),
        move_of("Bad_Id", "alpha"),
        move_of("d1", ""),
        move_of("d1", "Alpha"),
        proto::MoveHostRequest {
            reason: String::new(),
            ..move_of("d1", "alpha")
        },
        proto::MoveHostRequest {
            reason: long,
            ..move_of("d1", "alpha")
        },
        proto::MoveHostRequest {
            reason: "a\nb".to_owned(),
            ..move_of("d1", "alpha")
        },
    ] {
        assert_eq!(
            refused(&operator(), request.clone()),
            Refusal::InvalidRequest,
            "{request:?}"
        );
    }
    for host in ["ghost", "d2"] {
        assert_eq!(
            refused(&operator(), move_of(host, "alpha")),
            Refusal::UnknownHost,
            "{host}"
        );
    }
    assert_eq!(
        refused(&operator(), move_of("d1", "nowhere")),
        Refusal::UnknownHostGroup
    );
    assert_eq!(
        refused(&operator(), move_of("d1", "alpha")),
        Refusal::SameHostGroup
    );
    assert_eq!(world.state(), before);

    let fake = Fake::new();
    let mut rig = Rig::new("hg-move-no-enrollment");
    rig.coordinator = coordinator_on(&fake, &rig.clock, None);
    rig.at(2_000);
    rig.coordinator
        .host_groups_put(&operator(), put_of(body("alpha", &[SELF], None), None))
        .expect("put");
    assert_eq!(
        rig.coordinator
            .host_groups_move(&operator(), move_of("d1", "alpha"))
            .expect_err("no host is enrolled"),
        Refusal::UnknownHost
    );
}

/// Proves: a commit that fails after the host's record was written is `store_failed`, and one
/// whose audit line cannot be kept `audit_unavailable`, and either puts the host's old record
/// back: the group and the move it had, the intents and the audit as they were; once the store
/// takes commits again the same move lands.
///
/// Catches: a host left in a group no audit line says it was moved to.
#[test]
fn a_commit_that_fails_puts_the_hosts_record_back() {
    let world = World::new("hg-move-commit-fails");
    world.enroll("d1");
    world.put("alpha", &[SELF, "svc"], None).expect("put");
    world.put("beta", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    // The host is behind alpha's version of `svc`, so its move is not over at its heartbeat.
    world.seed_ordered("alpha", "svc", "2.0");
    world.beat("d1", &[SELF, "svc"]);
    let before = world.state();
    for (failure, refusal) in [
        (store_failure as fn() -> _, Refusal::StoreFailed),
        (audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(world.move_to("d1", "beta").expect_err("refused"), refusal);
        assert_eq!(world.state(), before);
        assert_eq!(world.membership("d1"), membership("alpha", Some("")));
    }
    world.fake.heal();
    world.move_to("d1", "beta").expect("moved");
    assert_eq!(world.membership("d1"), membership("beta", Some("alpha")));
}

/// Proves: when the old record cannot be written back either, the call is still refused with the
/// token of the commit that failed, and the host stays in the group it was moved to.
///
/// Catches: a refusal that hides the failure of the commit behind the failure of the rollback.
#[test]
fn a_record_that_cannot_be_put_back_leaves_the_host_where_it_was_moved() {
    let world = World::new("hg-move-no-rollback");
    world.enroll("d1");
    world.put("alpha", &[SELF], None).expect("put");
    world.put("beta", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.fake.refuse(|_| Some(audit_failure()));
    // The reads of the call take two operations of the records, its write four, and the write
    // back begins with the seventh.
    world.records.fail_at(world.records.count() + 7);
    assert_eq!(
        world.move_to("d1", "beta").expect_err("refused"),
        Refusal::AuditUnavailable
    );
    world.records.fail_at(0);
    assert_eq!(world.membership("d1"), membership("beta", Some("alpha")));
}

/// Proves: a record that cannot be read or written is `store_failed` at whichever of its steps it
/// fails (the read of the host, the lock, the read, the deny row, the commit), and nothing of the
/// host or of the operations state changes.
///
/// Catches: a failed read taken for a host that is not enrolled.
#[test]
fn a_record_that_fails_to_read_or_write_changes_nothing() {
    let world = World::new("hg-move-record-fails");
    world.enroll("d1");
    world.put("alpha", &[SELF], None).expect("put");
    world.put("beta", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    let before = world.state();
    for step in 1..=6 {
        world.records.fail_at(world.records.count() + step);
        assert_eq!(
            world.move_to("d1", "beta").expect_err("refused"),
            Refusal::StoreFailed,
            "step {step}"
        );
        world.records.fail_at(0);
        assert_eq!(world.state(), before, "step {step}");
    }
}
