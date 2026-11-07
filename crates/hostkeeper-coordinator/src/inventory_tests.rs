//! The host inventory's calls and its place in the ops file: `Enrollment.AddRecord`,
//! `ListRecords` and `RemoveRecord` refuse what they must, keep one spelling of each MAC, keep
//! records apart by host id, MAC and serial, page by host id, survive a restart, change under
//! the ops file's lock, and leave a file the release before reads while there is no record.

use std::fs;
use std::sync::Barrier;

use hostkeeper_enrollment::enroll::Claims;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::Value;

use crate::holds_calls_tests::PreviousOps;
use crate::inventory::{
    DEFAULT_APPROVE_FOR_SECS, InventoryMatch, InventoryRecord, MAX_DATA_MACS, MAX_RECORDS,
    match_claims,
};
use crate::marks_support::{operator, restart, rig};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsState, OpsStore, Write};
use crate::rollback_tests::unwritable;
use crate::testutil::{Rig, scratch};

use proto::EnrollmentRefusal as Refusal;

/// An add of `host` with the data MAC `data` and the BMC MAC `bmc`, in the storage role.
fn add_of(host: &str, data: &[&str], bmc: &str) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        ..proto::AddRecordRequest::default()
    }
}

fn add(rig: &Rig, request: proto::AddRecordRequest) -> Result<proto::AddRecordResponse, Refusal> {
    rig.coordinator.add_record(&operator(), request)
}

fn list(rig: &Rig, limit: u32, token: &str) -> Result<proto::ListRecordsResponse, Refusal> {
    rig.coordinator.list_records(
        &operator(),
        proto::ListRecordsRequest {
            limit,
            page_token: token.to_owned(),
            enrolled: None,
        },
    )
}

fn remove(rig: &Rig, host: &str, reason: &str) -> Result<proto::RemoveRecordResponse, Refusal> {
    rig.coordinator.remove_record(
        &operator(),
        proto::RemoveRecordRequest {
            host_id: host.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// The host ids of the records the coordinator lists, all pages.
fn hosts(rig: &Rig) -> Vec<String> {
    list(rig, 0, "")
        .expect("listed")
        .records
        .into_iter()
        .map(|r| r.host_id)
        .collect()
}

/// A record as the ops file keeps it.
fn record(host: &str, data: &[&str], bmc: &str, serial: &str) -> InventoryRecord {
    InventoryRecord {
        host_id: host.to_owned(),
        role: String::new(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        serial: serial.to_owned(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: 1_800_000_000,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }
}

/// A Linux host's claims: its interface addresses and its BMC MAC (empty for none).
fn linux(addresses: &[&str], bmc: &str) -> Claims {
    Claims {
        addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        ..Claims::default()
    }
}

/// Catches: a record kept in the spelling the operator typed (so `AA-BB-..` never meets the
/// host's `aa:bb:..`), a duplicate data MAC kept twice, an add that reaches neither the list nor
/// the audit, and the operator's name or the time left off the record.
#[test]
fn an_added_record_keeps_one_spelling_of_each_mac_and_is_listed_and_audited() {
    let rig = rig("inventory-add", "");
    let mut request = add_of(
        "db-1",
        &["0C-C4-7A-00-00-02", "0c:c4:7a:00:00:02", "0CC47A000003"],
        "0C:C4:7A:00:00:01",
    );
    request.serial = " SN-1 ".to_owned();
    request.notes = "  rack 4, slot 2 ".to_owned();
    let added = add(&rig, request).expect("added");
    let added_unix = added.record.as_ref().map_or(0, |r| r.added_unix);
    let want = proto::InventoryRecord {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![
            "0c:c4:7a:00:00:02".to_owned(),
            "0c:c4:7a:00:00:03".to_owned(),
        ],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
        added_by: "operator".to_owned(),
        added_unix,
        enrolled: false,
        host_held: proto::HostHeld::Unspecified.into(),
        approve_until_unix: added_unix + DEFAULT_APPROVE_FOR_SECS,
        state: proto::RecordState::Open.into(),
        used_by_request_id: String::new(),
        used_unix: 0,
        platform: 0,
    };
    assert_eq!(added.record.as_ref(), Some(&want));
    let entry = added.entry.expect("an audit entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "record_add",
            Some("db-1"),
            Some(
                format!(
                    "role storage; data 0c:c4:7a:00:00:02 0c:c4:7a:00:00:03; \
                     bmc 0c:c4:7a:00:00:01; serial SN-1; approve until {}",
                    added_unix + DEFAULT_APPROVE_FOR_SECS
                )
                .as_str()
            )
        )
    );
    assert_eq!(entry.by, proto::Actor::Operator as i32);
    assert_eq!(list(&rig, 0, "").expect("listed").records, vec![want]);

    // A Mac's record: a serial, no MAC, no role; its audit line says only the serial.
    let mac = proto::AddRecordRequest {
        host_id: "mac-1".to_owned(),
        serial: "C02X1".to_owned(),
        ..proto::AddRecordRequest::default()
    };
    let added = add(&rig, mac).expect("a serial alone is a record");
    assert_eq!(
        added.entry.and_then(|e| e.reason).as_deref(),
        Some("serial C02X1")
    );
    assert_eq!(hosts(&rig), ["db-1", "mac-1"]);
}

/// Catches: a record that cannot identify a server (no data MAC and no serial), a field let
/// through outside its grammar, more data MACs than a record holds, and a BMC MAC that is also a
/// data MAC (which would match a request through its BMC claim alone). Each refusal changes
/// nothing.
#[test]
fn an_add_outside_the_grammar_is_refused_and_changes_nothing() {
    let rig = rig("inventory-add-refused", "");
    let good = || add_of("db-1", &["0c:c4:7a:00:00:02"], "");
    let many: Vec<String> = (0..=MAX_DATA_MACS)
        .map(|n| format!("0c:c4:7a:00:01:{n:02x}"))
        .collect();
    let cases: Vec<(proto::AddRecordRequest, Refusal)> = vec![
        (
            proto::AddRecordRequest {
                host_id: "DB_1".to_owned(),
                ..good()
            },
            Refusal::InvalidHostId,
        ),
        (
            proto::AddRecordRequest {
                role: "Storage".to_owned(),
                ..good()
            },
            Refusal::InvalidRole,
        ),
        (
            add_of("db-1", &["0c:c4:7a:00:00"], ""),
            Refusal::InvalidRequest,
        ),
        (
            add_of("db-1", &["0c:c4:7a-00-00-02"], ""),
            Refusal::InvalidRequest,
        ),
        (
            add_of("db-1", &["0c:c4:7a:00:00:02"], "zz:c4:7a:00:00:01"),
            Refusal::InvalidRequest,
        ),
        (
            add_of("db-1", &["0c:c4:7a:00:00:02"], "0C-C4-7A-00-00-02"),
            Refusal::InvalidRequest,
        ),
        (add_of("db-1", &[], ""), Refusal::InvalidRequest),
        (
            proto::AddRecordRequest {
                data_macs: many,
                ..good()
            },
            Refusal::InvalidRequest,
        ),
        (
            proto::AddRecordRequest {
                serial: "SN 1".to_owned(),
                ..good()
            },
            Refusal::InvalidRequest,
        ),
        (
            proto::AddRecordRequest {
                serial: "S".repeat(65),
                ..good()
            },
            Refusal::InvalidRequest,
        ),
        (
            proto::AddRecordRequest {
                notes: "line\nbreak".to_owned(),
                ..good()
            },
            Refusal::InvalidRequest,
        ),
    ];
    for (request, refusal) in cases {
        assert_eq!(
            add(&rig, request.clone()).err(),
            Some(refusal),
            "{request:?}"
        );
    }
    let sixteen: Vec<String> = (0..MAX_DATA_MACS)
        .map(|n| format!("0c:c4:7a:00:01:{n:02x}"))
        .collect();
    let at_most = proto::AddRecordRequest {
        data_macs: sixteen,
        serial: "S".repeat(64),
        ..good()
    };
    add(&rig, at_most).expect("16 data MACs and a 64-byte serial are a record");
    assert_eq!(hosts(&rig), ["db-1"]);
    assert_eq!(rig.coordinator.ops.state().audit.len(), 1);
}

/// Catches: a second record for one host id, or a record that shares a MAC (data or BMC, in any
/// spelling, against the other's data or BMC MAC) or a serial (in any case) with another, which
/// would make one machine match two records forever.
#[test]
fn a_record_cannot_share_a_host_id_a_mac_or_a_serial_with_another() {
    let rig = rig("inventory-conflict", "");
    let mut first = add_of("db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01");
    first.serial = "sn-1".to_owned();
    add(&rig, first).expect("added");
    let cases = [
        (
            add_of("db-1", &["0c:c4:7a:00:00:09"], ""),
            Refusal::RecordExists,
        ),
        (
            add_of("db-2", &["0C-C4-7A-00-00-02"], ""),
            Refusal::RecordConflict,
        ),
        (
            add_of("db-2", &["0c:c4:7a:00:00:01"], ""),
            Refusal::RecordConflict,
        ),
        (
            add_of("db-2", &["0c:c4:7a:00:00:09"], "0c:c4:7a:00:00:02"),
            Refusal::RecordConflict,
        ),
        (
            add_of("db-2", &["0c:c4:7a:00:00:09"], "0c:c4:7a:00:00:01"),
            Refusal::RecordConflict,
        ),
        (
            proto::AddRecordRequest {
                serial: "SN-1".to_owned(),
                ..add_of("db-2", &["0c:c4:7a:00:00:09"], "")
            },
            Refusal::RecordConflict,
        ),
    ];
    for (request, refusal) in cases {
        assert_eq!(
            add(&rig, request.clone()).err(),
            Some(refusal),
            "{request:?}"
        );
    }
    add(&rig, add_of("db-2", &["0c:c4:7a:00:00:09"], "")).expect("nothing shared");
    assert_eq!(hosts(&rig), ["db-1", "db-2"]);
}

/// Catches: a remove of a record that is not there answered as done, a remove without a reason
/// or of a host id outside the grammar, and a removed record that still blocks its host id or
/// its MACs.
#[test]
fn a_removed_record_frees_its_host_id_and_its_macs() {
    let rig = rig("inventory-remove", "");
    add(
        &rig,
        add_of("db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01"),
    )
    .expect("added");
    assert_eq!(
        remove(&rig, "db-1", " ").err(),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        remove(&rig, "DB-1", "gone").err(),
        Some(Refusal::InvalidHostId)
    );
    assert_eq!(remove(&rig, "db-2", "gone").err(), Some(Refusal::NoRecord));
    let removed = remove(&rig, "db-1", "returned to the vendor").expect("removed");
    let entry = removed.entry.expect("an audit entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "record_remove",
            Some("db-1"),
            Some("returned to the vendor")
        )
    );
    assert_eq!(hosts(&rig), Vec::<String>::new());
    assert_eq!(remove(&rig, "db-1", "again").err(), Some(Refusal::NoRecord));
    add(
        &rig,
        add_of("db-3", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01"),
    )
    .expect("the MACs are free again");
}

/// Catches: a page that repeats or skips a record, a next-page token on the last page, a limit
/// above the cap honoured, and a page token that is no host id let through.
#[test]
fn records_page_by_host_id() {
    let rig = rig("inventory-pages", "");
    for n in [3, 1, 2] {
        add(
            &rig,
            add_of(&format!("db-{n}"), &[&format!("0c:c4:7a:00:00:0{n}")], ""),
        )
        .expect("added");
    }
    let first = list(&rig, 2, "").expect("a page");
    let ids: Vec<&str> = first.records.iter().map(|r| r.host_id.as_str()).collect();
    assert_eq!(
        (ids, first.next_page_token.as_str()),
        (vec!["db-1", "db-2"], "db-2")
    );
    let second = list(&rig, 2, &first.next_page_token).expect("a page");
    let ids: Vec<&str> = second.records.iter().map(|r| r.host_id.as_str()).collect();
    assert_eq!((ids, second.next_page_token.as_str()), (vec!["db-3"], ""));
    let exact = list(&rig, 3, "").expect("a page");
    assert_eq!(
        (exact.records.len(), exact.next_page_token.as_str()),
        (3, "")
    );
    let past = list(&rig, u32::MAX, "db-3").expect("past the end");
    assert_eq!((past.records.len(), past.next_page_token.as_str()), (0, ""));
    assert_eq!(list(&rig, 0, "DB 1").err(), Some(Refusal::InvalidRequest));
}

/// Catches: an inventory call that a CI token, or no token, reaches; each is refused before the
/// request is read, as every operator call is.
#[test]
fn the_record_calls_take_the_operator_token_only() {
    let rig = rig("inventory-callers", "");
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        let added = rig
            .coordinator
            .add_record(&caller, add_of("db-1", &["0c:c4:7a:00:00:02"], ""));
        assert_eq!(added.err(), Some(refusal));
        let listed = rig
            .coordinator
            .list_records(&caller, proto::ListRecordsRequest::default());
        assert_eq!(listed.err(), Some(refusal));
        let removed = rig
            .coordinator
            .remove_record(&caller, proto::RemoveRecordRequest::default());
        assert_eq!(removed.err(), Some(refusal));
        let pending = rig
            .coordinator
            .enrollment_list(&caller, &proto::ListPendingRequest::default());
        assert_eq!(pending.err(), Some(refusal));
    }
}

/// Catches: a record that is in memory when the ops file could not be written (so a restart
/// would lose it), or a remove that stands in memory after its write failed.
#[test]
fn a_failed_write_keeps_the_records_as_they_were() {
    let mut state = OpsState::default();
    state
        .inventory
        .push(record("db-1", &["0c:c4:7a:00:00:02"], "", ""));
    let mut rig = Rig::new("inventory-unwritable");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_ops(unwritable(state));
    assert_eq!(
        add(&rig, add_of("db-2", &["0c:c4:7a:00:00:03"], "")).err(),
        Some(Refusal::StoreFailed)
    );
    assert_eq!(
        remove(&rig, "db-1", "gone").err(),
        Some(Refusal::StoreFailed)
    );
    assert_eq!(hosts(&rig), ["db-1"]);
}

/// Catches: records that a restart loses, or reads back in another order or spelling.
#[test]
fn records_survive_a_restart() {
    let mut rig = rig("inventory-restart", "");
    add(&rig, add_of("db-2", &["0C-C4-7A-00-00-02"], "")).expect("added");
    add(&rig, add_of("db-1", &["0c:c4:7a:00:00:01"], "")).expect("added");
    let before = list(&rig, 0, "").expect("listed");
    restart(&mut rig, "");
    assert_eq!(list(&rig, 0, "").expect("listed"), before);
    assert_eq!(hosts(&rig), ["db-1", "db-2"]);
}

/// Catches: two operators' edits racing past each other: a record lost because two adds each
/// wrote the file from what they read before the other's write, two records for one host id
/// because both checked before either wrote, or an audit number given twice. Every call starts
/// at one barrier; the ops file's lock must make them one at a time.
#[test]
fn concurrent_record_edits_keep_every_record_and_refuse_the_second_of_one_host_id() {
    let mut rig = rig("inventory-concurrent", "");
    add(&rig, add_of("old-1", &["0c:c4:7a:00:0f:01"], "")).expect("added");
    const DISTINCT: usize = 12;
    const SAME: usize = 6;
    let barrier = Barrier::new(DISTINCT + SAME + 1);
    let coordinator = &rig.coordinator;
    let (distinct, same, removed) = std::thread::scope(|scope| {
        let distinct: Vec<_> = (0..DISTINCT)
            .map(|n| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    coordinator.add_record(
                        &operator(),
                        add_of(
                            &format!("db-{n}"),
                            &[&format!("0c:c4:7a:00:01:{n:02x}")],
                            "",
                        ),
                    )
                })
            })
            .collect();
        let same: Vec<_> = (0..SAME)
            .map(|n| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    coordinator.add_record(
                        &operator(),
                        add_of("twin", &[&format!("0c:c4:7a:00:02:{n:02x}")], ""),
                    )
                })
            })
            .collect();
        let remover = scope.spawn(|| {
            barrier.wait();
            coordinator.remove_record(
                &operator(),
                proto::RemoveRecordRequest {
                    host_id: "old-1".to_owned(),
                    reason: "rebuilt".to_owned(),
                },
            )
        });
        let join = |handles: Vec<std::thread::ScopedJoinHandle<'_, _>>| -> Vec<_> {
            handles
                .into_iter()
                .map(|h| h.join().expect("joined"))
                .collect()
        };
        (join(distinct), join(same), remover.join().expect("joined"))
    });
    assert!(distinct.iter().all(Result::is_ok), "{distinct:?}");
    let won: Vec<_> = same.iter().filter(|r| r.is_ok()).collect();
    let lost: Vec<_> = same.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(won.len(), 1, "{same:?}");
    assert_eq!(lost, vec![&Refusal::RecordExists; SAME - 1]);
    assert!(removed.is_ok());

    let mut want: Vec<String> = (0..DISTINCT).map(|n| format!("db-{n}")).collect();
    want.push("twin".to_owned());
    want.sort();
    assert_eq!(hosts(&rig), want);
    let state = rig.coordinator.ops.state();
    let mut seqs: Vec<u64> = state.audit.iter().map(|e| e.seq).collect();
    let count = seqs.len();
    seqs.dedup();
    assert_eq!((count, seqs.len()), (DISTINCT + 1 + 2, DISTINCT + 1 + 2));
    restart(&mut rig, "");
    assert_eq!(hosts(&rig), want, "every record reached the file");
}

/// Catches: a farm that never records a server writing an `inventory` key the release before
/// refuses (so it could not roll back), and a record written where that release would ignore it
/// rather than refuse the file. The reader of the release before is the strict one of
/// `holds_calls_tests`.
#[test]
fn the_ops_file_has_no_inventory_key_until_a_record_is_added() {
    let dir = scratch("inventory-previous-ops");
    let path = dir.path().join("desired.json.ops");
    let store = OpsStore::open(&path).expect("a new file");
    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "exclude"),
            |_| {},
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert!(written.get("inventory").is_none(), "{text}");
    PreviousOps::read(&text).expect("the previous release reads a file without records");

    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "record_add"),
            |state| {
                state
                    .inventory
                    .push(record("db-1", &["0c:c4:7a:00:00:02"], "", ""));
            },
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(
        written["inventory"],
        serde_json::json!([{"host_id": "db-1", "data_macs": ["0c:c4:7a:00:00:02"],
                            "by": "operator", "at_unix": 1_800_000_000}]),
        "{text}"
    );
    let reread = OpsStore::open(&path).expect("this build reads it back");
    assert_eq!(
        reread.state().inventory,
        vec![record("db-1", &["0c:c4:7a:00:00:02"], "", "")]
    );
    let why = PreviousOps::read(&text).expect_err("the previous release refuses a record");
    assert!(why.contains("inventory"), "{why}");
}

/// Catches: a capacity check that lets the file grow past [`MAX_RECORDS`].
#[test]
fn an_add_past_the_cap_is_refused() {
    let mut state = OpsState::default();
    state.inventory = (0..MAX_RECORDS)
        .map(|n| record(&format!("h-{n}"), &[], "", &format!("s{n}")))
        .collect();
    let mut rig = Rig::new("inventory-cap");
    rig.coordinator =
        Rig::start(&rig.dir, &rig.clock).with_ops(OpsStore::refusing(state, Box::new(|_| Ok(()))));
    assert_eq!(
        add(&rig, add_of("db-1", &["0c:c4:7a:00:00:02"], "")).err(),
        Some(Refusal::Capacity)
    );
}

/// Catches: a match that guesses. A request matches a record by a data MAC, in any spelling,
/// with no BMC claim or the record's own; a BMC claim alone, or one that contradicts the record,
/// is no match; two records matching is ambiguous with both named; a Mac matches by serial
/// without case, never by MAC; an address that is no MAC matches nothing.
#[test]
fn a_request_matches_one_record_none_or_several_and_is_never_guessed() {
    let records = vec![
        record("db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01", ""),
        record("db-2", &["0c:c4:7a:00:00:12"], "0c:c4:7a:00:00:11", ""),
        record("mac-1", &[], "", "C02X1"),
    ];
    let matched = |claims: &Claims| match match_claims(claims, &records) {
        InventoryMatch::Matched(r) => Some(r.host_id.clone()),
        _ => None,
    };
    // Exact: data MAC and BMC MAC both the record's.
    assert_eq!(
        matched(&linux(&["0C-C4-7A-00-00-02"], "0C:C4:7A:00:00:01")),
        Some("db-1".to_owned())
    );
    // A missing BMC claim still matches on the data MAC; another interface does not stop it.
    assert_eq!(
        matched(&linux(&["02:42:ac:11:00:02", "0c:c4:7a:00:00:02"], "")),
        Some("db-1".to_owned())
    );
    // An unreadable BMC claim is no claim.
    assert_eq!(
        matched(&linux(&["0c:c4:7a:00:00:02"], "not a mac")),
        Some("db-1".to_owned())
    );
    for claims in [
        linux(&["0c:c4:7a:00:00:99"], ""),
        linux(&[], "0c:c4:7a:00:00:01"),
        linux(&["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:11"),
        linux(
            &["80:00:02:08:fe:80:00:00:00:00:00:00:0c:c4:7a:00:00:02"],
            "",
        ),
        linux(&[], ""),
    ] {
        assert_eq!(
            match_claims(&claims, &records),
            InventoryMatch::Unmatched,
            "{claims:?}"
        );
    }
    assert_eq!(
        match_claims(
            &linux(&["0c:c4:7a:00:00:12", "0c:c4:7a:00:00:02"], ""),
            &records
        ),
        InventoryMatch::Ambiguous(vec!["db-1".to_owned(), "db-2".to_owned()])
    );
    let mac = |serial: &str, addresses: &[&str]| Claims {
        mac_serial: serial.to_owned(),
        ..linux(addresses, "")
    };
    assert_eq!(matched(&mac("c02x1", &[])), Some("mac-1".to_owned()));
    assert_eq!(
        match_claims(&mac("C02X9", &["0c:c4:7a:00:00:02"]), &records),
        InventoryMatch::Unmatched
    );
    let serial_less = [record("db-1", &["0c:c4:7a:00:00:02"], "", "")];
    assert_eq!(
        match_claims(&mac("C02X1", &[]), &serial_less),
        InventoryMatch::Unmatched
    );
}

/// Catches: a record the operator left without a BMC MAC refusing a host that reports one (the
/// claim was compared with the empty text), a record's own BMC MAC no longer refusing another
/// BMC, and a record with every field empty matching a host.
#[test]
fn a_field_absent_from_the_record_does_not_constrain_the_match() {
    let no_bmc = vec![record("db-1", &["0c:c4:7a:00:00:02"], "", "")];
    assert!(matches!(
        match_claims(&linux(&["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01"), &no_bmc),
        InventoryMatch::Matched(r) if r.host_id == "db-1"
    ));
    let with_bmc = vec![record(
        "db-1",
        &["0c:c4:7a:00:00:02"],
        "0c:c4:7a:00:00:01",
        "",
    )];
    assert_eq!(
        match_claims(
            &linux(&["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:11"),
            &with_bmc
        ),
        InventoryMatch::Unmatched
    );
    let empty = vec![record("db-1", &[], "", "")];
    for claims in [
        linux(&["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01"),
        linux(&["0c:c4:7a:00:00:02"], ""),
        linux(&[], "0c:c4:7a:00:00:01"),
        linux(&[], ""),
        Claims {
            mac_serial: "C02X1".to_owned(),
            ..Claims::default()
        },
    ] {
        assert_eq!(
            match_claims(&claims, &empty),
            InventoryMatch::Unmatched,
            "{claims:?}"
        );
    }
}
