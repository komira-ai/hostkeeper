//! The approval window of an inventory record: how long `Enrollment.AddRecord` opens it for and
//! what it refuses, `Enrollment.OpenRecord`, the state `Enrollment.ListRecords` shows (open,
//! closed, used), the ops file's two new keys, and the limits `--record-approve-for` and
//! `--record-approve-max` set. The approval of a request by a record is
//! `enrollment/tests/by_record*.rs`'s.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_enrollment::enroll::Claims;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::{Value, json};

use crate::inventory::{
    DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS, InventoryMatch, InventoryRecord, RecordUse,
    RecordWindow, WindowAboveMax, mark_used, match_claims,
};
use crate::marks_support::{operator, start};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsStore, Write};
use crate::testutil::{Rig, ops_of, scratch};

use proto::EnrollmentRefusal as Refusal;
use proto::RecordState;

/// The time the tests start at, in unix seconds.
const NOW: u64 = 1_800_000_000;

/// A rig whose ops clock the test sets, with `window` as its limits.
fn rig(name: &str, window: RecordWindow) -> (Rig, Arc<AtomicU64>) {
    let mut rig = Rig::new(name);
    let unix = Arc::new(AtomicU64::new(NOW));
    let clock = Arc::clone(&unix);
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(
        ops_of(&rig.dir)
            .with_unix(Arc::new(move || clock.load(Ordering::SeqCst)))
            .with_record_window(window),
    );
    (rig, unix)
}

/// A rig with the limits of the flags' defaults.
fn default_rig(name: &str) -> (Rig, Arc<AtomicU64>) {
    rig(name, RecordWindow::default())
}

/// An add of `host` in the storage role with the data MAC `mac`.
fn add_of(host: &str, mac: &str) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![mac.to_owned()],
        ..proto::AddRecordRequest::default()
    }
}

fn add(rig: &Rig, request: proto::AddRecordRequest) -> Result<proto::AddRecordResponse, Refusal> {
    rig.coordinator.add_record(&operator(), request)
}

fn open(
    rig: &Rig,
    request: proto::OpenRecordRequest,
) -> Result<proto::OpenRecordResponse, Refusal> {
    rig.coordinator.open_record(&operator(), request)
}

/// An open of `host` for `secs` seconds.
fn open_for(host: &str, secs: u64) -> proto::OpenRecordRequest {
    proto::OpenRecordRequest {
        host_id: host.to_owned(),
        approve_for_secs: secs,
        ..proto::OpenRecordRequest::default()
    }
}

/// The record of `host` as `Enrollment.ListRecords` shows it.
fn listed(rig: &Rig, host: &str) -> proto::InventoryRecord {
    rig.coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
        .into_iter()
        .find(|record| record.host_id == host)
        .expect("a record")
}

/// The number of audit entries.
fn audited(rig: &Rig) -> usize {
    rig.coordinator.ops.state().audit.len()
}

/// Marks the record of `host` used by the request `request` at `at`, as an approval does.
fn use_record(rig: &Rig, host: &str, request: &str, at: u64) {
    let (host, request) = (host.to_owned(), request.to_owned());
    rig.coordinator
        .ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("inventory", "enrollment.auto_approved"),
            move |state| mark_used(&mut state.inventory, &host, &request, at),
        )
        .expect("marked");
}

/// Proves: a record that names a role is added with a window of the coordinator's default length
/// from now, and one that names none with no window; the state each shows is open while the time
/// is before the window's end and closed from it on; and the audit line of the add says the end
/// of the window.
#[test]
fn a_role_record_is_added_with_the_default_window_and_a_roleless_one_with_none() {
    let (rig, unix) = rig(
        "window-default",
        RecordWindow::new(600, 3600).expect("limits"),
    );
    let added = add(&rig, add_of("db-1", "0c:c4:7a:00:00:02")).expect("added");
    let mut serial = proto::AddRecordRequest {
        host_id: "mac-1".to_owned(),
        serial: "C02X1".to_owned(),
        ..proto::AddRecordRequest::default()
    };
    let mac = add(&rig, serial.clone()).expect("a Mac's record");
    serial.host_id = "mac-2".to_owned();
    serial.serial = "C02X2".to_owned();
    serial.role = "build".to_owned();
    let mac_role = add(&rig, serial).expect("a Mac's record with a role");

    let db = added.record.expect("a record");
    assert_eq!(
        (db.approve_until_unix, db.state()),
        (NOW + 600, RecordState::Open)
    );
    assert_eq!(
        added.entry.and_then(|e| e.reason),
        Some(format!(
            "role storage; data 0c:c4:7a:00:00:02; approve until {}",
            NOW + 600
        ))
    );
    let none = mac.record.expect("a record");
    assert_eq!(
        (none.approve_until_unix, none.state()),
        (0, RecordState::Closed)
    );
    assert_eq!(
        mac.entry.and_then(|e| e.reason),
        Some("serial C02X1".to_owned())
    );
    assert_eq!(
        mac_role.record.map(|r| r.approve_until_unix),
        Some(NOW + 600)
    );

    unix.store(NOW + 599, Ordering::SeqCst);
    assert_eq!(listed(&rig, "db-1").state(), RecordState::Open);
    unix.store(NOW + 600, Ordering::SeqCst);
    let closed = listed(&rig, "db-1");
    assert_eq!(
        (closed.state(), closed.approve_until_unix),
        (RecordState::Closed, NOW + 600)
    );
    assert_eq!(listed(&rig, "mac-1").state(), RecordState::Closed);
}

/// Proves: the window an add asks for is its end or its length, never both, after now, and no
/// longer than the coordinator's most; an add that asks for a window for a record with no role is
/// refused too. Each refusal is `invalid_window` and changes nothing; the longest allowed window
/// and the shortest are taken as asked.
#[test]
fn an_add_names_its_window_by_end_or_length_within_the_most() {
    let (rig, _) = rig("window-add", RecordWindow::new(600, 3600).expect("limits"));
    let with = |host: &str, until: u64, secs: u64, role: &str| {
        let mut request = add_of(host, "0c:c4:7a:00:00:02");
        request.role = role.to_owned();
        request.approve_until_unix = until;
        request.approve_for_secs = secs;
        request
    };
    let before = audited(&rig);
    for (why, request) in [
        ("both", with("a-1", NOW + 60, 60, "storage")),
        ("an end that is now", with("a-2", NOW, 0, "storage")),
        ("an end before now", with("a-3", NOW - 1, 0, "storage")),
        (
            "an end past the most",
            with("a-4", NOW + 3601, 0, "storage"),
        ),
        ("a length past the most", with("a-5", 0, 3601, "storage")),
        ("an end for no role", with("a-6", NOW + 60, 0, "")),
        ("a length for no role", with("a-7", 0, 60, "")),
        ("both for no role", with("a-8", NOW + 60, 60, "")),
    ] {
        assert_eq!(
            add(&rig, request).err(),
            Some(Refusal::InvalidWindow),
            "{why}"
        );
    }
    assert_eq!(audited(&rig), before);
    assert!(rig.coordinator.ops.read(|s| s.inventory.is_empty()));

    for (host, request, end) in [
        ("b-1", with("b-1", NOW + 3600, 0, "storage"), NOW + 3600),
        ("b-2", with("b-2", NOW + 1, 0, "storage"), NOW + 1),
        ("b-3", with("b-3", 0, 3600, "storage"), NOW + 3600),
        ("b-4", with("b-4", 0, 1, "storage"), NOW + 1),
    ] {
        let mut request = request;
        request.data_macs = vec![format!("0c:c4:7a:00:01:{:02x}", host.as_bytes()[2])];
        let added = add(&rig, request).expect(host);
        assert_eq!(
            added.record.map(|r| r.approve_until_unix),
            Some(end),
            "{host}"
        );
    }
}

/// Proves: the limits are the coordinator's: the same end or length that one allows another
/// refuses, and the defaults are 24 hours and 7 days.
#[test]
fn the_limits_are_the_coordinators() {
    let default = RecordWindow::default();
    assert_eq!(
        (default.for_secs(), default.max_secs()),
        (DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS)
    );
    assert_eq!(
        (DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS),
        (86_400, 7 * 86_400)
    );
    let (rig, _) = default_rig("window-defaults");
    let mut week = add_of("db-1", "0c:c4:7a:00:00:02");
    week.approve_for_secs = DEFAULT_APPROVE_MAX_SECS;
    assert!(add(&rig, week).is_ok());
    let mut longer = add_of("db-2", "0c:c4:7a:00:00:03");
    longer.approve_for_secs = DEFAULT_APPROVE_MAX_SECS + 1;
    assert_eq!(add(&rig, longer).err(), Some(Refusal::InvalidWindow));
    assert_eq!(rig.coordinator.record_window(), default);

    assert_eq!(RecordWindow::new(60, 60).map(|w| w.for_secs()), Ok(60));
    assert_eq!(
        RecordWindow::new(61, 60),
        Err(WindowAboveMax {
            for_secs: 61,
            max_secs: 60
        })
    );
    let why = WindowAboveMax {
        for_secs: 61,
        max_secs: 60,
    }
    .to_string();
    assert!(
        why.contains("--record-approve-for") && why.contains("--record-approve-max"),
        "{why}"
    );
}

/// Proves: `OpenRecord` sets the window of an existing record from now (for the default length
/// when none is asked) and changes nothing else of it; it moves the end of a window still open,
/// earlier or later; and its audit line says the end, with the reason when there is one.
#[test]
fn open_record_sets_the_window_from_now_and_changes_nothing_else() {
    let (rig, unix) = rig("window-open", RecordWindow::new(600, 3600).expect("limits"));
    let mut roleless = add_of("mac-1", "0c:c4:7a:00:00:09");
    roleless.role = String::new();
    add(&rig, roleless).expect("added");
    add(&rig, add_of("db-1", "0c:c4:7a:00:00:02")).expect("added");
    let kept = rig
        .coordinator
        .ops
        .read(|s| s.inventory.iter().find(|r| r.host_id == "db-1").cloned())
        .expect("kept");

    unix.store(NOW + 1000, Ordering::SeqCst);
    assert_eq!(listed(&rig, "db-1").state(), RecordState::Closed);
    let reopened = open(
        &rig,
        proto::OpenRecordRequest {
            host_id: "db-1".to_owned(),
            ..proto::OpenRecordRequest::default()
        },
    )
    .expect("opened with the default length");
    let record = reopened.record.expect("a record");
    assert_eq!(
        (record.approve_until_unix, record.state()),
        (NOW + 1000 + 600, RecordState::Open)
    );
    let entry = reopened.entry.expect("an entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "record_open",
            Some("db-1"),
            Some(format!("until {}", NOW + 1600).as_str())
        )
    );
    let after = rig
        .coordinator
        .ops
        .read(|s| s.inventory.iter().find(|r| r.host_id == "db-1").cloned())
        .expect("kept");
    assert_eq!(
        after,
        InventoryRecord {
            approve_until_unix: NOW + 1600,
            ..kept
        },
        "only the window changed"
    );

    // An open window moves, later or earlier, and an end can be asked for.
    let later = open(&rig, open_for("db-1", 3600)).expect("moved later");
    assert_eq!(
        later.record.map(|r| r.approve_until_unix),
        Some(NOW + 1000 + 3600)
    );
    let earlier = open(
        &rig,
        proto::OpenRecordRequest {
            host_id: "db-1".to_owned(),
            approve_until_unix: NOW + 1000 + 30,
            reason: "  too long  ".to_owned(),
            ..proto::OpenRecordRequest::default()
        },
    )
    .expect("moved earlier");
    assert_eq!(
        earlier.record.map(|r| r.approve_until_unix),
        Some(NOW + 1030)
    );
    assert_eq!(
        earlier.entry.and_then(|e| e.reason),
        Some(format!("until {}; too long", NOW + 1030))
    );
}

/// Proves: `OpenRecord` refuses what it must and changes nothing: no operator (`unauthorized`,
/// `forbidden`), a host id outside its grammar, a reason with a control character, a host id no
/// record holds, a window both ways, not after now, or past the most, a record that names no role
/// (it has no window to open), and a record an approval used (`record_used`, whatever window it
/// asks for).
#[test]
fn open_record_refuses_what_it_must_and_changes_nothing() {
    let (rig, _) = rig(
        "window-open-refused",
        RecordWindow::new(600, 3600).expect("limits"),
    );
    add(&rig, add_of("db-1", "0c:c4:7a:00:00:02")).expect("added");
    let mut roleless = add_of("mac-1", "0c:c4:7a:00:00:09");
    roleless.role = String::new();
    add(&rig, roleless).expect("added");
    add(&rig, add_of("used-1", "0c:c4:7a:00:00:03")).expect("added");
    use_record(&rig, "used-1", &"a".repeat(64), NOW);
    let state = rig.coordinator.ops.state().inventory;
    let before = audited(&rig);

    let by = |caller: Caller| rig.coordinator.open_record(&caller, open_for("db-1", 60));
    assert_eq!(by(Caller::Anonymous).err(), Some(Refusal::Unauthorized));
    assert_eq!(by(Caller::Ci).err(), Some(Refusal::Forbidden));
    for (why, request, refusal) in [
        (
            "a host id outside the grammar",
            open_for("Not A Host!", 60),
            Refusal::InvalidHostId,
        ),
        ("no host id", open_for("", 60), Refusal::InvalidHostId),
        (
            "a reason with a control character",
            proto::OpenRecordRequest {
                reason: "bad\u{7}".to_owned(),
                ..open_for("db-1", 60)
            },
            Refusal::InvalidRequest,
        ),
        ("no such record", open_for("db-9", 60), Refusal::NoRecord),
        (
            "both",
            proto::OpenRecordRequest {
                approve_until_unix: NOW + 60,
                ..open_for("db-1", 60)
            },
            Refusal::InvalidWindow,
        ),
        (
            "an end that is now",
            proto::OpenRecordRequest {
                approve_until_unix: NOW,
                ..open_for("db-1", 0)
            },
            Refusal::InvalidWindow,
        ),
        (
            "an end past the most",
            proto::OpenRecordRequest {
                approve_until_unix: NOW + 3601,
                ..open_for("db-1", 0)
            },
            Refusal::InvalidWindow,
        ),
        (
            "a length past the most",
            open_for("db-1", 3601),
            Refusal::InvalidWindow,
        ),
        (
            "a record with no role",
            open_for("mac-1", 60),
            Refusal::InvalidWindow,
        ),
        ("a used record", open_for("used-1", 60), Refusal::RecordUsed),
        (
            "a used record and an impossible window",
            proto::OpenRecordRequest {
                approve_until_unix: 1,
                ..open_for("used-1", 60)
            },
            Refusal::RecordUsed,
        ),
    ] {
        assert_eq!(open(&rig, request).err(), Some(refusal), "{why}");
    }
    assert_eq!(rig.coordinator.ops.state().inventory, state);
    assert_eq!(audited(&rig), before);
}

/// Proves: `OpenRecord` writes nothing when the audit cannot be written
/// (`audit_unavailable`): the window stands as it was.
#[test]
fn an_open_whose_audit_line_cannot_be_written_changes_nothing() {
    let (mut rig, _) = default_rig("window-open-audit");
    add(&rig, add_of("db-1", "0c:c4:7a:00:00:02")).expect("added");
    let failing =
        ops_of(&rig.dir).with_append(Box::new(|_| Err(std::io::Error::other("disk full"))));
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(failing);
    let before = rig.coordinator.ops.state().inventory;

    assert_eq!(
        open(&rig, open_for("db-1", 60)).err(),
        Some(Refusal::AuditUnavailable)
    );
    assert_eq!(rig.coordinator.ops.state().inventory, before);
}

/// Proves: the list shows where each record's window stands at the time of the call: open before
/// its end, closed from it on and for a record with none, and used, with the request and the time,
/// whatever its window.
#[test]
fn the_list_shows_open_closed_and_used() {
    let (rig, unix) = rig(
        "window-listed",
        RecordWindow::new(600, 3600).expect("limits"),
    );
    add(&rig, add_of("a-open", "0c:c4:7a:00:00:01")).expect("added");
    let mut short = add_of("b-short", "0c:c4:7a:00:00:02");
    short.approve_for_secs = 10;
    add(&rig, short).expect("added");
    let mut roleless = add_of("c-none", "0c:c4:7a:00:00:03");
    roleless.role = String::new();
    add(&rig, roleless).expect("added");
    add(&rig, add_of("d-used", "0c:c4:7a:00:00:04")).expect("added");
    let request = "b".repeat(64);
    use_record(&rig, "d-used", &request, NOW + 5);
    unix.store(NOW + 20, Ordering::SeqCst);

    let shown: Vec<(String, RecordState, u64, String, u64)> = rig
        .coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
        .into_iter()
        .map(|r| {
            (
                r.host_id.clone(),
                r.state(),
                r.approve_until_unix,
                r.used_by_request_id.clone(),
                r.used_unix,
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "a-open".to_owned(),
                RecordState::Open,
                NOW + 600,
                String::new(),
                0
            ),
            (
                "b-short".to_owned(),
                RecordState::Closed,
                NOW + 10,
                String::new(),
                0
            ),
            (
                "c-none".to_owned(),
                RecordState::Closed,
                0,
                String::new(),
                0
            ),
            (
                "d-used".to_owned(),
                RecordState::Used,
                NOW + 600,
                request,
                NOW + 5
            ),
        ]
    );
    unix.store(NOW + 10_000, Ordering::SeqCst);
    assert_eq!(listed(&rig, "d-used").state(), RecordState::Used);
}

/// Proves: the ops file of the release before windows reads unchanged (its records are closed
/// and unused); a record with neither new key is written as it was, without them; the keys are
/// written only when set, and read back as written; and a key the file's record does not know
/// is still refused.
#[test]
fn the_ops_file_reads_a_record_without_the_new_keys_and_writes_them_only_when_set() {
    let dir = scratch("window-ops-file");
    let path = dir.path().join("desired.json.ops");
    fs::write(
        &path,
        r#"{"schema":1,"inventory":[{"host_id":"db-1","role":"storage","data_macs":["0c:c4:7a:00:00:02"],"by":"operator","at_unix":1799999999}]}"#,
    )
    .expect("written");
    let store = OpsStore::open(&path).expect("the file of the release before reads");
    let read = store.state().inventory;
    assert_eq!(read.len(), 1);
    assert_eq!(
        (read[0].approve_until_unix, read[0].used.clone()),
        (0, None)
    );

    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "record_add"),
            |state| {
                state.inventory.push(InventoryRecord {
                    host_id: "db-2".to_owned(),
                    role: "storage".to_owned(),
                    data_macs: vec!["0c:c4:7a:00:00:03".to_owned()],
                    bmc_mac: String::new(),
                    serial: String::new(),
                    notes: String::new(),
                    by: "operator".to_owned(),
                    at_unix: NOW,
                    platform: None,
                    approve_until_unix: NOW + 60,
                    used: Some(RecordUse {
                        request_id: "c".repeat(64),
                        at_unix: NOW + 1,
                    }),
                });
            },
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(
        written["inventory"],
        json!([
            {"host_id": "db-1", "role": "storage", "data_macs": ["0c:c4:7a:00:00:02"],
             "by": "operator", "at_unix": 1_799_999_999_u64},
            {"host_id": "db-2", "role": "storage", "data_macs": ["0c:c4:7a:00:00:03"],
             "by": "operator", "at_unix": NOW, "approve_until_unix": NOW + 60,
             "used": {"request_id": "c".repeat(64), "at_unix": NOW + 1}}
        ]),
        "{text}"
    );
    let reread = OpsStore::open(&path).expect("read back");
    assert_eq!(reread.state().inventory, store.state().inventory);

    for bad in [
        r#"{"host_id":"db-3","data_macs":["0c:c4:7a:00:00:04"],"by":"o","at_unix":1,"window":1}"#,
        r#"{"host_id":"db-3","data_macs":["0c:c4:7a:00:00:04"],"by":"o","at_unix":1,"used":{"request_id":"x","at_unix":1,"by":"y"}}"#,
    ] {
        fs::write(&path, format!(r#"{{"schema":1,"inventory":[{bad}]}}"#)).expect("written");
        assert!(OpsStore::open(&path).is_err(), "{bad}");
    }
}

/// Proves: marking a record used marks the first record of the host id that is unused, and
/// nothing when there is none or it is used already.
#[test]
fn marking_a_record_used_leaves_a_used_or_missing_record_alone() {
    let record = |host: &str| InventoryRecord {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: Vec::new(),
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: NOW,
        platform: None,
        approve_until_unix: NOW + 60,
        used: None,
    };
    let mut records = vec![record("db-1"), record("db-2")];

    mark_used(&mut records, "db-2", "r1", NOW + 1);
    mark_used(&mut records, "db-2", "r2", NOW + 2);
    mark_used(&mut records, "db-9", "r3", NOW + 3);

    assert_eq!(records[0].used, None);
    assert_eq!(
        records[1].used,
        Some(RecordUse {
            request_id: "r1".to_owned(),
            at_unix: NOW + 1
        })
    );
}

/// Proves: a record matches a request exactly only where the match is what the coordinator
/// approves by: a data MAC among the claimed addresses and, when the record holds a BMC MAC,
/// that BMC MAC claimed (a request that claims none matches the record in the list but not
/// exactly; one that claims another matches it not at all); a record without a BMC MAC is
/// matched exactly by any claim of a BMC MAC; a Mac is matched by its serial, in any case, and
/// its other claims do not decide; and a request that matches the record not at all matches it
/// not exactly.
#[test]
fn a_record_matches_exactly_only_what_the_coordinator_approves_by() {
    let record = |data: &[&str], bmc: &str, serial: &str| InventoryRecord {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        serial: serial.to_owned(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: NOW,
        platform: None,
        approve_until_unix: NOW + 60,
        used: None,
    };
    let claims = |addresses: &[&str], bmc: &str, mac_serial: &str| Claims {
        addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        mac_serial: mac_serial.to_owned(),
        ..Claims::default()
    };
    const DATA: &str = "0c:c4:7a:00:00:02";
    const BMC: &str = "0c:c4:7a:00:00:01";
    for (why, record, claims, matched, exactly) in [
        (
            "the data MAC and the BMC MAC",
            record(&[DATA], BMC, ""),
            claims(&[DATA], "0C-C4-7A-00-00-01", ""),
            true,
            true,
        ),
        (
            "the data MAC and no BMC MAC claimed",
            record(&[DATA], BMC, ""),
            claims(&[DATA], "", ""),
            true,
            false,
        ),
        (
            "the data MAC and another BMC MAC",
            record(&[DATA], BMC, ""),
            claims(&[DATA], "0c:c4:7a:00:00:99", ""),
            false,
            false,
        ),
        (
            "a record with no BMC MAC and a BMC MAC claimed",
            record(&[DATA], "", ""),
            claims(&[DATA], BMC, ""),
            true,
            true,
        ),
        (
            "a record with no BMC MAC and none claimed",
            record(&[DATA], "", ""),
            claims(&[DATA], "", ""),
            true,
            true,
        ),
        (
            "another data MAC",
            record(&[DATA], BMC, ""),
            claims(&["0c:c4:7a:00:00:77"], BMC, ""),
            false,
            false,
        ),
        (
            "a Mac's serial in another case, its BMC claim not decided",
            record(&[], BMC, "C02X1"),
            claims(&[], "", "c02x1"),
            true,
            true,
        ),
        (
            "another Mac serial",
            record(&[], "", "C02X1"),
            claims(&[], "", "C02X2"),
            false,
            false,
        ),
    ] {
        let found = match_claims(&claims, std::slice::from_ref(&record));
        assert_eq!(
            matches!(found, InventoryMatch::Matched(_)),
            matched,
            "{why}: matched"
        );
        assert_eq!(record.matches_exactly(&claims), exactly, "{why}: exactly");
    }
}
