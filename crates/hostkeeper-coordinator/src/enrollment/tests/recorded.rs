//! The inventory record the coordinator writes for a host enrolled without one
//! (`inventory_enrolled.rs`): at an operator's approval of a request no record matches (a Mac of
//! the bootstrap package, by its serial; a server, by its universally administered MACs), and at
//! the start for every host enrolled before that no record matches; never beside a record that
//! matches, holds the host id, a MAC or the serial; once however often it runs; approving no
//! second key; and a host's place moved to its new record.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{
    Audited, HostKey, Rig, SERIAL, add_of, approve, audit_after, audit_of, audited, claiming,
    closed_windows, mac_request,
};
use crate::host_groups_support::{World, coordinator_on};
use crate::layout::{Kind, Layout, LayoutStore, Subject};
use crate::marks_support::operator;
use crate::operator_layout::placement_of;
use crate::store::{audit_failure, store_failure};
use crate::testutil::Rig as CoordinatorRig;

/// A server's data MAC, universally administered.
const DATA: &str = "0c:c4:7a:00:00:02";

/// The server's BMC MAC.
const BMC: &str = "0c:c4:7a:00:00:01";

/// A locally administered MAC (a container bridge's), which no record is written with.
const BRIDGE: &str = "02:42:ac:11:00:02";

/// The notes of a server's record written from its claims.
const NOTES: &str = "Recorded at enrollment from the host's claims.";

/// The notes of a Mac's record written from its claims.
const MAC_NOTES: &str = "Recorded at enrollment from the host's claims: a Mac (darwin-arm64).";

/// The reason the audit gives a record written at an approval, before what the record holds.
const AT_APPROVAL: &str = "at enrollment, from the host's claims";

/// The reason the audit gives a record written at a start, before what the record holds.
const AT_START: &str = "at the coordinator's start, for a host enrolled without one";

/// The approval of `key`'s request as `host`, a storage host, by the operator.
fn approval(key: &HostKey, host: &str) -> proto::ApproveRequest {
    proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        reason: "built today".to_owned(),
        ..proto::ApproveRequest::default()
    }
}

/// Posts `request` to `enrollment`: it waits for an operator.
fn post(enrollment: &Rig, request: hostkeeper_proto::hostkeeper::agent::v1::EnrollRequest) {
    let posted = enrollment.enrollment.enroll(request).expect("posted");
    assert_eq!(posted.state, EnrollState::Pending as i32);
}

/// Every record `rig`'s coordinator lists.
fn records(rig: &CoordinatorRig) -> Vec<proto::InventoryRecord> {
    rig.coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
}

/// The record the coordinator writes for `host` at `at_unix`, holding `data`, `bmc` and `serial`,
/// with `notes`, as `ListRecords` answers it: enrolled against by `host`, its host approved.
fn written(
    host: &str,
    data: &[&str],
    bmc: &str,
    serial: &str,
    notes: &str,
    at_unix: u64,
) -> proto::InventoryRecord {
    proto::InventoryRecord {
        host_id: host.to_owned(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        serial: serial.to_owned(),
        notes: notes.to_owned(),
        added_by: "coordinator".to_owned(),
        added_unix: at_unix,
        enrolled: true,
        host_held: proto::HostHeld::Approved.into(),
        state: proto::RecordState::Closed.into(),
        enrolled_host_id: host.to_owned(),
        ..proto::InventoryRecord::default()
    }
}

/// The `record_add` entries of `rig`'s audit after the first `skip`.
fn record_adds(rig: &CoordinatorRig, skip: usize) -> Vec<Audited> {
    audit_after(&rig.coordinator, skip)
        .into_iter()
        .filter(|(_, action, _, _)| action == "record_add")
        .collect()
}

/// The ops clock of [`closed_windows`], at which its records are added.
const OPS_NOW: u64 = super::support::LONG_BEFORE;

/// Proves: a Mac that came through the bootstrap package (its request claims its serial and
/// platform UUID, and no record holds the serial), approved by an operator's click, is recorded
/// by the coordinator in the same call: `ListRecords` answers the record of its host id with its
/// serial, no MAC, no group nor role, no window (closed), added by `coordinator` with the notes
/// that say it was recorded at enrollment and is a Mac, enrolled against by the Mac; `Hosts.Get`
/// answers the record; the audit holds the approval, then `record_add` by `coordinator` naming
/// the serial. A second key that claims the same serial is not approved by the record: it waits
/// for an operator, matched to it.
///
/// Catches: the bootstrap Macs missing from the inventory (the record never written), a record
/// written without its serial (which then matches nothing), and a record that names a group or
/// has a window, which would approve a stranger claiming the serial.
#[test]
fn a_mac_approved_with_no_record_is_recorded_from_its_claims() {
    let enrollment = Rig::new("recorded-mac");
    let rig = closed_windows("recorded-mac-coordinator", &enrollment);
    let key = HostKey::new();
    post(&enrollment, mac_request(&key, "mac-m1", SERIAL, &[]));
    let before = audit_of(&rig.coordinator).len();

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "mac-1"))
        .expect("approved");

    assert_eq!(approved.host_id, "mac-1");
    assert_eq!(
        records(&rig),
        [written("mac-1", &[], "", SERIAL, MAC_NOTES, OPS_NOW)]
    );
    let entries = audit_after(&rig.coordinator, before);
    assert_eq!(
        entries
            .iter()
            .map(|(by, action, _, _)| (by.as_str(), action.as_str()))
            .collect::<Vec<_>>(),
        [
            ("operator", "enroll_approve"),
            ("coordinator", "record_add")
        ]
    );
    assert_eq!(
        entries.last(),
        Some(&audited(
            "coordinator",
            "record_add",
            Some("mac-1"),
            &format!("{AT_APPROVAL}; serial {SERIAL}")
        ))
    );
    let host = rig
        .coordinator
        .list_hosts(&operator(), proto::ListHostsRequest::default())
        .expect("listed")
        .hosts
        .into_iter()
        .find(|host| host.node_id == "mac-1")
        .expect("the Mac is listed");
    assert_eq!(
        (host.inventory_record_id.as_str(), host.serial.as_str()),
        ("mac-1", SERIAL)
    );

    let stranger = HostKey::new();
    let answered = rig
        .coordinator
        .enroll(
            &enrollment.enrollment,
            mac_request(&stranger, "mac-m2", SERIAL, &[]),
        )
        .expect("posted");
    assert_eq!(answered.state, EnrollState::Pending as i32);
    let listed = rig
        .coordinator
        .enrollment_list(&operator(), &proto::ListPendingRequest::default())
        .expect("listed");
    let pending = listed
        .requests
        .iter()
        .find(|r| r.request_id == stranger.key_hash().to_hex())
        .expect("the stranger's request is pending");
    assert_eq!(pending.record_host_id, "mac-1");
    assert_eq!(records(&rig).len(), 1, "no second record");
}

/// Proves: a server approved by an operator with no record that matches it is recorded from its
/// claims: its universally administered data MACs (not a bridge's locally administered MAC, nor
/// an address that is no MAC, nor its BMC MAC), its BMC MAC, no serial, and the notes that say it
/// was recorded at enrollment; the host is then enrolled against that record.
///
/// Catches: a made-up MAC recorded (it repeats on other hosts and would refuse their records),
/// the BMC MAC recorded as a data MAC, and a record the host does not match afterwards.
#[test]
fn a_server_approved_with_no_record_is_recorded_with_its_universal_macs() {
    let enrollment = Rig::new("recorded-server");
    let rig = closed_windows("recorded-server-coordinator", &enrollment);
    let key = HostKey::new();
    post(
        &enrollment,
        claiming(&key, "m-1", &[DATA, BRIDGE, "192.0.2.5", BMC], BMC),
    );

    rig.coordinator
        .enrollment_approve(&operator(), approval(&key, "web-1"))
        .expect("approved");

    assert_eq!(
        records(&rig),
        [written("web-1", &[DATA], BMC, "", NOTES, OPS_NOW)]
    );
}

/// Proves: an approval of a request a record matches writes no other record (the record stays as
/// the operator added it, and the audit holds no `record_add` by `coordinator`); and a host whose
/// claims hold neither a Mac serial nor a universally administered MAC is approved and recorded
/// by nothing.
///
/// Catches: a second record written beside the one that matched, and a record written with
/// neither a MAC nor a serial, which no host could match.
#[test]
fn a_matched_host_and_a_host_that_claims_nothing_recordable_get_no_record() {
    let enrollment = Rig::new("recorded-none");
    let rig = closed_windows("recorded-none-coordinator", &enrollment);
    rig.coordinator
        .add_record(&operator(), add_of("db-1", &[DATA], ""))
        .expect("recorded");
    let matched = HostKey::new();
    post(&enrollment, claiming(&matched, "m-1", &[DATA], ""));
    let bare = HostKey::new();
    post(&enrollment, claiming(&bare, "m-2", &[BRIDGE], ""));
    let before = audit_of(&rig.coordinator).len();

    for (key, host) in [(&matched, ""), (&bare, "w-2")] {
        rig.coordinator
            .enrollment_approve(&operator(), approval(key, host))
            .expect("approved");
    }

    let listed = records(&rig);
    assert_eq!(
        listed
            .iter()
            .map(|r| (r.host_id.as_str(), r.added_by.as_str()))
            .collect::<Vec<_>>(),
        [("db-1", "operator")]
    );
    assert_eq!(record_adds(&rig, before), Vec::<Audited>::new());
}

/// Proves: a host whose claims give a record that another record refuses is approved and left
/// without a record: a MAC another record holds (`record_conflict`: that record holds another BMC
/// MAC, so it does not match the host), and a host id a record of other MACs holds
/// (`record_exists`). Neither record is changed.
///
/// Catches: a record written beside one that holds its MAC or its name, which would break the
/// one-record-per-MAC and per-host-id rules every match rests on, and an approval refused for a
/// record it could not write.
#[test]
fn a_claim_another_record_holds_leaves_the_host_unrecorded() {
    let enrollment = Rig::new("recorded-conflict");
    let rig = closed_windows("recorded-conflict-coordinator", &enrollment);
    let other_bmc = "0c:c4:7a:00:00:a1";
    rig.coordinator
        .add_record(&operator(), add_of("other", &[DATA], other_bmc))
        .expect("recorded");
    rig.coordinator
        .add_record(&operator(), add_of("w-3", &["0c:c4:7a:00:00:31"], ""))
        .expect("recorded");
    let sharing = HostKey::new();
    post(&enrollment, claiming(&sharing, "m-1", &[DATA], BMC));
    let named = HostKey::new();
    post(
        &enrollment,
        claiming(&named, "m-2", &["0c:c4:7a:00:00:32"], ""),
    );
    let held = records(&rig);
    let before = audit_of(&rig.coordinator).len();

    for (key, host) in [(&sharing, "w-2"), (&named, "w-3")] {
        let approved = rig
            .coordinator
            .enrollment_approve(&operator(), approval(key, host))
            .expect("approved");
        assert_eq!(approved.host_id, host);
    }

    assert_eq!(
        records(&rig)
            .into_iter()
            .map(|r| (r.host_id, r.data_macs, r.added_by))
            .collect::<Vec<_>>(),
        held.into_iter()
            .map(|r| (r.host_id, r.data_macs, r.added_by))
            .collect::<Vec<_>>()
    );
    assert_eq!(record_adds(&rig, before), Vec::<Audited>::new());
}

/// Proves: at the start (`record_unrecorded`) every host enrolled before, approved without a
/// record (a Mac by its serial, a server by its MACs), is recorded from its claims with the reason
/// that says it was recorded at the start, and a host a record matches is not; the count answered
/// is the records written; a second start writes nothing and audits nothing.
///
/// Catches: the four bootstrap Macs enrolled before this release kept out of the inventory, a
/// record written twice (or a second audit line) at each start, and a recorded host recorded
/// again.
#[test]
fn the_start_records_every_enrolled_host_without_one_once() {
    let enrollment = Rig::new("recorded-start");
    let rig = closed_windows("recorded-start-coordinator", &enrollment);
    rig.coordinator
        .add_record(&operator(), add_of("db-1", &[DATA], ""))
        .expect("recorded");
    let mac = HostKey::new();
    post(&enrollment, mac_request(&mac, "mac-m1", SERIAL, &[]));
    approve(&enrollment.enrollment, &mac, "mac-1", false);
    let server = HostKey::new();
    post(
        &enrollment,
        claiming(&server, "m-2", &["0c:c4:7a:00:00:22"], ""),
    );
    approve(&enrollment.enrollment, &server, "web-2", false);
    let matched = HostKey::new();
    post(&enrollment, claiming(&matched, "m-3", &[DATA], ""));
    approve(&enrollment.enrollment, &matched, "db-1", false);
    let before = audit_of(&rig.coordinator).len();

    assert_eq!(rig.coordinator.record_unrecorded(), 2);

    let listed = records(&rig);
    assert_eq!(
        listed
            .iter()
            .map(|r| (r.host_id.as_str(), r.added_by.as_str(), r.enrolled))
            .collect::<Vec<_>>(),
        [
            ("db-1", "operator", true),
            ("mac-1", "coordinator", true),
            ("web-2", "coordinator", true)
        ]
    );
    assert_eq!(
        listed[1..],
        [
            written("mac-1", &[], "", SERIAL, MAC_NOTES, OPS_NOW),
            written("web-2", &["0c:c4:7a:00:00:22"], "", "", NOTES, OPS_NOW),
        ]
    );
    assert_eq!(
        record_adds(&rig, before),
        [
            audited(
                "coordinator",
                "record_add",
                Some("mac-1"),
                &format!("{AT_START}; serial {SERIAL}")
            ),
            audited(
                "coordinator",
                "record_add",
                Some("web-2"),
                &format!("{AT_START}; data 0c:c4:7a:00:00:22")
            ),
        ]
    );
    let after = audit_of(&rig.coordinator).len();
    assert_eq!(rig.coordinator.record_unrecorded(), 0);
    assert_eq!(audit_of(&rig.coordinator).len(), after, "nothing audited");
}

/// A subject of `kind` named `id`.
fn subject(kind: Kind, id: &str) -> Subject {
    Subject {
        kind,
        id: id.to_owned(),
    }
}

/// `rig`'s layout with the row 1 and its rack 1 (10 U).
fn with_rack(rig: &CoordinatorRig) {
    rig.coordinator
        .layout()
        .change(|held| {
            let (held, row) = held.with_next_row()?;
            held.with_rack("", &row.id, 10)
        })
        .expect("a layout");
}

/// Places `subject` in rack 1 from U `u`, 1 U high, in `rig`'s layout.
fn place(rig: &CoordinatorRig, subject: Subject, u: u32) {
    rig.coordinator
        .layout()
        .change(|held| {
            let (next, _) = held.with_placement(placement_of(subject, "rack-1", u, 1))?;
            Ok((next, ()))
        })
        .expect("placed");
}

/// Where `subject` stands in `layout`: its rack, lowest U and slot.
fn where_of(layout: &Layout, subject: &Subject) -> Option<(String, u32, u32)> {
    layout
        .placement(subject)
        .map(|(item, sled)| (sled.rack.clone(), sled.u, item.slot))
}

/// Proves: a host the layout places as itself (`host:<id>`) that the start records has its place
/// moved to the record (`inventory:<id>`), on the same sled and slot, audited `layout_place` by
/// `coordinator`; a start that records nothing moves nothing.
///
/// Catches: a placed Mac shown unplaced, with its old place left as a server that is gone, once
/// its record is written (the lab view joins a host with a record by the record's key).
#[test]
fn a_host_placed_as_itself_has_its_place_moved_to_its_new_record() {
    let enrollment = Rig::new("recorded-placed");
    let rig = closed_windows("recorded-placed-coordinator", &enrollment);
    with_rack(&rig);
    let mac = HostKey::new();
    post(&enrollment, mac_request(&mac, "mac-m1", SERIAL, &[]));
    approve(&enrollment.enrollment, &mac, "mac-1", false);
    place(&rig, subject(Kind::Host, "mac-1"), 3);
    let before = audit_of(&rig.coordinator).len();

    assert_eq!(rig.coordinator.record_unrecorded(), 1);

    let layout = rig.coordinator.layout().snapshot();
    assert_eq!(
        where_of(&layout, &subject(Kind::Inventory, "mac-1")),
        Some(("rack-1".to_owned(), 3, 0))
    );
    assert_eq!(where_of(&layout, &subject(Kind::Host, "mac-1")), None);
    assert_eq!(
        audit_after(&rig.coordinator, before)
            .last()
            .map(|(by, action, _, reason)| (by.as_str(), action.as_str(), reason.as_deref())),
        Some((
            "coordinator",
            "layout_place",
            Some("moved from host:mac-1 to its record")
        ))
    );
    let still = rig.coordinator.layout().snapshot();
    assert_eq!(rig.coordinator.record_unrecorded(), 0);
    assert_eq!(rig.coordinator.layout().snapshot(), still);
}

/// Proves: an approval that names a rack, of a host the approval records, places the record
/// (`inventory:<id>`); and one that takes over (`replace`) a host id placed as itself moves that
/// place to the record and then to the place it names, leaving no place of the host behind.
///
/// Catches: a new host placed as itself beside the record written for it, and a replaced host's
/// old place left behind as a second server.
#[test]
fn an_approval_with_a_rack_places_the_record_it_writes() {
    let enrollment = Rig::new("recorded-approve-placed");
    let rig = closed_windows("recorded-approve-placed-coordinator", &enrollment);
    with_rack(&rig);
    let first = HostKey::new();
    post(&enrollment, claiming(&first, "m-1", &[DATA], ""));
    rig.coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                rack: "rack-1".to_owned(),
                placement_u: 2,
                placement_height_u: 1,
                ..approval(&first, "web-1")
            },
        )
        .expect("approved");
    let layout = rig.coordinator.layout().snapshot();
    assert_eq!(
        where_of(&layout, &subject(Kind::Inventory, "web-1")),
        Some(("rack-1".to_owned(), 2, 0))
    );
    assert_eq!(where_of(&layout, &subject(Kind::Host, "web-1")), None);

    let old = HostKey::new();
    post(&enrollment, mac_request(&old, "mac-m1", SERIAL, &[]));
    approve(&enrollment.enrollment, &old, "mac-1", false);
    place(&rig, subject(Kind::Host, "mac-1"), 5);
    let wiped = HostKey::new();
    post(
        &enrollment,
        mac_request(&wiped, "mac-m1b", "C02OTHER1", &[]),
    );
    rig.coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                rack: "rack-1".to_owned(),
                placement_u: 7,
                placement_height_u: 1,
                replace: true,
                ..approval(&wiped, "mac-1")
            },
        )
        .expect("approved");
    let layout = rig.coordinator.layout().snapshot();
    assert_eq!(
        where_of(&layout, &subject(Kind::Inventory, "mac-1")),
        Some(("rack-1".to_owned(), 7, 0))
    );
    assert_eq!(where_of(&layout, &subject(Kind::Host, "mac-1")), None);
    assert_eq!(layout.sleds().len(), 2, "the old place's sled went with it");
}

/// Proves: an approval whose host is placed as itself and recorded without a rack moves the
/// place to the record, as the start does: the replaced host id of a Mac placed as itself.
///
/// Catches: the move made only at the start, so a host approved again in a running coordinator
/// is shown unplaced until the next start.
#[test]
fn an_approval_without_a_rack_moves_the_place_to_the_record() {
    let enrollment = Rig::new("recorded-approve-moved");
    let rig = closed_windows("recorded-approve-moved-coordinator", &enrollment);
    with_rack(&rig);
    let old = HostKey::new();
    post(&enrollment, mac_request(&old, "mac-m1", SERIAL, &[]));
    approve(&enrollment.enrollment, &old, "mac-1", false);
    place(&rig, subject(Kind::Host, "mac-1"), 4);
    let wiped = HostKey::new();
    post(
        &enrollment,
        mac_request(&wiped, "mac-m1b", "C02OTHER1", &[]),
    );

    rig.coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                replace: true,
                ..approval(&wiped, "mac-1")
            },
        )
        .expect("approved");

    let layout = rig.coordinator.layout().snapshot();
    assert_eq!(
        where_of(&layout, &subject(Kind::Inventory, "mac-1")),
        Some(("rack-1".to_owned(), 4, 0))
    );
    assert_eq!(where_of(&layout, &subject(Kind::Host, "mac-1")), None);
    assert_eq!(
        records(&rig)
            .iter()
            .map(|r| (r.host_id.as_str(), r.serial.as_str()))
            .collect::<Vec<_>>(),
        [("mac-1", "C02OTHER1")]
    );
}

/// A world (an in-memory store that can be told to fail) whose layout holds the rack 1, and a Mac
/// `mac-1` approved before (no record), placed as itself at U 3.
fn placed_world(name: &str) -> World {
    let mut world = World::new(name);
    world.rig.coordinator = coordinator_on(
        &world.fake,
        &world.rig.clock,
        Some(std::sync::Arc::clone(&world.enrollment.enrollment)),
    )
    .with_layout(LayoutStore::over(
        std::sync::Arc::new(world.fake.clone()),
        Layout::default(),
    ));
    with_rack(&world.rig);
    let mac = HostKey::new();
    post(&world.enrollment, mac_request(&mac, "mac-m1", SERIAL, &[]));
    approve(&world.enrollment.enrollment, &mac, "mac-1", false);
    place(&world.rig, subject(Kind::Host, "mac-1"), 3);
    world
}

/// The host ids of the records `world` holds.
fn held_ids(world: &World) -> Vec<String> {
    world
        .coordinator()
        .ops
        .read(|state| state.inventory.iter().map(|r| r.host_id.clone()).collect())
}

/// Proves: a record the store cannot write leaves the approval standing and the host without a
/// record, so the next start writes it; a place the layout cannot move leaves the record written
/// and the place as it was, and the next start does not move it (the host is recorded); a move
/// whose audit line cannot be kept is still made.
///
/// Catches: an approval refused, or a start stopped, because the inventory or the layout could not
/// be written, and a record or a move half made.
#[test]
fn a_store_that_refuses_the_record_or_the_move_leaves_the_rest_standing() {
    let world = placed_world("recorded-store");
    world.fake.refuse(|commit| {
        commit
            .audit
            .iter()
            .any(|line| line.action == "record_add")
            .then(store_failure)
    });
    let key = HostKey::new();
    post(&world.enrollment, claiming(&key, "m-2", &[DATA], ""));
    let approved = world
        .coordinator()
        .enrollment_approve(&operator(), approval(&key, "web-2"));
    assert_eq!(approved.map(|a| a.host_id), Ok("web-2".to_owned()));
    assert_eq!(world.coordinator().record_unrecorded(), 0);
    assert_eq!(held_ids(&world), Vec::<String>::new());

    world
        .fake
        .refuse(|commit| commit.layout.map(|_| store_failure()));
    assert_eq!(world.coordinator().record_unrecorded(), 2);
    assert_eq!(held_ids(&world), ["mac-1", "web-2"]);
    let layout = world.coordinator().layout().snapshot();
    assert!(layout.placement(&subject(Kind::Host, "mac-1")).is_some());
    assert!(
        layout
            .placement(&subject(Kind::Inventory, "mac-1"))
            .is_none()
    );

    world.fake.refuse(|commit| {
        commit
            .audit
            .iter()
            .any(|line| line.action == "layout_place")
            .then(audit_failure)
    });
    world.coordinator().place_with_record("mac-1");
    let layout = world.coordinator().layout().snapshot();
    assert!(
        layout
            .placement(&subject(Kind::Inventory, "mac-1"))
            .is_some()
    );
    assert!(layout.placement(&subject(Kind::Host, "mac-1")).is_none());
}

/// Proves: when the enrollment store cannot be read, an approval still stands (it was written
/// before the read) and writes no record, and a start writes none and answers 0; once the store
/// reads again, a start records both hosts.
///
/// Catches: a failed read taken for "no host enrolled" that records nothing silently forever (the
/// next start writes the records), and an approval refused for it.
#[test]
fn an_unreadable_enrollment_store_writes_no_record() {
    let world = placed_world("recorded-unreadable");
    let key = HostKey::new();
    post(&world.enrollment, claiming(&key, "m-2", &[DATA], ""));
    // The approval's own reads and write are the next six operations of the records; the read
    // of the approved host, which the record is written from, is the seventh.
    world.records.fail_at(world.records.count() + 7);
    let approved = world
        .coordinator()
        .enrollment_approve(&operator(), approval(&key, "web-2"));
    assert_eq!(approved.map(|a| a.host_id), Ok("web-2".to_owned()));
    assert_eq!(held_ids(&world), Vec::<String>::new());

    world.records.fail_at(world.records.count() + 1);
    assert_eq!(world.coordinator().record_unrecorded(), 0);
    assert_eq!(held_ids(&world), Vec::<String>::new());
    world.records.fail_at(0);
    assert_eq!(world.coordinator().record_unrecorded(), 2);
    assert_eq!(held_ids(&world), ["mac-1", "web-2"]);
    let state = world
        .enrollment
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state;
    assert_eq!(state, RequestState::Approved);
}

/// The `n`th universally administered data MAC of the tests' server with many ports.
fn port(n: u8) -> String {
    format!("0c:c4:7a:00:01:{n:02x}")
}

/// Proves: the edges of what a record is written from: a BMC MAC that is not universally
/// administered is left out (the data MAC recorded alone); of more than 16 universally
/// administered data MACs the record holds the 16 lowest, and the host matches it; a Mac whose
/// claimed serial is not one a record may hold (a space in it) gets no record, nor do its data
/// MACs give it one, and its notes would not call another host a Mac.
///
/// Catches: a made-up BMC MAC recorded, a record refused (or the approval failing) for having more
/// MACs than a record holds, and a record a Mac never matches (it is matched by its serial alone).
#[test]
fn the_record_takes_what_a_record_may_hold_and_no_more() {
    let enrollment = Rig::new("recorded-edges");
    let rig = closed_windows("recorded-edges-coordinator", &enrollment);
    let local_bmc = HostKey::new();
    post(
        &enrollment,
        claiming(&local_bmc, "m-1", &[DATA], "02:00:00:00:00:01"),
    );
    let many = HostKey::new();
    let ports: Vec<String> = (0..17).map(port).collect();
    let claimed: Vec<&str> = ports.iter().map(String::as_str).collect();
    post(&enrollment, claiming(&many, "m-2", &claimed, ""));
    let bad_serial = HostKey::new();
    let mut request = mac_request(&bad_serial, "mac-m3", "C02 X1", &[]);
    request.claims.as_mut().expect("claims").addresses = vec!["0c:c4:7a:00:00:33".to_owned()];
    post(&enrollment, request);

    for (key, host) in [
        (&local_bmc, "web-1"),
        (&many, "web-2"),
        (&bad_serial, "mac-3"),
    ] {
        rig.coordinator
            .enrollment_approve(&operator(), approval(key, host))
            .expect("approved");
    }

    let listed = records(&rig);
    assert_eq!(
        listed,
        [
            written("web-1", &[DATA], "", "", NOTES, OPS_NOW),
            written("web-2", &claimed[..16], "", "", NOTES, OPS_NOW),
        ]
    );
}

/// Proves: a host the layout places as itself that an approval leaves without a record (it claims
/// nothing a record holds) keeps its place as itself, and so does one the start cannot record;
/// moving the place of a host whose host id no record holds changes nothing.
///
/// Catches: a host's place handed to a record that does not exist, which shows the host unplaced.
#[test]
fn a_host_left_without_a_record_keeps_its_place() {
    let enrollment = Rig::new("recorded-kept-place");
    let rig = closed_windows("recorded-kept-place-coordinator", &enrollment);
    with_rack(&rig);
    let old = HostKey::new();
    post(&enrollment, claiming(&old, "m-1", &[BRIDGE], ""));
    approve(&enrollment.enrollment, &old, "w-1", false);
    place(&rig, subject(Kind::Host, "w-1"), 2);
    let again = HostKey::new();
    post(&enrollment, claiming(&again, "m-1b", &[BRIDGE], ""));

    rig.coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                replace: true,
                ..approval(&again, "w-1")
            },
        )
        .expect("approved");
    assert_eq!(rig.coordinator.record_unrecorded(), 0);
    rig.coordinator.place_with_record("w-1");

    assert_eq!(records(&rig), Vec::<proto::InventoryRecord>::new());
    let layout = rig.coordinator.layout().snapshot();
    assert_eq!(
        where_of(&layout, &subject(Kind::Host, "w-1")),
        Some(("rack-1".to_owned(), 2, 0))
    );
    assert_eq!(where_of(&layout, &subject(Kind::Inventory, "w-1")), None);
}
