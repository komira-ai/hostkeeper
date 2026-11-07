//! `InventoryRecord.host_held` (field 10) and its enum `HostHeld`, added after `enrolled`: a
//! record written before the field reads as `UNSPECIFIED`, which is not `NONE`; a reader of the
//! record before the field reads every field it knows from a record that carries it; and the
//! numbers of the enum are pinned. The baseline (`proto/compat/baseline.binpb`) holds no
//! `InventoryRecord`, so the record before the field is spelled here as a message of its own.

use prost::Message as _;
use prost_reflect::DescriptorPool;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `InventoryRecord` as it was before `host_held`: fields 1 to 9.
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
}

fn before() -> RecordBefore {
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
    }
}

/// `record` as the generated type spells it, with `held`.
fn now_of(record: &RecordBefore, held: op::HostHeld) -> op::InventoryRecord {
    op::InventoryRecord {
        host_id: record.host_id.clone(),
        role: record.role.clone(),
        data_macs: record.data_macs.clone(),
        bmc_mac: record.bmc_mac.clone(),
        serial: record.serial.clone(),
        notes: record.notes.clone(),
        added_by: record.added_by.clone(),
        added_unix: record.added_unix,
        enrolled: record.enrolled,
        host_held: held.into(),
        ..op::InventoryRecord::default()
    }
}

/// Proves: a record a coordinator of the release before wrote (no field 10) reads with every
/// field it carried and `host_held` `UNSPECIFIED`, which is not `NONE`.
///
/// Catches: `NONE` made the zero value, so that a coordinator that never read its store would
/// say no host holds every record's id.
#[test]
fn a_record_written_before_host_held_reads_unspecified_not_none() {
    let read = op::InventoryRecord::decode(before().encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read, now_of(&before(), op::HostHeld::Unspecified));
    assert_eq!(read.host_held(), op::HostHeld::Unspecified);
    assert_ne!(read.host_held(), op::HostHeld::None);
}

/// Proves: a reader of the record before `host_held` reads, from a record of each state, every
/// field it knows as it was written; no released descriptor set holds the record.
///
/// Catches: field 10 written in a way an older reader cannot skip, or a number shared with a
/// field it knows.
#[test]
fn a_reader_before_host_held_reads_every_field_it_knows() {
    assert!(
        baseline_pool()
            .get_message_by_name("hostkeeper.operator.v1.InventoryRecord")
            .is_none(),
        "the release before has no InventoryRecord"
    );
    for held in [
        op::HostHeld::None,
        op::HostHeld::Approved,
        op::HostHeld::Denied,
        op::HostHeld::Retired,
    ] {
        let record = now_of(&before(), held);
        let bytes = record.encode_to_vec();
        assert_eq!(
            op::InventoryRecord::decode(bytes.as_slice()).expect("decodes"),
            record
        );
        let older = RecordBefore::decode(bytes.as_slice()).expect("an older reader decodes");
        assert_eq!(older, before(), "{held:?}");
    }
}

/// Proves: the numbers of `HostHeld` are 0 to 4 in the order the proto gives them, and a
/// retired record writes field 10 as the varint 4 after the fields before it.
///
/// Catches: a value renumbered, which would turn a retired host into another state for a reader
/// of either side.
#[test]
fn host_held_numbers_are_pinned() {
    let numbers = [
        (op::HostHeld::Unspecified, 0),
        (op::HostHeld::None, 1),
        (op::HostHeld::Approved, 2),
        (op::HostHeld::Denied, 3),
        (op::HostHeld::Retired, 4),
    ];
    for (held, number) in numbers {
        assert_eq!(held as i32, number, "{held:?}");
    }
    let bytes = now_of(&RecordBefore::default(), op::HostHeld::Retired).encode_to_vec();
    assert_eq!(bytes, [0x50, 0x04]);
}
