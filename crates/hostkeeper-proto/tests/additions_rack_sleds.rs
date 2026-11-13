//! What sleds add to the wire, new since the release before (the network-gear API, whose
//! `Layout` this extends): `LayoutSled`, the `PutSled` and `RemoveSled` calls with their
//! messages, `GetLayoutResponse.sleds` (6), `LayoutPlacement.sled` (6) and `slot` (7),
//! `PlaceLayoutRequest.sled` (7) and `slot` (8), and `LayoutRefusal` value 15
//! (`operator/v1/layout.proto`).
//!
//! Nothing existing changed: a reader of the release before (here: a reader that knows only the
//! fields of that release, written as the prost structs below) reads the new bytes, keeps every
//! field it knows and ignores the rest, and this build reads the bytes that release wrote as a
//! layout with no sled and a placement in a rack. Those are the tests of rule B8 in both
//! directions.

use prost::Message as _;
use prost_reflect::DescriptorPool;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// `LayoutPlacement` as the release before knows it: fields 1, 2, 4 and 5.
#[derive(Clone, PartialEq, prost::Message)]
struct OlderPlacement {
    #[prost(message, optional, tag = "1")]
    subject: Option<op::LayoutSubject>,
    #[prost(string, tag = "2")]
    rack: String,
    #[prost(uint32, tag = "4")]
    height_u: u32,
    #[prost(uint32, tag = "5")]
    u: u32,
}

/// `PlaceLayoutRequest` as the release before knows it: fields 1, 2, 4, 5 and 6.
#[derive(Clone, PartialEq, prost::Message)]
struct OlderPlace {
    #[prost(message, optional, tag = "1")]
    subject: Option<op::LayoutSubject>,
    #[prost(string, tag = "2")]
    rack: String,
    #[prost(uint32, tag = "4")]
    height_u: u32,
    #[prost(uint32, tag = "5")]
    u: u32,
    #[prost(string, tag = "6")]
    reason: String,
}

/// `GetLayoutResponse` as the release before knows it: fields 1 to 5.
#[derive(Clone, PartialEq, prost::Message)]
struct OlderLayout {
    #[prost(message, repeated, tag = "1")]
    rows: Vec<op::LayoutRow>,
    #[prost(message, repeated, tag = "2")]
    racks: Vec<op::LayoutRack>,
    #[prost(message, repeated, tag = "3")]
    placements: Vec<OlderPlacement>,
    #[prost(string, tag = "4")]
    name: String,
    #[prost(message, repeated, tag = "5")]
    devices: Vec<op::NetworkDevice>,
}

fn subject(id: &str) -> Option<op::LayoutSubject> {
    Some(op::LayoutSubject {
        kind: op::LayoutSubjectKind::Inventory.into(),
        id: id.to_owned(),
    })
}

fn sled() -> op::LayoutSled {
    op::LayoutSled {
        id: "sled-1".to_owned(),
        rack: "rack-1".to_owned(),
        number: 1,
        u: 5,
        height_u: 3,
        label: "Studios".to_owned(),
        address: "C0-R0-R0-S0".to_owned(),
        slots: 2,
    }
}

/// Catches: a `GetLayoutResponse` of the release before that this build reads as holding sleds
/// or a placement on a sled, or one with sleds that a reader of the release before cannot read.
#[test]
fn a_layout_reply_of_the_release_before_reads_as_no_sleds_and_a_new_one_reads_in_the_old_reader() {
    let before = OlderLayout {
        rows: vec![op::LayoutRow {
            id: "row-1".to_owned(),
            number: 1,
        }],
        racks: vec![],
        placements: vec![OlderPlacement {
            subject: subject("a"),
            rack: "rack-1".to_owned(),
            height_u: 2,
            u: 3,
        }],
        name: "Lab".to_owned(),
        devices: vec![],
    };
    let now = op::GetLayoutResponse::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert!(now.sleds.is_empty());
    assert_eq!(
        (
            now.placements[0].sled.as_str(),
            now.placements[0].slot,
            now.placements[0].address.as_str()
        ),
        ("", 0, "")
    );
    assert_eq!(
        (now.placements[0].rack.as_str(), now.placements[0].u),
        ("rack-1", 3)
    );

    let with = op::GetLayoutResponse {
        rows: now.rows.clone(),
        name: "Lab".to_owned(),
        placements: vec![
            now.placements[0].clone(),
            op::LayoutPlacement {
                subject: subject("b"),
                sled: "sled-1".to_owned(),
                slot: 2,
                address: "C0-R0-R0-S0-S2".to_owned(),
                ..op::LayoutPlacement::default()
            },
        ],
        sleds: vec![sled()],
        ..op::GetLayoutResponse::default()
    };
    let older =
        OlderLayout::decode(with.encode_to_vec().as_slice()).expect("the old reader reads it");
    assert_eq!(older.name, "Lab");
    assert_eq!(older.rows, before.rows);
    assert_eq!(older.placements[0], before.placements[0]);
    // The subject on a sled reads as a placement with no rack: the old reader keeps its subject
    // and has nothing in a rack to draw.
    assert_eq!(older.placements[1].subject, subject("b"));
    assert_eq!(
        (older.placements[1].rack.as_str(), older.placements[1].u),
        ("", 0)
    );
    assert_eq!(
        op::GetLayoutResponse::decode(with.encode_to_vec().as_slice()).expect("decodes"),
        with
    );
}

/// Catches: a `PlaceLayoutRequest` of the release before that this build reads as asking for a
/// sled, or one for a sled that the release before reads as a placement it can act on.
#[test]
fn a_place_request_of_the_release_before_asks_for_no_sled_and_a_sled_request_has_no_rack_for_it() {
    let before = OlderPlace {
        subject: subject("a"),
        rack: "rack-1".to_owned(),
        height_u: 1,
        u: 7,
        reason: "racked".to_owned(),
    };
    let now = op::PlaceLayoutRequest::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!((now.sled.as_str(), now.slot), ("", None));
    assert_eq!((now.rack.as_str(), now.u, now.height_u), ("rack-1", 7, 1));

    let sledded = op::PlaceLayoutRequest {
        subject: subject("a"),
        sled: "sled-1".to_owned(),
        slot: Some(2),
        reason: "on the sled".to_owned(),
        ..op::PlaceLayoutRequest::default()
    };
    let older =
        OlderPlace::decode(sledded.encode_to_vec().as_slice()).expect("the old reader reads it");
    assert_eq!((older.rack.as_str(), older.u, older.height_u), ("", 0, 0));
    assert_eq!(older.reason, "on the sled");
}

/// Catches: the new messages being known to the release before, or reading back changed.
#[test]
fn the_sled_messages_round_trip() {
    let entry = op::AuditEntry {
        seq: 9,
        action: "layout_sled_put".to_owned(),
        ..op::AuditEntry::default()
    };
    let put = op::PutLayoutSledRequest {
        id: "sled-1".to_owned(),
        rack: "rack-1".to_owned(),
        u: 5,
        height_u: 3,
        label: "Studios".to_owned(),
        reason: "racked".to_owned(),
        slots: 2,
    };
    assert_eq!(
        put,
        op::PutLayoutSledRequest::decode(put.encode_to_vec().as_slice()).expect("decodes")
    );
    let reply = op::PutLayoutSledResponse {
        sled: Some(sled()),
        entry: Some(entry.clone()),
    };
    assert_eq!(
        reply,
        op::PutLayoutSledResponse::decode(reply.encode_to_vec().as_slice()).expect("decodes")
    );
    let remove = op::RemoveLayoutSledRequest {
        id: "sled-1".to_owned(),
        reason: "gone".to_owned(),
    };
    assert_eq!(
        remove,
        op::RemoveLayoutSledRequest::decode(remove.encode_to_vec().as_slice()).expect("decodes")
    );
    let removed = op::RemoveLayoutSledResponse { entry: Some(entry) };
    assert_eq!(
        removed,
        op::RemoveLayoutSledResponse::decode(removed.encode_to_vec().as_slice()).expect("decodes")
    );
}

/// Catches: a number or a token renamed, renumbered or reordered, which callers branch on.
#[test]
fn the_new_numbers_and_tokens_are_pinned() {
    assert_eq!(op::LayoutRefusal::UnknownSled as i32, 15);
    assert_eq!(
        op::LayoutRefusal::from_str_name("LAYOUT_REFUSAL_UNKNOWN_SLED"),
        Some(op::LayoutRefusal::UnknownSled)
    );
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("this build's descriptors");
    let numbered = |name: &str| -> Vec<(u32, String)> {
        pool.get_message_by_name(&format!("hostkeeper.operator.v1.{name}"))
            .expect("the message")
            .fields()
            .map(|f| (f.number(), f.name().to_owned()))
            .collect()
    };
    let owned = |fields: &[(u32, &str)]| -> Vec<(u32, String)> {
        fields
            .iter()
            .map(|(n, name)| (*n, (*name).to_owned()))
            .collect()
    };
    assert_eq!(
        numbered("LayoutSled"),
        owned(&[
            (1, "id"),
            (2, "rack"),
            (3, "number"),
            (4, "u"),
            (5, "height_u"),
            (6, "label"),
            (7, "address"),
            (8, "slots"),
        ])
    );
    assert_eq!(
        numbered("LayoutPlacement"),
        owned(&[
            (1, "subject"),
            (2, "rack"),
            (4, "height_u"),
            (5, "u"),
            (6, "sled"),
            (7, "slot"),
            (8, "address"),
        ])
    );
    assert_eq!(
        numbered("PlaceLayoutRequest"),
        owned(&[
            (1, "subject"),
            (2, "rack"),
            (4, "height_u"),
            (5, "u"),
            (6, "reason"),
            (7, "sled"),
            (8, "slot"),
        ])
    );
    assert_eq!(
        pool.get_message_by_name("hostkeeper.operator.v1.GetLayoutResponse")
            .and_then(|m| m.get_field(6).map(|f| f.name().to_owned()))
            .as_deref(),
        Some("sleds")
    );
    assert_eq!(sled().encode_to_vec()[..2], [0x0a, 6]);
}
