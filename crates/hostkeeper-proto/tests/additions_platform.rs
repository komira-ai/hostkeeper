//! `InventoryRecord.platform` (field 15) and `AddRecordRequest.platform` (field 9), typed as
//! `hostkeeper.agent.v1.Platform`: a reader of the messages before the field reads every field
//! it knows from a message that carries it, a message written before the field reads as
//! `PLATFORM_UNSPECIFIED` (no platform), and the field numbers are pinned. The baseline holds
//! neither message, so each is spelled here as it was before the field.

use prost::Message as _;

use hostkeeper_proto::hostkeeper::agent::v1::Platform;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// `InventoryRecord` as it was before `platform`: fields 1 to 14.
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
    #[prost(uint64, tag = "11")]
    approve_until_unix: u64,
    #[prost(int32, tag = "12")]
    state: i32,
    #[prost(string, tag = "13")]
    used_by_request_id: String,
    #[prost(uint64, tag = "14")]
    used_unix: u64,
}

/// `AddRecordRequest` as it was before `platform`: fields 1 to 8.
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
    #[prost(uint64, tag = "7")]
    approve_until_unix: u64,
    #[prost(uint64, tag = "8")]
    approve_for_secs: u64,
}

fn record(platform: Platform) -> op::InventoryRecord {
    op::InventoryRecord {
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
        approve_until_unix: 1_800_086_400,
        state: op::RecordState::Open.into(),
        used_by_request_id: String::new(),
        used_unix: 0,
        platform: platform.into(),
        host_group: String::new(),
    }
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
        approve_until_unix: 1_800_086_400,
        state: op::RecordState::Open.into(),
        used_by_request_id: String::new(),
        used_unix: 0,
    }
}

/// Proves: a reader of the record before `platform` reads every field it knows from a record of
/// each Linux platform, and this release reads a record written before the field as
/// `PLATFORM_UNSPECIFIED`.
///
/// Catches: the field put at a number an older field holds, which an older reader would read
/// as that field.
#[test]
fn a_record_with_a_platform_reads_across_the_release_before() {
    for platform in [Platform::LinuxX8664, Platform::LinuxArm64] {
        let bytes = record(platform).encode_to_vec();
        let older = RecordBefore::decode(bytes.as_slice()).expect("an older reader decodes");
        assert_eq!(older, record_before(), "{platform:?}");
        let now = op::InventoryRecord::decode(bytes.as_slice()).expect("decodes");
        assert_eq!(now.platform(), platform);
    }
    let old = record_before().encode_to_vec();
    let read = op::InventoryRecord::decode(old.as_slice()).expect("decodes");
    assert_eq!(read, record(Platform::Unspecified));
}

/// Proves: an `AddRecordRequest` of a ctl of the release before (no field 7) reads as asking for
/// no platform, and a reader of the release before reads the rest of one that states a platform.
#[test]
fn an_add_record_request_reads_across_the_release_before() {
    let before = AddBefore {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: String::new(),
        serial: "SN-1".to_owned(),
        notes: "rack 4".to_owned(),
        approve_until_unix: 0,
        approve_for_secs: 3_600,
    };
    let read = op::AddRecordRequest::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.platform(), Platform::Unspecified);
    assert_eq!(read.host_id, "db-1");
    let now = op::AddRecordRequest {
        platform: Platform::LinuxArm64.into(),
        ..read
    };
    let older = AddBefore::decode(now.encode_to_vec().as_slice()).expect("an older reader decodes");
    assert_eq!(older, before);
}

/// Proves: `InventoryRecord.platform` is field 15 and `AddRecordRequest.platform` field 9, each
/// written as the varint of the platform's number (linux-arm64 is 2).
///
/// Catches: a renumbered field or platform value, which a reader of either side would misread.
#[test]
fn the_platform_fields_are_pinned() {
    let record = op::InventoryRecord {
        platform: Platform::LinuxArm64.into(),
        ..op::InventoryRecord::default()
    };
    assert_eq!(record.encode_to_vec(), [0x78, 0x02]);
    let request = op::AddRecordRequest {
        platform: Platform::LinuxX8664.into(),
        ..op::AddRecordRequest::default()
    };
    assert_eq!(request.encode_to_vec(), [0x48, 0x01]);
}
