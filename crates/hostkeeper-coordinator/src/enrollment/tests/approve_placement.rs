//! `Enrollment.Approve` with a placement: the rack, the lowest U and the height an approval names
//! are put in the lab layout in the same step, of the inventory entry the host id names or, with
//! none, of the host; a placement that cannot be made refuses the approval and nothing changes; an
//! approval that names no rack places nothing.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::approve_groups::{approve, ask, post as post_in};
use super::support::{HostKey, Rig, audit_after, audit_of, closed_windows, enroll_request};
use crate::host_groups_support::{SELF, World, coordinator_on};
use crate::layout::{Kind, Layout, Subject};
use crate::marks_support::operator;
use crate::store::{audit_failure, store_failure};
use crate::testutil::Rig as CoordinatorRig;

use proto::EnrollmentRefusal as Refusal;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// A coordinator whose layout holds the row 1 with the racks 1 (10 U) and 2 (4 U).
fn coordinator(name: &str, enrollment: &Rig) -> CoordinatorRig {
    let rig = closed_windows(name, enrollment);
    rig.coordinator
        .layout()
        .change(|held| {
            let (held, row) = held.with_next_row()?;
            let (held, _) = held.with_rack("", &row.id, 10)?;
            held.with_rack("", &row.id, 4)
        })
        .expect("a layout");
    rig
}

/// Posts the request of `key` from `machine`, which claims the data MAC `data` when it is not empty.
fn post(enrollment: &Rig, key: &HostKey, machine: &str, data: &str) {
    let mut request = enroll_request(key, machine);
    if !data.is_empty() {
        request.claims.as_mut().expect("claims").addresses = vec![data.to_owned()];
    }
    enrollment.enrollment.enroll(request).expect("posted");
}

/// The approval of `key` as `host` (a storage host) into `rack`, from `u`, `height` U high.
fn approval(key: &HostKey, host: &str, rack: &str, u: u32, height: u32) -> proto::ApproveRequest {
    proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        reason: "built today".to_owned(),
        rack: rack.to_owned(),
        placement_u: u,
        placement_height_u: height,
        ..Default::default()
    }
}

fn layout(rig: &CoordinatorRig) -> Layout {
    rig.coordinator.layout().snapshot()
}

fn state(enrollment: &Rig, key: &HostKey) -> RequestState {
    enrollment
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

fn subject(kind: Kind, id: &str) -> Subject {
    Subject {
        kind,
        id: id.to_owned(),
    }
}

/// Proves: an approval that names a rack, a lowest U and a height places the approved host there
/// in the same step: the layout holds the placement of the host, the audit holds `enroll_approve`
/// and then `layout_place` about `host:<id>` with the reason `approval`.
///
/// Catches: an approved host that lands unplaced, or a placement that is not audited.
#[test]
fn an_approval_with_a_rack_places_the_host() {
    let enrollment = Rig::new("approve-places");
    let rig = coordinator("approve-places-coordinator", &enrollment);
    let key = HostKey::new();
    post(&enrollment, &key, "m-2", "");
    let before = audit_of(&rig.coordinator).len();

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "w1", "rack-1", 5, 2))
        .expect("approved");

    assert_eq!(approved.host_id, "w1");
    let held = layout(&rig);
    let placement = held.placement(&subject(Kind::Host, "w1")).expect("placed");
    assert_eq!(
        (placement.rack.as_str(), placement.u, placement.height_u),
        ("rack-1", 5, 2)
    );
    let entries = audit_after(&rig.coordinator, before);
    assert_eq!(
        entries
            .iter()
            .map(|(by, action, _, _)| (by.as_str(), action.as_str()))
            .collect::<Vec<_>>(),
        [("operator", "enroll_approve"), ("operator", "layout_place")]
    );
    assert_eq!(state(&enrollment, &key), RequestState::Approved);
}

/// Proves: an approval of a request that matches an inventory entry places the entry (the physical
/// server), not the host id's host, so a server keeps its place across a wipe and a new enrollment.
///
/// Catches: a placement attached to the enrollment record, which a wipe loses.
#[test]
fn an_approval_that_matches_an_inventory_entry_places_the_entry() {
    let enrollment = Rig::new("approve-places-entry");
    let rig = coordinator("approve-places-entry-coordinator", &enrollment);
    rig.coordinator
        .add_record(&operator(), super::support::add_of("db-1", &[DB1_DATA], ""))
        .expect("recorded");
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", DB1_DATA);

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "", "rack-2", 1, 1))
        .expect("approved as the record's");

    assert_eq!(approved.host_id, "db-1");
    let held = layout(&rig);
    assert!(held.placement(&subject(Kind::Inventory, "db-1")).is_some());
    assert!(held.placement(&subject(Kind::Host, "db-1")).is_none());
}

/// Proves: a placement that is half given, in a rack the layout lacks, over a server already in the
/// rack or past the rack's top refuses the approval with its own token, and the request stays
/// pending, the layout is as it was and the audit holds no new line; the same approval with a free
/// slot then lands.
///
/// Catches: an approval that stands while its placement was refused, or one that half-places.
#[test]
fn a_placement_that_cannot_be_made_refuses_the_approval_and_changes_nothing() {
    let enrollment = Rig::new("approve-refused");
    let rig = coordinator("approve-refused-coordinator", &enrollment);
    let first = HostKey::new();
    post(&enrollment, &first, "m-1", "");
    rig.coordinator
        .enrollment_approve(&operator(), approval(&first, "w1", "rack-1", 5, 2))
        .expect("the first server");
    let key = HostKey::new();
    post(&enrollment, &key, "m-2", "");
    let (before, audited) = (layout(&rig), audit_of(&rig.coordinator).len());

    for (rack, u, height, refusal) in [
        ("rack-1", 6, 1, Refusal::PlacementOverlap),
        ("rack-1", 10, 2, Refusal::PlacementOutOfRack),
        ("rack-9", 1, 1, Refusal::UnknownRack),
        ("rack-1", 0, 1, Refusal::PlacementInvalid),
        ("rack-1", 1, 0, Refusal::PlacementInvalid),
        ("", 1, 1, Refusal::PlacementInvalid),
        ("", 0, 1, Refusal::PlacementInvalid),
    ] {
        let refused = rig
            .coordinator
            .enrollment_approve(&operator(), approval(&key, "w2", rack, u, height));
        assert_eq!(refused.err(), Some(refusal), "{rack} {u} {height}");
        assert_eq!(state(&enrollment, &key), RequestState::Pending);
        assert_eq!(layout(&rig), before);
        assert_eq!(audit_of(&rig.coordinator).len(), audited);
    }

    rig.coordinator
        .enrollment_approve(&operator(), approval(&key, "w2", "rack-1", 7, 1))
        .expect("a free slot");
    assert_eq!(state(&enrollment, &key), RequestState::Approved);
}

/// Proves: an approval the enrollment itself refuses (a host id already held) leaves the layout as
/// it was even when its placement is free, and an approval that names no rack places nothing and
/// writes no `layout_place` line.
///
/// Catches: a placement made for an approval that then failed, or one made for an approval that
/// asked for none.
#[test]
fn a_refused_approval_places_nothing_and_an_approval_without_a_rack_places_nothing() {
    let enrollment = Rig::new("approve-no-place");
    let rig = coordinator("approve-no-place-coordinator", &enrollment);
    let first = HostKey::new();
    post(&enrollment, &first, "m-1", "");
    rig.coordinator
        .enrollment_approve(&operator(), approval(&first, "w1", "", 0, 0))
        .expect("approved, unplaced");
    assert!(layout(&rig).placements().is_empty());
    assert!(
        audit_of(&rig.coordinator)
            .iter()
            .all(|(_, action, _, _)| action != "layout_place")
    );

    let key = HostKey::new();
    post(&enrollment, &key, "m-2", "");
    let refused = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "w1", "rack-1", 1, 1));
    assert!(refused.is_err(), "the host id is taken");
    assert!(layout(&rig).placements().is_empty());
    assert_eq!(state(&enrollment, &key), RequestState::Pending);

    let loose = rig.coordinator.enrollment_approve(
        &operator(),
        proto::ApproveRequest {
            placement_u: 3,
            ..approval(&key, "w3", "", 0, 0)
        },
    );
    assert_eq!(loose.err(), Some(Refusal::PlacementInvalid));
}

/// A world whose coordinator keeps its layout in the world's store (rack 1 of 10 U in row 1), so a
/// store that refuses commits refuses the layout's too, and a group `web` to approve into.
fn world(name: &str) -> World {
    let mut world = World::new(name);
    world.rig.coordinator = coordinator_on(
        &world.fake,
        &world.rig.clock,
        Some(std::sync::Arc::clone(&world.enrollment.enrollment)),
    )
    .with_layout(crate::layout::LayoutStore::over(
        std::sync::Arc::new(world.fake.clone()),
        Layout::default(),
    ));
    world
        .coordinator()
        .layout()
        .change(|held| {
            let (held, row) = held.with_next_row()?;
            held.with_rack("", &row.id, 10)
        })
        .expect("a layout");
    world.put("web", &[SELF, "web"], None).expect("put");
    world
}

fn placed_request(key: &HostKey) -> proto::ApproveRequest {
    proto::ApproveRequest {
        rack: "rack-1".to_owned(),
        placement_u: 2,
        placement_height_u: 2,
        ..ask(key, "w1", "", "web", &[])
    }
}

/// Proves: a placement the store cannot write after the approval and the placement were judged
/// leaves the approval standing and the host unplaced, and an audit line of the placement that
/// cannot be kept leaves both the approval and the placement.
///
/// Catches: an approval undone, or a host lost, because the lab's store failed.
#[test]
fn a_store_that_fails_after_the_judgement_leaves_the_approval_standing() {
    let world = world("approve-placement-store");
    let key = post_in(&world, "m-1");
    world
        .fake
        .refuse(|commit| commit.layout.map(|_| store_failure()));
    let approved = approve(&world, placed_request(&key));
    assert_eq!(approved, Ok("w1".to_owned()));
    assert!(
        world
            .coordinator()
            .layout()
            .snapshot()
            .placements()
            .is_empty()
    );
    assert_eq!(world.membership("w1").host_group, "web");

    world.fake.heal();
    let second = post_in(&world, "m-2");
    world.fake.refuse(|commit| {
        commit
            .audit
            .iter()
            .any(|line| line.action == "layout_place")
            .then(audit_failure)
    });
    let approved = approve(
        &world,
        proto::ApproveRequest {
            host_id: "w2".to_owned(),
            ..placed_request(&second)
        },
    );
    assert_eq!(approved, Ok("w2".to_owned()));
    let layout = world.coordinator().layout().snapshot();
    assert!(layout.placement(&subject(Kind::Host, "w2")).is_some());
}

/// Proves: each refusal of the layout reads as the refusal of the approval it is: an unknown rack,
/// an overlap and a rack too short as their own tokens, anything else as an invalid placement.
///
/// Catches: a placement refusal told as a store failure.
#[test]
fn a_refusal_of_the_layout_is_the_approvals_own_token() {
    use crate::layout::LayoutError as E;
    use crate::operator_enrollment::placement_refusal as of;
    let rack = || "rack-1".to_owned();
    assert_eq!(of(&E::UnknownRack(rack())), Refusal::UnknownRack);
    assert_eq!(
        of(&E::Overlap {
            rack: rack(),
            first: "a".into(),
            second: "b".into()
        }),
        Refusal::PlacementOverlap
    );
    assert_eq!(
        of(&E::OutOfRack {
            rack: rack(),
            subject: "a".into(),
            height: 4
        }),
        Refusal::PlacementOutOfRack
    );
    assert_eq!(of(&E::Capacity("x".into())), Refusal::PlacementInvalid);
    assert_eq!(of(&E::Invalid("x".into())), Refusal::PlacementInvalid);
}
