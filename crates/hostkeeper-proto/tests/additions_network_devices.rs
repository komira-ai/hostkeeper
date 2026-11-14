//! What network devices add to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `NetworkDevice` and `NetworkRole`, the
//! `PutDevice` and `RemoveDevice` calls of `Layout` with their messages, the optional
//! `GetLayoutResponse.devices` (5) and `LayoutRefusal` value 14 (`operator/v1/layout.proto`).
//!
//! Nothing existing changed: a reader of the release before that meets the new messages, or a
//! `GetLayoutResponse` that carries devices, keeps every byte, and this build reads the bytes of
//! a `GetLayoutResponse` written without devices as one that holds none. Those are the tests of
//! rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

fn device() -> op::NetworkDevice {
    op::NetworkDevice {
        id: "net.1".to_owned(),
        name: "bmc-switch".to_owned(),
        role: op::NetworkRole::Bmc.into(),
        console_host: "node-3".to_owned(),
        console_device: "/dev/ttyUSB0".to_owned(),
        management_mac: "aa:bb:cc:dd:ee:ff".to_owned(),
        notes: "uplink".to_owned(),
    }
}

/// Catches: a new message that reads back changed, that the release before already knows, or that
/// an older reader drops instead of keeping its bytes.
#[test]
fn the_device_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let put = op::PutLayoutDeviceRequest {
        id: "net.1".to_owned(),
        name: "bmc-switch".to_owned(),
        role: op::NetworkRole::Bmc.into(),
        console_host: "node-3".to_owned(),
        console_device: "/dev/ttyUSB0".to_owned(),
        management_mac: "aa:bb:cc:dd:ee:ff".to_owned(),
        notes: "uplink".to_owned(),
        reason: "cabled".to_owned(),
    };
    let entry = op::AuditEntry {
        seq: 4,
        action: "layout_device_put".to_owned(),
        ..op::AuditEntry::default()
    };
    let messages: Vec<(&str, Vec<u8>)> = vec![
        ("NetworkDevice", device().encode_to_vec()),
        ("PutLayoutDeviceRequest", put.encode_to_vec()),
        (
            "PutLayoutDeviceResponse",
            op::PutLayoutDeviceResponse {
                device: Some(device()),
                entry: Some(entry.clone()),
            }
            .encode_to_vec(),
        ),
        (
            "RemoveLayoutDeviceRequest",
            op::RemoveLayoutDeviceRequest {
                id: "net.1".to_owned(),
                reason: "gone".to_owned(),
            }
            .encode_to_vec(),
        ),
        (
            "RemoveLayoutDeviceResponse",
            op::RemoveLayoutDeviceResponse { entry: Some(entry) }.encode_to_vec(),
        ),
    ];
    for (name, bytes) in messages {
        let full = format!("hostkeeper.operator.v1.{name}");
        assert!(older.get_message_by_name(&full).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.NetworkRole")
            .is_none()
    );
    assert_eq!(
        put,
        op::PutLayoutDeviceRequest::decode(put.encode_to_vec().as_slice()).expect("decodes")
    );
}

/// Catches: a `GetLayoutResponse` of the layout before devices that this build reads as holding
/// devices, or one with devices that a reader knowing only fields 1 to 4 drops or changes.
#[test]
fn a_layout_reply_without_devices_reads_as_none_and_with_devices_keeps_its_bytes() {
    let before = op::GetLayoutResponse {
        rows: vec![op::LayoutRow {
            id: "row-1".to_owned(),
            number: 1,
        }],
        name: "Lab".to_owned(),
        ..op::GetLayoutResponse::default()
    };
    let now = op::GetLayoutResponse::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert!(now.devices.is_empty());
    assert_eq!(now, before);

    let with = op::GetLayoutResponse {
        devices: vec![device()],
        ..before.clone()
    };
    let bytes = with.encode_to_vec();
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("this build's descriptors");
    let message = pool
        .get_message_by_name("hostkeeper.operator.v1.GetLayoutResponse")
        .expect("the message");
    assert_eq!(
        message.get_field(5).map(|f| f.name().to_owned()).as_deref(),
        Some("devices")
    );
    // The reader of fields 1 to 4: the same bytes with the devices field cut out of the message.
    let older = op::GetLayoutResponse::decode(&bytes[..bytes.len() - device().encoded_len() - 2])
        .expect("decodes");
    assert_eq!(older, before);
}

/// Catches: a number or a token renamed, renumbered or reordered, which callers branch on.
#[test]
fn the_new_numbers_and_tokens_are_pinned() {
    assert_eq!(op::LayoutRefusal::UnknownDevice as i32, 14);
    assert_eq!(
        op::LayoutRefusal::from_str_name("LAYOUT_REFUSAL_UNKNOWN_DEVICE"),
        Some(op::LayoutRefusal::UnknownDevice)
    );
    assert_eq!(op::NetworkRole::Bmc as i32, 1);
    assert_eq!(op::NetworkRole::Data as i32, 2);
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("this build's descriptors");
    let message = pool
        .get_message_by_name("hostkeeper.operator.v1.NetworkDevice")
        .expect("the message");
    let fields: Vec<(u32, String)> = message
        .fields()
        .map(|f| (f.number(), f.name().to_owned()))
        .collect();
    assert_eq!(
        fields,
        [
            (1, "id"),
            (2, "name"),
            (3, "role"),
            (4, "console_host"),
            (5, "console_device"),
            (6, "management_mac"),
            (7, "notes"),
        ]
        .map(|(n, name)| (n, name.to_owned()))
    );
    assert_eq!(device().encode_to_vec()[..2], [0x0a, 5]);
}
