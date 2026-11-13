//! The approval window of an inventory record, added after `host_held`: `InventoryRecord` fields
//! 11 to 14 and its enum `RecordState`, `PendingEnrollment` fields 20 and 21, `AddRecordRequest`
//! fields 7 and 8, the refusals `invalid_window` and `record_used`, and the messages of
//! `Enrollment.OpenRecord`. A message written before the fields reads with every field it knew
//! and the new ones zero; a reader before them keeps them and writes them back unchanged; the
//! numbers are pinned. The baseline (`proto/compat/baseline.binpb`) holds none of these
//! messages, so each message before the fields is spelled here as a message of its own.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `InventoryRecord` as it was before the window: fields 1 to 10.
#[derive(Clone, PartialEq, prost::Message)]
struct RecordBefore {
    #[prost(string, tag = "1")]
    host_id: String,
    #[prost(string, tag = "2")]
    role: String,
    #[prost(string, repeated, tag = "3")]
    data_macs: Vec<String>,
    #[prost(string, tag = "4")]
    bmc_mac: String,
    #[prost(string, tag = "5")]
    serial: String,
    #[prost(string, tag = "6")]
    notes: String,
    #[prost(string, tag = "7")]
    added_by: String,
    #[prost(uint64, tag = "8")]
    added_unix: u64,
    #[prost(bool, tag = "9")]
    enrolled: bool,
    #[prost(int32, tag = "10")]
    host_held: i32,
}

/// `PendingEnrollment` as it was before the window: the fields this test carries of 1 to 19.
#[derive(Clone, PartialEq, prost::Message)]
struct PendingBefore {
    #[prost(string, tag = "1")]
    request_id: String,
    #[prost(string, tag = "10")]
    record_host_id: String,
    #[prost(string, tag = "11")]
    record_role: String,
    #[prost(string, repeated, tag = "17")]
    contested_by: Vec<String>,
    #[prost(string, tag = "19")]
    record_bmc_mac: String,
}

/// `AddRecordRequest` as it was before the window: fields 1 to 6.
#[derive(Clone, PartialEq, prost::Message)]
struct AddBefore {
    #[prost(string, tag = "1")]
    host_id: String,
    #[prost(string, tag = "2")]
    role: String,
    #[prost(string, repeated, tag = "3")]
    data_macs: Vec<String>,
    #[prost(string, tag = "4")]
    bmc_mac: String,
    #[prost(string, tag = "5")]
    serial: String,
    #[prost(string, tag = "6")]
    notes: String,
}

fn record_before() -> RecordBefore {
    RecordBefore {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
        added_by: "alice".to_owned(),
        added_unix: 1_800_000_000,
        enrolled: false,
        host_held: op::HostHeld::None.into(),
    }
}

fn pending_before() -> PendingBefore {
    PendingBefore {
        request_id: "ab".repeat(32),
        record_host_id: "db-1".to_owned(),
        record_role: "storage".to_owned(),
        contested_by: vec!["2e".repeat(32)],
        record_bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
    }
}

fn add_before() -> AddBefore {
    AddBefore {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
    }
}

/// An `InventoryRecord` with every field set, the window's too.
fn record_now() -> op::InventoryRecord {
    op::InventoryRecord {
        enrolled_host_id: String::new(),
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
        added_by: "alice".to_owned(),
        added_unix: 1_800_000_000,
        enrolled: true,
        host_held: op::HostHeld::Approved.into(),
        approve_until_unix: 1_800_086_400,
        state: op::RecordState::Used.into(),
        used_by_request_id: "ab".repeat(32),
        used_unix: 1_800_000_060,
        platform: 0,
        host_group: String::new(),
    }
}

/// Proves: a record the release before wrote (no fields 11 to 14) reads with every field it
/// carried, no window, state `UNSPECIFIED` (not `CLOSED`), and no use.
#[test]
fn a_record_written_before_the_window_reads_with_no_window_and_no_state() {
    let read =
        op::InventoryRecord::decode(record_before().encode_to_vec().as_slice()).expect("decodes");
    let before = record_before();
    assert_eq!(read.host_id, before.host_id);
    assert_eq!(read.role, before.role);
    assert_eq!(read.data_macs, before.data_macs);
    assert_eq!(read.bmc_mac, before.bmc_mac);
    assert_eq!(read.serial, before.serial);
    assert_eq!(read.notes, before.notes);
    assert_eq!(read.added_by, before.added_by);
    assert_eq!(read.added_unix, before.added_unix);
    assert_eq!(read.host_held(), op::HostHeld::None);
    assert_eq!(read.approve_until_unix, 0);
    assert_eq!(read.state(), op::RecordState::Unspecified);
    assert_ne!(read.state(), op::RecordState::Closed);
    assert_eq!(read.used_by_request_id, "");
    assert_eq!(read.used_unix, 0);
}

/// Proves: a reader of the record before the window reads every field it knows from a record
/// that carries the window, and a pending request or an add request written before the fields
/// reads with the new ones zero.
#[test]
fn a_reader_before_the_window_reads_every_field_it_knows() {
    let bytes = record_now().encode_to_vec();
    let older = RecordBefore::decode(bytes.as_slice()).expect("an older reader decodes");
    assert_eq!(
        older,
        RecordBefore {
            enrolled: true,
            host_held: op::HostHeld::Approved.into(),
            ..record_before()
        }
    );

    let pending = op::PendingEnrollment {
        request_id: "ab".repeat(32),
        record_host_id: "db-1".to_owned(),
        record_role: "storage".to_owned(),
        contested_by: vec!["2e".repeat(32)],
        record_bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        record_state: op::RecordState::Open.into(),
        record_approve_until_unix: 1_800_086_400,
        ..op::PendingEnrollment::default()
    };
    let bytes = pending.encode_to_vec();
    assert_eq!(
        PendingBefore::decode(bytes.as_slice()).expect("an older reader decodes"),
        pending_before()
    );
    let read = op::PendingEnrollment::decode(pending_before().encode_to_vec().as_slice())
        .expect("decodes");
    assert_eq!(read.record_host_id, "db-1");
    assert_eq!(read.record_state(), op::RecordState::Unspecified);
    assert_eq!(read.record_approve_until_unix, 0);

    let add = op::AddRecordRequest {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
        approve_until_unix: 1_800_086_400,
        approve_for_secs: 0,
        platform: 0,
        host_group: String::new(),
    };
    assert_eq!(
        AddBefore::decode(add.encode_to_vec().as_slice()).expect("an older reader decodes"),
        add_before()
    );
    let read =
        op::AddRecordRequest::decode(add_before().encode_to_vec().as_slice()).expect("decodes");
    assert_eq!((read.approve_until_unix, read.approve_for_secs), (0, 0));
    assert_eq!(read.host_id, "db-1");
}

/// Proves: a reader of the release before, built from the baseline, which holds none of these
/// messages, keeps the window fields it does not know: an `InventoryRecord`, a
/// `PendingEnrollment` and an `AddRecordRequest` read as the baseline's message with no field
/// are written back byte for byte.
#[test]
fn an_older_reader_keeps_the_window_fields_and_writes_them_back_unchanged() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    for name in [
        "InventoryRecord",
        "PendingEnrollment",
        "AddRecordRequest",
        "OpenRecordRequest",
        "OpenRecordResponse",
    ] {
        assert!(
            older
                .get_message_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name} is new"
        );
    }
    let pending = op::PendingEnrollment {
        request_id: "ab".repeat(32),
        record_state: op::RecordState::Closed.into(),
        record_approve_until_unix: 1_800_086_400,
        ..op::PendingEnrollment::default()
    };
    let add = op::AddRecordRequest {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        approve_for_secs: 3_600,
        ..op::AddRecordRequest::default()
    };
    let open = op::OpenRecordRequest {
        host_id: "db-1".to_owned(),
        approve_until_unix: 1_800_086_400,
        approve_for_secs: 0,
        reason: "rack was late".to_owned(),
    };
    let answer = op::OpenRecordResponse {
        record: Some(record_now()),
        entry: Some(op::AuditEntry {
            seq: 3,
            action: "record_open".to_owned(),
            ..op::AuditEntry::default()
        }),
    };
    let encoded = [
        record_now().encode_to_vec(),
        pending.encode_to_vec(),
        add.encode_to_vec(),
        open.encode_to_vec(),
        answer.encode_to_vec(),
    ];
    for bytes in encoded {
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes);
    }
}

/// Proves: `OpenRecordRequest` and `OpenRecordResponse` read back as written, each field set.
#[test]
fn open_record_messages_round_trip() {
    let request = op::OpenRecordRequest {
        host_id: "db-1".to_owned(),
        approve_until_unix: 0,
        approve_for_secs: 7_200,
        reason: "rack was late".to_owned(),
    };
    assert_eq!(
        op::OpenRecordRequest::decode(request.encode_to_vec().as_slice()).expect("decodes"),
        request
    );
    let answer = op::OpenRecordResponse {
        record: Some(record_now()),
        entry: Some(op::AuditEntry {
            seq: 4,
            action: "record_open".to_owned(),
            node_id: Some("db-1".to_owned()),
            reason: Some("until 1800086400; rack was late".to_owned()),
            ..op::AuditEntry::default()
        }),
    };
    assert_eq!(
        op::OpenRecordResponse::decode(answer.encode_to_vec().as_slice()).expect("decodes"),
        answer
    );
}

/// Proves: the numbers of `RecordState` are 0 to 3 in the order the proto gives them, the two
/// new refusals are 22 and 23 after `record_contested` (21), and a used record writes field 12
/// as the varint 3.
#[test]
fn record_state_and_refusal_numbers_are_pinned() {
    let states = [
        (op::RecordState::Unspecified, 0),
        (op::RecordState::Open, 1),
        (op::RecordState::Closed, 2),
        (op::RecordState::Used, 3),
    ];
    for (state, number) in states {
        assert_eq!(state as i32, number, "{state:?}");
    }
    assert_eq!(op::EnrollmentRefusal::RecordContested as i32, 21);
    assert_eq!(op::EnrollmentRefusal::InvalidWindow as i32, 22);
    assert_eq!(op::EnrollmentRefusal::RecordUsed as i32, 23);
    let used = op::InventoryRecord {
        state: op::RecordState::Used.into(),
        ..op::InventoryRecord::default()
    };
    assert_eq!(used.encode_to_vec(), [0x60, 0x03]);
}
