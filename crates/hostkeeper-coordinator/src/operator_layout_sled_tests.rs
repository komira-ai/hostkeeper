//! The sled calls of `Layout` of the operator API ([`crate::operator_layout`]): `PutSled`,
//! `RemoveSled` and `Place` onto a sled, each answer and audit entry and each refusal with nothing
//! changed and no audit line. The helpers are those of `operator_layout_tests`. No clock and no timing.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::LayoutRefusal as Refusal;

use crate::host_groups_support::World;
use crate::marks_support::operator;
use crate::operator_layout_tests::{
    audit_actions, get, host, place, rack_of, remove_rack, unplace, world,
};

fn sled_at(rack: &str, u: u32, height_u: u32) -> proto::PutLayoutSledRequest {
    proto::PutLayoutSledRequest {
        rack: rack.to_owned(),
        u,
        height_u,
        ..proto::PutLayoutSledRequest::default()
    }
}

fn on_sled(
    subject: Option<proto::LayoutSubject>,
    sled: &str,
    slot: Option<u32>,
) -> proto::PlaceLayoutRequest {
    proto::PlaceLayoutRequest {
        subject,
        sled: sled.to_owned(),
        slot,
        ..proto::PlaceLayoutRequest::default()
    }
}

/// A coordinator with `rack-1` (24 U) and a 3 U sled at U 5.
fn sled_world(name: &str) -> World {
    let world = world(name);
    let c = world.coordinator();
    let op = operator();
    let row = c
        .layout_add_row(&op, &proto::AddLayoutRowRequest::default())
        .expect("a row")
        .row
        .expect("the row")
        .id;
    c.layout_put_rack(&op, &rack_of(&row, 24)).expect("a rack");
    let sled = c
        .layout_put_sled(
            &op,
            &proto::PutLayoutSledRequest {
                label: "Studios".into(),
                slots: 8,
                ..sled_at("rack-1", 5, 3)
            },
        )
        .expect("a sled");
    assert_eq!(sled.sled.expect("the sled").id, "sled-1");
    world
}

/// Proves: `PutSled` adds a sled (id, number, address, label back), `Place` with a sled puts a
/// server on its lowest free slot (or on the slot named) and `Get` lists it with `sled`, `slot`,
/// the computed address and the sled's rack, U and height, a `Place` by rack and U makes a sled
/// that `Get` lists too and that goes when its server is unplaced, a second `Place` moves the
/// server, moving the sled with `PutSled` keeps the server on it, `Unplace` takes it off,
/// `RemoveSled` removes the empty sled, and every write is audited under its own action with the
/// id of the sled or the subject's key.
///
/// Catches: a server whose sled is not reported, a sled move that strands its servers, an
/// implicit sled that stays after its server leaves, or a write with no audit line.
#[test]
fn a_sled_is_added_carries_servers_moves_with_them_and_is_removed_when_empty() {
    let world = sled_world("layout-sleds");
    let c = world.coordinator();
    let op = operator();
    let got = get(c);
    assert_eq!(
        got.sleds,
        [proto::LayoutSled {
            id: "sled-1".into(),
            rack: "rack-1".into(),
            number: 0,
            u: 5,
            height_u: 3,
            label: "Studios".into(),
            address: "C0-R0-R0-S0".into(),
            slots: 8,
        }]
    );
    let placed = c
        .layout_place(&op, &on_sled(host("node-1"), "sled-1", Some(1)))
        .expect("on the sled");
    assert_eq!(
        placed.placement,
        Some(proto::LayoutPlacement {
            subject: host("node-1"),
            rack: "rack-1".into(),
            u: 5,
            height_u: 3,
            sled: "sled-1".into(),
            slot: 1,
            address: "C0-R0-R0-S0-S1".into(),
        })
    );
    // A placement by rack and U makes a sled; both are listed by U.
    c.layout_place(&op, &place(host("node-2"), "rack-1", 12, 1))
        .expect("in the rack");
    let got = get(c);
    assert_eq!(
        got.placements
            .iter()
            .map(|p| (
                p.subject.clone().map(|s| s.id).unwrap_or_default(),
                p.rack.as_str(),
                p.sled.as_str(),
                p.slot,
                p.u,
                p.height_u,
                p.address.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (
                "node-1".to_owned(),
                "rack-1",
                "sled-1",
                1,
                5,
                3,
                "C0-R0-R0-S0-S1"
            ),
            (
                "node-2".to_owned(),
                "rack-1",
                "sled-2",
                0,
                12,
                1,
                "C0-R0-R0-S1-S0"
            )
        ]
    );
    assert_eq!(
        got.sleds
            .iter()
            .map(|s| (s.id.as_str(), s.number, s.address.as_str()))
            .collect::<Vec<_>>(),
        [("sled-1", 0, "C0-R0-R0-S0"), ("sled-2", 1, "C0-R0-R0-S1")]
    );
    // A move of the sled (another U, a taller sled) keeps the server on it.
    let moved = c
        .layout_put_sled(
            &op,
            &proto::PutLayoutSledRequest {
                id: "sled-1".into(),
                reason: "re-racked".into(),
                ..sled_at("rack-1", 15, 4)
            },
        )
        .expect("moved");
    assert_eq!(moved.sled.map(|s| (s.u, s.height_u)), Some((15, 4)));
    assert!(
        get(c)
            .placements
            .iter()
            .any(|p| p.sled == "sled-1" && p.slot == 1)
    );
    // A server moves from a sled to a rack and back.
    c.layout_place(&op, &place(host("node-1"), "rack-1", 1, 1))
        .expect("to the rack");
    c.layout_place(&op, &on_sled(host("node-1"), "sled-1", None))
        .expect("back on the sled");
    c.layout_unplace(&op, &unplace("node-1"))
        .expect("off the sled");
    c.layout_remove_sled(
        &op,
        &proto::RemoveLayoutSledRequest {
            id: "sled-1".into(),
            reason: String::new(),
        },
    )
    .expect("removed");
    assert_eq!(
        get(c)
            .sleds
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["sled-2"],
        "the sled made for node-2 stays while node-2 stands on it"
    );
    c.layout_unplace(&op, &unplace("node-2")).expect("off");
    assert!(
        get(c).sleds.is_empty(),
        "an implicit sled goes with its server"
    );
    let actions: Vec<(String, Option<String>)> = audit_actions(&world.audit());
    assert_eq!(
        actions
            .iter()
            .filter(|(a, _)| a.starts_with("layout_sled"))
            .cloned()
            .collect::<Vec<_>>(),
        [
            ("layout_sled_put".to_owned(), Some("sled".to_owned())),
            ("layout_sled_put".to_owned(), Some("sled-1".to_owned())),
            ("layout_sled_remove".to_owned(), Some("sled-1".to_owned())),
        ]
    );
    assert!(actions.contains(&("layout_place".to_owned(), Some("host:node-1".to_owned()))));
}

/// Proves: each refusal of a sled is its token and changes nothing and writes no audit line: a
/// sled over another sled or past the rack's top is `overlap` or `out_of_rack`, a bad height, U,
/// label or reason `invalid_request`, an unknown rack or sled its own token, a slot taken
/// `overlap`, a slot past 7 or a slot with no sled, or a rack together with a sled,
/// `invalid_request`, a sled with every slot taken `capacity`, and a sled that carries a server
/// cannot be removed (`in_use`), nor can its rack.
///
/// Catches: a refused sled call that is half kept, or a sled removed with servers on it.
#[test]
fn a_refused_sled_call_changes_nothing_and_writes_no_audit_line() {
    let world = sled_world("layout-sleds-refused");
    let c = world.coordinator();
    let op = operator();
    c.layout_place(&op, &place(host("srv"), "rack-1", 2, 1))
        .expect("a server");
    c.layout_place(&op, &on_sled(host("a"), "sled-1", Some(0)))
        .expect("on the sled");
    let lines = world.audit().len();
    let before = get(c);
    let put = |request: proto::PutLayoutSledRequest| c.layout_put_sled(&op, &request).err();
    let places = |request: proto::PlaceLayoutRequest| c.layout_place(&op, &request).err();
    assert_eq!(put(sled_at("rack-1", 2, 2)), Some(Refusal::Overlap));
    assert_eq!(put(sled_at("rack-1", 7, 3)), Some(Refusal::Overlap));
    assert_eq!(put(sled_at("rack-1", 23, 3)), Some(Refusal::OutOfRack));
    assert_eq!(put(sled_at("rack-1", 10, 0)), Some(Refusal::InvalidRequest));
    assert_eq!(
        put(sled_at("rack-1", 10, 61)),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(put(sled_at("rack-1", 0, 2)), Some(Refusal::InvalidRequest));
    assert_eq!(
        put(proto::PutLayoutSledRequest {
            label: "a/b".into(),
            ..sled_at("rack-1", 10, 2)
        }),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        put(proto::PutLayoutSledRequest {
            reason: "a\u{7}".into(),
            ..sled_at("rack-1", 10, 2)
        }),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(put(sled_at("rack-9", 10, 2)), Some(Refusal::UnknownRack));
    assert_eq!(
        put(proto::PutLayoutSledRequest {
            id: "sled-9".into(),
            ..sled_at("rack-1", 10, 2)
        }),
        Some(Refusal::UnknownSled)
    );
    c.layout_place(&op, &on_sled(host("b"), "sled-1", None))
        .expect("the next slot");
    let after_b = get(c);
    assert_eq!(
        after_b
            .placements
            .iter()
            .filter(|p| p.sled == "sled-1")
            .map(|p| p.slot)
            .collect::<Vec<_>>(),
        [0, 1],
        "no slot named: the lowest free one"
    );
    assert_eq!(
        places(on_sled(host("c"), "sled-1", Some(1))),
        Some(Refusal::Overlap)
    );
    assert_eq!(
        places(on_sled(host("c"), "sled-1", Some(8))),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        places(on_sled(host("c"), "", Some(0))),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        places(proto::PlaceLayoutRequest {
            rack: "rack-1".into(),
            ..on_sled(host("c"), "sled-1", None)
        }),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        places(proto::PlaceLayoutRequest {
            u: 3,
            ..on_sled(host("c"), "sled-1", None)
        }),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        places(proto::PlaceLayoutRequest {
            height_u: 1,
            ..on_sled(host("c"), "sled-1", None)
        }),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        places(on_sled(host("c"), "sled-7", None)),
        Some(Refusal::UnknownSled)
    );
    let remove = |id: &str| {
        c.layout_remove_sled(
            &op,
            &proto::RemoveLayoutSledRequest {
                id: id.into(),
                reason: String::new(),
            },
        )
        .err()
    };
    assert_eq!(remove("sled-1"), Some(Refusal::InUse));
    assert_eq!(remove("sled-9"), Some(Refusal::UnknownSled));
    assert_eq!(
        c.layout_remove_rack(&op, &remove_rack("rack-1")).err(),
        Some(Refusal::InUse)
    );
    assert_eq!(get(c), after_b);
    assert_eq!(world.audit().len(), lines + 1, "only the slot made above");
    assert_ne!(before, after_b);
    for n in 0..6 {
        c.layout_place(&op, &on_sled(host(&format!("fill{n}")), "sled-1", None))
            .expect("a free slot");
    }
    assert_eq!(
        places(on_sled(host("last"), "sled-1", None)),
        Some(Refusal::Capacity)
    );
}
