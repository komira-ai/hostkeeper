//! `Enrollment.UpdateRecord`: it changes the data MACs, the BMC MAC and the notes of a record and
//! nothing else, checks them as `AddRecord` does, refuses what would break the records' rules,
//! audits the change, and survives a restart. Notes may hold line breaks.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::inventory::mark_used;
use crate::marks_support::{operator, restart, rig};
use crate::operator::Caller;
use crate::ops::{AuditEntry, Write};
use crate::testutil::Rig;

use proto::EnrollmentRefusal as Refusal;

fn add(rig: &Rig, request: proto::AddRecordRequest) {
    rig.coordinator
        .add_record(&operator(), request)
        .expect("added");
}

/// A record of `host` with the role `storage`, one data MAC, a serial, a platform and notes: more
/// than the four fields the Inventory page shows.
fn full(host: &str, mac: &str) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![mac.to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "first".to_owned(),
        platform: agent::Platform::LinuxArm64.into(),
        ..proto::AddRecordRequest::default()
    }
}

fn update(
    rig: &Rig,
    host: &str,
    data: &[&str],
    bmc: &str,
    notes: &str,
) -> Result<proto::UpdateRecordResponse, Refusal> {
    rig.coordinator.update_record(
        &operator(),
        proto::UpdateRecordRequest {
            host_id: host.to_owned(),
            data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
            bmc_mac: bmc.to_owned(),
            notes: notes.to_owned(),
        },
    )
}

fn listed(rig: &Rig) -> Vec<proto::InventoryRecord> {
    rig.coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
}

/// Proves: an update sets the data MACs (in their one spelling, sorted, without repeats), the BMC
/// MAC and the notes, and leaves the role, serial, platform, window, who added it and when exactly
/// as they were, through a restart too; the audit line is `record_update` for the host.
///
/// Catches: a change of a MAC or note that wipes the fields the Inventory page does not show.
#[test]
fn an_update_changes_the_macs_and_notes_and_keeps_every_other_field() {
    let mut rig = rig("update-keeps", "");
    add(&rig, full("db-1", "0c:c4:7a:00:00:02"));
    let before = listed(&rig).remove(0);
    let done = update(
        &rig,
        "db-1",
        &["0C-C4-7A-00-00-04", "0cc47a000003", "0C:C4:7A:00:00:04"],
        "0C:C4:7A:00:00:05",
        "  rack 4\nslot 2 ",
    )
    .expect("updated");
    let want = proto::InventoryRecord {
        data_macs: vec![
            "0c:c4:7a:00:00:03".to_owned(),
            "0c:c4:7a:00:00:04".to_owned(),
        ],
        bmc_mac: "0c:c4:7a:00:00:05".to_owned(),
        notes: "rack 4\nslot 2".to_owned(),
        ..before.clone()
    };
    assert_eq!(done.record.as_ref(), Some(&want));
    let entry = done.entry.expect("an audit entry");
    assert_eq!(entry.action, "record_update");
    assert_eq!(entry.node_id.as_deref(), Some("db-1"));
    assert_eq!(listed(&rig), vec![want.clone()]);
    restart(&mut rig, "");
    assert_eq!(listed(&rig), vec![want]);
}

/// Proves: an update empties the BMC MAC and the notes when it gives none, and a record with a
/// serial (a Mac's) may have no data MAC.
#[test]
fn an_update_may_clear_the_bmc_mac_and_the_notes_and_a_serial_record_may_hold_no_data_mac() {
    let rig = rig("update-clear", "");
    add(&rig, full("db-1", "0c:c4:7a:00:00:02"));
    let done = update(&rig, "db-1", &[], "", "").expect("updated");
    let record = done.record.expect("a record");
    assert_eq!(
        (
            record.data_macs,
            record.bmc_mac,
            record.notes,
            record.serial
        ),
        (vec![], String::new(), String::new(), "SN-1".to_owned())
    );
}

/// Proves: a record that was used by an approval stays used, with its request and time, after its
/// notes are updated, and a record the update leaves with its own MACs is not in conflict with
/// itself.
///
/// Catches: removing and adding a record again to change it, which opens a used record again.
#[test]
fn an_update_keeps_the_use_of_a_used_record_and_its_own_macs_are_no_conflict() {
    let rig = rig("update-used", "");
    add(&rig, full("db-1", "0c:c4:7a:00:00:02"));
    rig.coordinator
        .ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("inventory", "enrollment.auto_approved"),
            |state| mark_used(&mut state.inventory, "db-1", "r1", 1_800_000_100),
        )
        .expect("marked");
    let done = update(
        &rig,
        "db-1",
        &["0c:c4:7a:00:00:02"],
        "0c:c4:7a:00:00:01",
        "second",
    )
    .expect("updated");
    let record = done.record.expect("a record");
    assert_eq!(record.state, i32::from(proto::RecordState::Used));
    assert_eq!(record.used_by_request_id, "r1");
    assert_eq!(record.used_unix, 1_800_000_100);
    assert_eq!(record.notes, "second");
}

/// Proves: each refusal, and that a refused update writes nothing.
#[test]
fn an_update_is_refused_for_what_add_refuses_and_for_a_conflict_or_a_missing_record() {
    let rig = rig("update-refused", "");
    add(&rig, full("db-1", "0c:c4:7a:00:00:02"));
    let mut other = full("db-2", "0c:c4:7a:00:00:12");
    other.serial = "SN-2".to_owned();
    other.bmc_mac = "0c:c4:7a:00:00:11".to_owned();
    add(&rig, other);
    let many: Vec<String> = (0..17).map(|n| format!("0c:c4:7a:00:01:{n:02x}")).collect();
    let seventeen: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut plain = full("db-3", "0c:c4:7a:00:00:22");
    plain.serial = String::new();
    plain.bmc_mac = String::new();
    add(&rig, plain);
    let cases = [
        ("db-9", vec!["0c:c4:7a:00:00:02"], "", "", Refusal::NoRecord),
        (
            "DB-1",
            vec!["0c:c4:7a:00:00:02"],
            "",
            "",
            Refusal::InvalidHostId,
        ),
        ("db-1", vec!["0c:c4:7a:00"], "", "", Refusal::InvalidRequest),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:02"],
            "zz",
            "",
            Refusal::InvalidRequest,
        ),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:02"],
            "0c:c4:7a:00:00:02",
            "",
            Refusal::InvalidRequest,
        ),
        ("db-1", seventeen, "", "", Refusal::InvalidRequest),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:02"],
            "",
            "a\tb",
            Refusal::InvalidRequest,
        ),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:02"],
            "",
            &"n".repeat(257),
            Refusal::InvalidRequest,
        ),
        ("db-3", vec![], "", "", Refusal::InvalidRequest),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:12"],
            "",
            "",
            Refusal::RecordConflict,
        ),
        (
            "db-1",
            vec!["0c:c4:7a:00:00:02"],
            "0c:c4:7a:00:00:11",
            "",
            Refusal::RecordConflict,
        ),
    ];
    let before = listed(&rig);
    let audited = rig.coordinator.ops.state().audit.len();
    for (host, data, bmc, notes, refusal) in cases {
        assert_eq!(
            update(&rig, host, &data, bmc, notes).err(),
            Some(refusal),
            "{host} {data:?} {bmc} {notes}"
        );
    }
    assert_eq!(listed(&rig), before);
    assert_eq!(rig.coordinator.ops.state().audit.len(), audited);
    let exactly = update(&rig, "db-1", &["0c:c4:7a:00:00:02"], "", &"n".repeat(256));
    assert!(exactly.is_ok(), "256 bytes of notes are notes");
}

/// Proves: only an operator updates a record.
#[test]
fn only_an_operator_may_update_a_record() {
    let rig = rig("update-caller", "");
    add(&rig, full("db-1", "0c:c4:7a:00:00:02"));
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        let done = rig
            .coordinator
            .update_record(&caller, proto::UpdateRecordRequest::default());
        assert_eq!(done.err(), Some(refusal));
    }
}

/// Proves: notes with line breaks are kept by `AddRecord` and listed as typed, and notes with
/// another control character, or past 256 bytes, are still refused.
#[test]
fn notes_may_hold_line_breaks_when_added() {
    let rig = rig("update-notes-add", "");
    let mut request = full("db-1", "0c:c4:7a:00:00:02");
    request.notes = "rack 4\nslot 2".to_owned();
    add(&rig, request);
    assert_eq!(listed(&rig)[0].notes, "rack 4\nslot 2");
    let mut bad = full("db-2", "0c:c4:7a:00:00:12");
    bad.notes = "tab\there".to_owned();
    assert_eq!(
        rig.coordinator.add_record(&operator(), bad).err(),
        Some(Refusal::InvalidRequest)
    );
}
