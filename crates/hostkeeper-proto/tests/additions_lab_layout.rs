//! What the lab layout adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Layout` service of the operator API
//! with its messages, `LayoutRefusal` and `LayoutSubjectKind` (`operator/v1/layout.proto`), and,
//! on `ApproveRequest`, the optional placement (`rack` 9, `placement_u` 10, `placement_height_u`
//! 11) with four new `EnrollmentRefusal` values (27 to 30).
//!
//! Nothing existing changed: the release before knows none of the new messages, a reader of it that
//! meets one keeps every byte, it drops nothing of an `ApproveRequest` that carries a placement (the
//! fields are unknown to it and kept), and this build reads an `ApproveRequest` that release wrote
//! (no placement) as one that asks for none. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

fn placement() -> op::LayoutPlacement {
    op::LayoutPlacement {
        subject: Some(op::LayoutSubject {
            kind: op::LayoutSubjectKind::Inventory.into(),
            id: "db-1".to_owned(),
        }),
        rack: "rack-1".to_owned(),
        u: 40,
        height_u: 2,
        ..op::LayoutPlacement::default()
    }
}

fn entry() -> op::AuditEntry {
    op::AuditEntry {
        seq: 3,
        action: "layout_place".to_owned(),
        ..op::AuditEntry::default()
    }
}

/// One message of each type the layout added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    let row = op::LayoutRow {
        id: "row-2".to_owned(),
        number: 2,
    };
    let rack = op::LayoutRack {
        id: "rack-3".to_owned(),
        row: "row-2".to_owned(),
        height_u: 42,
        number: 3,
    };
    vec![
        ("hostkeeper.operator.v1.LayoutRow", round_trip(&row)),
        ("hostkeeper.operator.v1.LayoutRack", round_trip(&rack)),
        (
            "hostkeeper.operator.v1.LayoutPlacement",
            round_trip(&placement()),
        ),
        (
            "hostkeeper.operator.v1.GetLayoutResponse",
            round_trip(&op::GetLayoutResponse {
                rows: vec![row.clone()],
                racks: vec![rack.clone()],
                placements: vec![placement()],
                name: "Lab".to_owned(),
                devices: Vec::new(),
                sleds: Vec::new(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutLayoutNameRequest",
            round_trip(&op::PutLayoutNameRequest {
                name: "Lab".to_owned(),
                reason: "rename".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.AddLayoutRowResponse",
            round_trip(&op::AddLayoutRowResponse {
                row: Some(row),
                entry: Some(entry()),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutLayoutRackRequest",
            round_trip(&op::PutLayoutRackRequest {
                id: "rack-3".to_owned(),
                row: "row-2".to_owned(),
                height_u: 42,
                reason: "resize".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PlaceLayoutRequest",
            round_trip(&op::PlaceLayoutRequest {
                subject: placement().subject,
                rack: "rack-1".to_owned(),
                height_u: 2,
                u: 40,
                reason: "lab move".to_owned(),
                ..op::PlaceLayoutRequest::default()
            }),
        ),
        (
            "hostkeeper.operator.v1.PlaceLayoutResponse",
            round_trip(&op::PlaceLayoutResponse {
                placement: Some(placement()),
                entry: Some(entry()),
            }),
        ),
        (
            "hostkeeper.operator.v1.UnplaceLayoutRequest",
            round_trip(&op::UnplaceLayoutRequest {
                subject: placement().subject,
                reason: "retired".to_owned(),
            }),
        ),
    ]
}

/// Catches: a message, enum or service of the layout that the release before already knows, or that
/// does not read back as written, or that a reader of that release drops: it knows none of them,
/// and a message it reads with no field keeps every byte of the new ones.
#[test]
fn the_layout_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 10);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    for name in ["LayoutRefusal", "LayoutSubjectKind"] {
        assert!(
            older
                .get_enum_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name} is new"
        );
    }
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Layout")
            .is_none()
    );
}

/// Catches: an `ApproveRequest` that the release before cannot carry through unchanged, or whose
/// absent placement reads as a placement: the release before reads one with a placement and keeps
/// every byte, this build reads the bytes of one without as a request that asks for none, and the
/// new fields keep their numbers (9, 10, 11).
#[test]
fn an_approval_carries_its_placement_to_an_older_reader_and_reads_none_from_it() {
    let older = baseline_pool();
    // The release before may not know the message at all, or know it without the placement: it reads
    // the bytes either way as a message it has no field for, and keeps them.
    let approve = older
        .get_message_by_name("hostkeeper.operator.v1.ApproveRequest")
        .unwrap_or_else(|| {
            older
                .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
                .expect("a message of the release before")
        });
    for number in [9, 10, 11] {
        assert!(approve.get_field(number).is_none(), "field {number} is new");
    }
    let placed = op::ApproveRequest {
        request_id: "ab".repeat(32),
        host_id: "web-1".to_owned(),
        role: "frontend".to_owned(),
        reason: "new".to_owned(),
        rack: "rack-1".to_owned(),
        placement_u: 40,
        placement_height_u: 2,
        ..op::ApproveRequest::default()
    };
    let bytes = round_trip(&placed);
    let read = DynamicMessage::decode(approve.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(read.encode_to_vec(), bytes);

    let before = op::ApproveRequest {
        rack: String::new(),
        placement_u: 0,
        placement_height_u: 0,
        ..placed
    };
    let written = DynamicMessage::decode(approve, before.encode_to_vec().as_slice())
        .expect("decodes")
        .encode_to_vec();
    let now = op::ApproveRequest::decode(written.as_slice()).expect("decodes");
    assert_eq!(
        (now.rack.as_str(), now.placement_u, now.placement_height_u),
        ("", 0, 0)
    );
    assert_eq!(now, before);
}

/// Catches: a token or number of `LayoutRefusal` or of the four new `EnrollmentRefusal` values
/// renamed, renumbered or reordered, which callers branch on, and a value that the release before
/// already had.
#[test]
fn the_refusal_tokens_and_numbers_are_pinned() {
    let layout = [
        ("UNAUTHORIZED", 1),
        ("FORBIDDEN", 2),
        ("INVALID_REQUEST", 3),
        ("STORE_FAILED", 4),
        ("AUDIT_UNAVAILABLE", 5),
        ("UNKNOWN_ROW", 6),
        ("UNKNOWN_RACK", 7),
        ("UNKNOWN_PLACEMENT", 8),
        ("NAME_TAKEN", 9),
        ("IN_USE", 10),
        ("OVERLAP", 11),
        ("OUT_OF_RACK", 12),
        ("CAPACITY", 13),
    ];
    for (token, number) in layout {
        let value = op::LayoutRefusal::from_str_name(&format!("LAYOUT_REFUSAL_{token}"))
            .unwrap_or_else(|| panic!("{token}"));
        assert_eq!(value as i32, number, "{token}");
    }
    let enrollment = [
        (
            "PLACEMENT_INVALID",
            27,
            op::EnrollmentRefusal::PlacementInvalid,
        ),
        ("UNKNOWN_RACK", 28, op::EnrollmentRefusal::UnknownRack),
        (
            "PLACEMENT_OVERLAP",
            29,
            op::EnrollmentRefusal::PlacementOverlap,
        ),
        (
            "PLACEMENT_OUT_OF_RACK",
            30,
            op::EnrollmentRefusal::PlacementOutOfRack,
        ),
    ];
    let older = baseline_pool();
    let refusal = older.get_enum_by_name("hostkeeper.operator.v1.EnrollmentRefusal");
    for (token, number, value) in enrollment {
        assert_eq!(value as i32, number, "{token}");
        if let Some(refusal) = &refusal {
            assert!(refusal.get_value(number).is_none(), "{token} is new");
            assert!(
                refusal
                    .get_value_by_name(&format!("ENROLLMENT_REFUSAL_{token}"))
                    .is_none()
            );
        }
    }
}
