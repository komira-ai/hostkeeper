//! The sleds of the layout ([`super`]): each field's rule, the U they hold against servers,
//! switches and other sleds, the slots a placement takes, the address of a place, what a move or
//! a removal does to what stands on a sled, and what a file or a record holds. No clock.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Extras, MAX_SLED_HEIGHT_U, MAX_SLEDS, MAX_SLOTS, Sled};
use crate::layout::{
    Device, Kind, Layout, LayoutError, MAX_PLACEMENTS, Placement, Rack, Records, Row, Segment,
    Subject, encode_file, parse_file,
};
use crate::operator_layout::placement_of;

fn subject(id: &str) -> Subject {
    Subject {
        kind: Kind::Inventory,
        id: id.to_owned(),
    }
}

/// Two racks (24 U and 10 U) in one row: `rack-1` and `rack-2`.
fn racks() -> Layout {
    let (next, row) = Layout::default().with_next_row().expect("a row");
    let (next, _) = next.with_rack("", &row.id, 24).expect("a rack");
    next.with_rack("", &row.id, 10).expect("a second rack").0
}

/// `racks()` with a 3 U sled at U 5 to 7 of `rack-1`: `sled-1`.
fn sledded() -> Layout {
    racks()
        .with_sled("", "rack-1", 5, 3, 8, "Studios")
        .expect("a sled")
        .0
}

fn on(layout: &Layout, id: &str, sled: &str, slot: u32) -> Layout {
    layout
        .with_sled_placement(&subject(id), sled, Some(slot))
        .expect("placed on the sled")
        .0
}

fn refused(result: Result<(Layout, impl Sized), LayoutError>) -> LayoutError {
    match result {
        Ok(_) => panic!("the change was accepted"),
        Err(why) => why,
    }
}

/// Proves: a sled is added under the next `sled-<n>` id, keeps its fields, and moving it to
/// another rack or another U keeps its id and replaces the rest.
///
/// Catches: a second sled that takes the first's id, or a move that leaves the sled where it was.
#[test]
fn a_sled_takes_the_next_id_and_a_move_replaces_its_place() {
    let (one, first) = racks()
        .with_sled("", "rack-1", 5, 3, 8, "Studios")
        .expect("added");
    assert_eq!(
        first,
        Sled {
            id: "sled-1".into(),
            rack: "rack-1".into(),
            u: 5,
            height_u: 3,
            label: "Studios".into(),
            implicit: false,
            slots: 8,
        }
    );
    let (two, second) = one.with_sled("", "rack-1", 9, 2, 8, "").expect("added");
    assert_eq!(second.id, "sled-2");
    let (moved, kept) = two
        .with_sled("sled-1", "rack-2", 4, 3, 8, "Studios")
        .expect("moved");
    assert_eq!(
        (kept.id.as_str(), kept.rack.as_str(), kept.u),
        ("sled-1", "rack-2", 4)
    );
    assert_eq!(
        moved
            .sleds()
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["sled-2", "sled-1"]
    );
    assert!(matches!(
        refused(moved.with_sled("sled-9", "rack-1", 1, 1, 8, "")),
        LayoutError::UnknownSled(id) if id == "sled-9"
    ));
}

/// Proves: a sled is refused with a height of 0 or past 60, a U of 0, a label that breaks the name
/// rule, an unknown rack, and a range that is not within its rack.
///
/// Catches: a sled of no height, or one that sticks out of the top of the rack.
#[test]
fn a_sled_keeps_to_its_height_label_and_rack() {
    let base = racks();
    for (u, height, label) in [
        (1, 0, ""),
        (1, MAX_SLED_HEIGHT_U + 1, ""),
        (0, 3, ""),
        (1, 3, "bad/label"),
        (1, 3, " padded "),
    ] {
        assert!(
            matches!(
                refused(base.with_sled("", "rack-1", u, height, 8, label)),
                LayoutError::Invalid(_)
            ),
            "{u} {height} {label:?}"
        );
    }
    assert!(matches!(
        refused(base.with_sled("", "rack-9", 1, 3, 8, "")),
        LayoutError::UnknownRack(_)
    ));
    // rack-2 is 10 U: U 8 to 10 fits, U 9 to 11 does not.
    base.with_sled("", "rack-2", 8, 3, 8, "")
        .expect("up to the top");
    assert!(matches!(
        refused(base.with_sled("", "rack-2", 9, 3, 8, "")),
        LayoutError::OutOfRack { height: 10, .. }
    ));
    base.with_sled("", "rack-1", 1, 24, 8, "x")
        .expect("the biggest sled of the rack");
}

/// Proves: sleds are the only things that hold U: a sled overlapping another is refused as
/// `Overlap` naming both, touching ranges are fine, a placement by rack and U over a sled is
/// refused (it would make a second sled there), and the same U of another rack is free.
///
/// Catches: two sleds drawn over one another.
#[test]
fn a_sled_shares_no_u_with_another_sled() {
    let layout = sledded();
    // The sled holds U 5 to 7.
    for (u, height) in [(4, 2), (7, 1), (5, 3), (6, 1), (1, 5), (7, 4)] {
        let placed = layout.with_placement(placement_of(subject("srv"), "rack-1", u, height));
        assert!(
            matches!(&placed, Err(LayoutError::Overlap { first, second, .. })
                if [first.as_str(), second.as_str()].contains(&"sled-1")),
            "U {u} height {height}: {placed:?}"
        );
    }
    for (u, height) in [(4, 1), (8, 2), (1, 4)] {
        layout
            .with_placement(placement_of(subject("srv"), "rack-1", u, height))
            .expect("beside the sled");
    }
    assert!(matches!(
        refused(layout.with_sled("", "rack-1", 7, 2, 8, "")),
        LayoutError::Overlap { .. }
    ));
    layout
        .with_sled("", "rack-1", 8, 2, 8, "")
        .expect("a sled right above");
    layout
        .with_sled("", "rack-2", 5, 3, 8, "")
        .expect("another rack");
}
/// Proves: a subject goes on slot 0 to 7 of a sled that declares 8 (8 is refused), two subjects never share a slot
/// Proves: a subject goes on slot 0 to 7 of a sled (8 is refused), two subjects never share a slot
/// (`SlotTaken` naming both), a placement that names no slot takes the lowest free one (a freed
/// slot is taken again; a full sled is `Capacity`), putting a subject again moves it, and a sled
/// the layout lacks is `UnknownSled`.
///
/// Catches: a slot past the end, or two servers drawn in one bay.
#[test]
fn a_subject_takes_a_slot_and_a_slot_holds_one_subject() {
    let layout = sledded();
    assert!(matches!(
        refused(layout.with_sled_placement(&subject("a"), "sled-1", Some(MAX_SLOTS))),
        LayoutError::Invalid(why) if why.contains("a slot of sled-1 is 0 to 7")
    ));
    assert!(matches!(
        refused(layout.with_sled_placement(&subject("a"), "sled-7", Some(0))),
        LayoutError::UnknownSled(id) if id == "sled-7"
    ));
    assert!(matches!(
        refused(layout.with_sled_placement(&subject("a"), "sled-7", None)),
        LayoutError::UnknownSled(_)
    ));
    let one = on(&layout, "a", "sled-1", 0);
    match refused(one.with_sled_placement(&subject("b"), "sled-1", Some(0))) {
        LayoutError::SlotTaken {
            sled,
            slot,
            first,
            second,
        } => {
            assert_eq!((sled.as_str(), slot), ("sled-1", 0));
            assert_eq!(
                (first.as_str(), second.as_str()),
                ("inventory:a", "inventory:b")
            );
        }
        other => panic!("not a taken slot: {other}"),
    }
    // The same subject on another slot is a move, not a collision.
    let moved = on(&one, "a", "sled-1", 3);
    assert_eq!(moved.sled_item(&subject("a")).map(|i| i.slot), Some(3));
    // No slot named: the lowest free one, 1 here, then 0 again once a is on 3 and b took 0.
    let (two, (item, _)) = one
        .with_sled_placement(&subject("b"), "sled-1", None)
        .expect("the next free slot");
    assert_eq!(item.slot, 1);
    let (three, (item, _)) = on(&two, "a", "sled-1", 5)
        .with_sled_placement(&subject("c"), "sled-1", None)
        .expect("the freed slot");
    assert_eq!(item.slot, 0);
    assert_eq!(
        three
            .sled_items()
            .iter()
            .map(|i| (i.slot, i.subject.key()))
            .collect::<Vec<_>>(),
        [
            (0, "inventory:c".to_owned()),
            (1, "inventory:b".to_owned()),
            (5, "inventory:a".to_owned())
        ]
    );
    let mut full = sledded();
    for n in 0..MAX_SLOTS {
        full = full
            .with_sled_placement(&subject(&format!("s{n}")), "sled-1", None)
            .expect("a free slot")
            .0;
    }
    assert!(matches!(
        refused(full.with_sled_placement(&subject("more"), "sled-1", None)),
        LayoutError::Capacity(what) if what == "8 slots on sled-1"
    ));
}

/// Proves: the address of a place is `C0-R<row>-R<rack>-S<sled>-S<slot>`, every number from 0:
/// the row and rack are the layout's numbers less one, the sled is counted from the bottom of its
/// rack (the lowest is 0, whatever order the sleds were added in, and it follows a move), the
/// slot is the slot; a sled or place the layout lacks has none.
///
/// Catches: addresses that count from 1, that count sleds from the top, or that keep a sled's
/// old number after another sled is put below it.
#[test]
fn the_address_counts_from_zero_and_sleds_from_the_bottom() {
    let (layout, row) = Layout::default().with_next_row().expect("a row");
    let (layout, _) = layout.with_next_row().expect("a second row");
    let layout = layout
        .with_rack("", &row.id, 24)
        .and_then(|(l, _)| l.with_rack("", &row.id, 24))
        .expect("racks")
        .0;
    // The first sled added is the highest: sled 0 only until a lower one comes.
    let (layout, _) = layout.with_sled("", "rack-2", 10, 3, 8, "").expect("high");
    assert_eq!(
        layout.sled_address("sled-1").as_deref(),
        Some("C0-R0-R1-S0")
    );
    let (layout, _) = layout.with_sled("", "rack-2", 2, 1, 8, "").expect("low");
    let (layout, _) = layout.with_sled("", "rack-2", 5, 2, 8, "").expect("middle");
    assert_eq!(
        ["sled-2", "sled-3", "sled-1"].map(|id| layout.sled_number(id)),
        [Some(0), Some(1), Some(2)]
    );
    let layout = on(&layout, "a", "sled-3", 0);
    let layout = on(&layout, "b", "sled-3", 1);
    assert_eq!(
        layout.address("sled-3", 0).as_deref(),
        Some("C0-R0-R1-S1-S0")
    );
    assert_eq!(
        layout.address("sled-3", 1).as_deref(),
        Some("C0-R0-R1-S1-S1")
    );
    // The sled moves to the top of the rack: the numbers follow the place.
    let (layout, _) = layout
        .with_sled("sled-3", "rack-2", 20, 2, 8, "")
        .expect("moved");
    assert_eq!(
        layout.address("sled-3", 0).as_deref(),
        Some("C0-R0-R1-S2-S0")
    );
    assert_eq!(layout.address("sled-9", 0), None);
    assert_eq!(layout.sled_number("sled-9"), None);
    // The first rack of the second row is R0 of row R1.
    let (layout, _) = layout
        .with_rack("", "row-2", 10)
        .expect("a rack of the second row");
    let (layout, _) = layout.with_sled("", "rack-3", 1, 1, 8, "").expect("a sled");
    assert_eq!(
        layout.sled_address("sled-4").as_deref(),
        Some("C0-R1-R0-S0")
    );
}

/// Proves: a placement by rack and U makes an implicit sled of that U and height (the next id)
/// with the subject on slot 0; placing the subject again moves it and the empty implicit sled it
/// leaves goes; a sled made by hand stays when its subject leaves it; `PutSled` of an implicit
/// sled keeps it implicit (it still goes with its last subject); unplacing takes the subject off
/// either; and a subject that is not placed is `UnknownPlacement`.
///
/// Catches: a server drawn twice, an implicit sled left behind as a ghost tray, or a sled the
/// operator made that disappears with its server.
#[test]
fn a_placement_by_rack_and_u_makes_a_sled_that_goes_with_its_subject() {
    let (racked, (item, sled)) = racks()
        .with_placement(placement_of(subject("a"), "rack-1", 12, 2))
        .expect("racked");
    assert_eq!((item.sled.as_str(), item.slot), ("sled-1", 0));
    assert_eq!(
        sled,
        Sled {
            id: "sled-1".into(),
            rack: "rack-1".into(),
            u: 12,
            height_u: 2,
            label: String::new(),
            slots: 1,
            implicit: true,
        }
    );
    let (moved, (_, sled)) = racked
        .with_placement(placement_of(subject("a"), "rack-1", 20, 1))
        .expect("moved");
    assert_eq!(
        moved
            .sleds()
            .iter()
            .map(|s| (s.id.as_str(), s.u))
            .collect::<Vec<_>>(),
        [("sled-1", 20)],
        "the old sled went, and its id is the next free one again"
    );
    assert_eq!(sled.id, "sled-1");
    let (kept, _) = sledded()
        .with_sled_placement(&subject("a"), "sled-1", Some(0))
        .expect("on the sled");
    let (to_rack, _) = kept
        .with_placement(placement_of(subject("a"), "rack-1", 12, 1))
        .expect("to a sled of its own");
    assert_eq!(
        to_rack
            .sleds()
            .iter()
            .map(|s| (s.id.as_str(), s.implicit))
            .collect::<Vec<_>>(),
        [("sled-1", false), ("sled-2", true)],
        "the sled made by hand stays empty"
    );
    let (back, _) = to_rack
        .with_sled_placement(&subject("a"), "sled-1", None)
        .expect("back on the sled");
    assert_eq!(back.sleds().len(), 1, "the implicit sled went");
    let (resized, sled) = racked
        .with_sled("sled-1", "rack-1", 12, 3, 8, "Pair")
        .expect("resized");
    assert!(sled.implicit, "a replaced sled keeps what it was");
    let (shared, _) = resized
        .with_sled_placement(&subject("b"), "sled-1", None)
        .expect("a second subject");
    let one_left = shared.without_placement(&subject("a")).expect("off");
    assert_eq!(one_left.sleds().len(), 1, "it still carries b");
    let gone = racked.without_placement(&subject("a")).expect("unplaced");
    assert!(gone.sleds().is_empty() && gone.placements().is_empty());
    assert!(matches!(
        refused(gone.without_placement(&subject("a")).map(|l| (l, ()))),
        LayoutError::UnknownPlacement(key) if key == "inventory:a"
    ));
}

/// Proves: what stands on a sled names the sled, so moving the sled to other U or another rack
/// keeps every subject on it in its slot; and a move onto occupied U is refused.
///
/// Catches: a move that strands the servers, or leaves them at the old U.
#[test]
fn moving_a_sled_moves_everything_on_it() {
    let layout = on(&on(&sledded(), "a", "sled-1", 0), "b", "sled-1", 1);
    let (up, _) = layout
        .with_sled("sled-1", "rack-1", 15, 3, 8, "Studios")
        .expect("moved up");
    let (across, sled) = up
        .with_sled("sled-1", "rack-2", 1, 3, 8, "Studios")
        .expect("moved to another rack");
    assert_eq!((sled.rack.as_str(), sled.u), ("rack-2", 1));
    assert_eq!(
        across
            .placements()
            .iter()
            .map(|(i, s)| (i.subject.key(), s.rack.clone(), s.u, i.slot))
            .collect::<Vec<_>>(),
        [
            ("inventory:a".to_owned(), "rack-2".to_owned(), 1, 0),
            ("inventory:b".to_owned(), "rack-2".to_owned(), 1, 1)
        ]
    );
    // Moved onto another sled's U: refused, and nothing moved.
    let (busy, _) = layout
        .with_placement(placement_of(subject("srv"), "rack-1", 12, 2))
        .expect("a server");
    assert!(matches!(
        refused(busy.with_sled("sled-1", "rack-1", 11, 3, 8, "Studios")),
        LayoutError::Overlap { .. }
    ));
    // A move out of the top of the shorter rack is refused.
    assert!(matches!(
        refused(layout.with_sled("sled-1", "rack-2", 9, 3, 8, "Studios")),
        LayoutError::OutOfRack { .. }
    ));
}

/// Proves: a sled with subjects on it is not removed (`SledInUse` naming them by slot), nor is
/// the rack under it removed; a sled with nothing on it blocks its rack too; once the subjects
/// are off it, the sled and then the rack go.
///
/// Catches: a sled removed with servers left pointing at it.
#[test]
fn a_sled_that_carries_servers_is_not_removed_and_blocks_its_rack() {
    let layout = on(&on(&sledded(), "b", "sled-1", 1), "a", "sled-1", 0);
    match refused(layout.without_sled("sled-1").map(|l| (l, ()))) {
        LayoutError::SledInUse { sled, items } => {
            assert_eq!(sled, "sled-1");
            assert_eq!(items, ["inventory:a", "inventory:b"]);
        }
        other => panic!("not in use: {other}"),
    }
    assert!(matches!(
        layout.without_rack("rack-1"),
        Err(LayoutError::InUse(rack)) if rack == "rack-1"
    ));
    assert!(matches!(
        sledded().without_rack("rack-1"),
        Err(LayoutError::InUse(_))
    ));
    let off = layout
        .without_placement(&subject("a"))
        .and_then(|l| l.without_placement(&subject("b")))
        .expect("unplaced");
    let gone = off.without_sled("sled-1").expect("removed");
    assert!(gone.sleds().is_empty());
    gone.without_rack("rack-1").expect("the rack is free");
    assert!(matches!(
        refused(gone.without_sled("sled-1").map(|l| (l, ()))),
        LayoutError::UnknownSled(_)
    ));
}

/// Proves: removing a switch that stands on a sled frees its slot, and a rack made shorter than a
/// sled it holds is refused.
///
/// Catches: a removed device that stays in its slot, or a rack shrunk under a sled.
#[test]
fn a_removed_device_frees_its_slot_and_a_rack_stays_above_its_sleds() {
    let (layout, device) = sledded()
        .with_device(
            "",
            Device {
                id: String::new(),
                name: "sw".into(),
                role: Segment::Data,
                console_host: String::new(),
                console_device: String::new(),
                management_mac: String::new(),
                notes: String::new(),
            },
        )
        .expect("a device");
    let layout = on(&layout, &device.id, "sled-1", 0);
    assert_eq!(layout.sled_items().len(), 1);
    assert!(
        layout
            .without_device(&device.id)
            .expect("removed")
            .sled_items()
            .is_empty()
    );
    assert!(matches!(
        refused(layout.with_rack("rack-1", "", 6)),
        LayoutError::OutOfRack { .. }
    ));
}

/// Proves: at most 4096 sleds are held (checked with one more than the limit built whole), and
/// placements of the old kind and subjects on sleds count together toward 4096 placements.
///
/// Catches: an unbounded sled list.
#[test]
fn the_layout_holds_at_most_4096_sleds_and_4096_placements() {
    let rows = BTreeMap::from([(
        "row-1".to_owned(),
        Row {
            id: String::new(),
            number: 1,
        },
    )]);
    let racks: BTreeMap<String, Rack> = (1..=70)
        .map(|n| {
            (
                format!("rack-{n}"),
                Rack {
                    id: String::new(),
                    row: "row-1".into(),
                    number: n,
                    height_u: 60,
                },
            )
        })
        .collect();
    let sled_at = |n: usize| {
        let rack = n / 60 + 1;
        (
            format!("sled-{}", n + 1),
            Sled {
                id: String::new(),
                rack: format!("rack-{rack}"),
                u: u32::try_from(n % 60).expect("small") + 1,
                height_u: 1,
                label: String::new(),
                slots: 1,
                implicit: false,
            },
        )
    };
    let sleds: BTreeMap<String, Sled> = (0..MAX_SLEDS).map(sled_at).collect();
    let full = Layout::build(
        Extras {
            sleds: sleds.clone(),
            ..Extras::default()
        },
        String::new(),
        rows.clone(),
        racks.clone(),
        BTreeMap::new(),
    )
    .expect("the most sleds");
    assert_eq!(full.sleds().len(), MAX_SLEDS);
    let mut over = sleds;
    over.extend([sled_at(MAX_SLEDS)]);
    assert!(matches!(
        Layout::build(
            Extras { sleds: over, ..Extras::default() },
            String::new(),
            rows.clone(),
            racks.clone(),
            BTreeMap::new(),
        ),
        Err(LayoutError::Capacity(what)) if what == "4096 sleds"
    ));
    let placements: BTreeMap<String, Placement> = (0..=MAX_PLACEMENTS)
        .map(|n| {
            (
                format!("host:n{n}"),
                placement_of(
                    Subject {
                        kind: Kind::Host,
                        id: format!("n{n}"),
                    },
                    "rack-1",
                    1,
                    1,
                ),
            )
        })
        .collect();
    assert!(matches!(
        Layout::new(String::new(), rows, racks, placements),
        Err(LayoutError::Capacity(what)) if what == "4096 placements"
    ));
}

/// Proves: a layout from before sleds (placements of rack, lowest U and height, in the file and
/// in the table's records) is read as one implicit sled of that U and height for each,
/// numbered in the order of rack, U and key with the same ids on every read; the subject stands on
/// slot 0; what the file writes next has no `placements` and reads back as the same layout; and a
/// placement outside its rack, over another, or with a U of 0 refuses the start as before.
///
/// Catches: placements lost or doubled by the move to sleds, sled ids that change from one
/// start to the next, or a bad old record that starts anyway.
#[test]
fn placements_from_before_sleds_become_one_slot_sleds() {
    let path = Path::new("layout");
    let base = r#"{"rows": {"row-1": {"number": 1}}, "racks": {"rack-1": {"row": "row-1", "number": 1, "height_u": 10}, "rack-2": {"row": "row-1", "number": 2, "height_u": 10}},"#;
    let old = format!(
        r#"{base} "placements": {{"host:z": {{"rack": "rack-2", "u": 3, "height_u": 4}}, "inventory:a": {{"rack": "rack-1", "u": 5, "height_u": 2}}, "host:n1": {{"rack": "rack-1", "u": 1, "height_u": 1}}}}}}"#
    );
    let held = parse_file(path, old.as_bytes()).expect("an old file");
    assert_eq!(
        held.sleds()
            .iter()
            .map(|s| (s.id.as_str(), s.rack.as_str(), s.u, s.height_u, s.implicit))
            .collect::<Vec<_>>(),
        [
            ("sled-1", "rack-1", 1, 1, true),
            ("sled-2", "rack-1", 5, 2, true),
            ("sled-3", "rack-2", 3, 4, true),
        ]
    );
    assert_eq!(
        held.placements()
            .iter()
            .map(|(i, s)| (i.subject.key(), i.sled.clone(), i.slot, s.u))
            .collect::<Vec<_>>(),
        [
            ("host:n1".to_owned(), "sled-1".to_owned(), 0, 1),
            ("inventory:a".to_owned(), "sled-2".to_owned(), 0, 5),
            ("host:z".to_owned(), "sled-3".to_owned(), 0, 3),
        ]
    );
    assert_eq!(
        parse_file(path, old.as_bytes()).expect("again"),
        held,
        "the same on every start"
    );
    let text = encode_file(&held).expect("encodes");
    let json: serde_json::Value = serde_json::from_slice(&text).expect("json");
    assert!(json.get("placements").is_none());
    assert_eq!(json["sleds"]["sled-2"]["implicit"], serde_json::json!(true));
    assert_eq!(parse_file(path, &text).expect("reads back"), held);

    let rows = vec![("row-1".to_owned(), serde_json::json!({"number": 1}))];
    let racks = vec![(
        "rack-1".to_owned(),
        serde_json::json!({"row": "row-1", "number": 1, "height_u": 10}),
    )];
    let records = vec![(
        "host:n1".to_owned(),
        serde_json::json!({"rack": "rack-1", "u": 2, "height_u": 3}),
    )];
    let read = Layout::of_records(
        Path::new("database:lab"),
        String::new(),
        rows,
        racks,
        records,
        Records::default(),
    )
    .expect("the table's old rows");
    assert_eq!(read.sleds().len(), 1);

    for (tail, why) in [
        (
            r#"{"host:a": {"rack": "rack-1", "u": 9, "height_u": 3}}"#,
            "not within U 1 to 10",
        ),
        (
            r#"{"host:a": {"rack": "rack-1", "u": 2, "height_u": 3}, "host:b": {"rack": "rack-1", "u": 4, "height_u": 1}}"#,
            "sled-1 and sled-2 share a U of rack-1",
        ),
        (
            r#"{"host:a": {"rack": "rack-1", "u": 0, "height_u": 1}}"#,
            "a U and a height are 1 or more",
        ),
        (
            r#"{"host:a": {"rack": "rack-1", "u": 1, "height_u": 0}}"#,
            "a U and a height are 1 or more",
        ),
        (
            r#"{"host:a": {"rack": "rack-9", "u": 1, "height_u": 1}}"#,
            "there is no rack rack-9",
        ),
    ] {
        let text = format!("{base} \"placements\": {tail}}}");
        let refused = parse_file(path, text.as_bytes()).expect_err(tail);
        assert!(refused.to_string().contains(why), "{refused}");
    }
}

/// Proves: the file writes `sleds` and `sled_items` and reads them back whole, a file without
/// them (what the release before wrote) loads as none and writes none, a file with a sled the
/// release before would not know still reads, a bad sled or slot or a subject in two places is
/// refused naming the file, and the stored records of the tables read as strictly.
///
/// Catches: sleds that do not survive a restart, or an old file the new build cannot start on.
#[test]
fn the_file_and_the_records_hold_the_sleds() {
    let layout = on(&sledded(), "a", "sled-1", 1);
    let path = Path::new("layout");
    let text = encode_file(&layout).expect("encodes");
    let json: serde_json::Value = serde_json::from_slice(&text).expect("json");
    assert_eq!(
        json["sleds"]["sled-1"],
        serde_json::json!({"rack": "rack-1", "u": 5, "height_u": 3, "label": "Studios", "slots": 8})
    );
    assert_eq!(
        json["sled_items"]["inventory:a"],
        serde_json::json!({"sled": "sled-1", "slot": 1})
    );
    assert!(json.get("placements").is_none());
    assert_eq!(parse_file(path, &text).expect("reads"), layout);

    let old = br#"{"name": "Lab", "rows": {"row-1": {"number": 1}}}"#;
    let held = parse_file(path, old).expect("an old file");
    assert!(held.sleds().is_empty() && held.sled_items().is_empty());
    let plain = String::from_utf8(encode_file(&Layout::default()).expect("encodes")).expect("text");
    assert!(!plain.contains("sleds") && !plain.contains("sled_items"));

    let base = r#"{"rows": {"row-1": {"number": 1}}, "racks": {"rack-1": {"row": "row-1", "number": 1, "height_u": 10}},"#;
    let one = r#""sleds": {"sled-1": {"rack": "rack-1", "u": 1, "height_u": 2}}"#;
    let bad = [
        r#""sleds": {"sled-1": {"rack": "rack-1", "u": 1, "height_u": 0}}}"#.to_owned(),
        r#""sleds": {"sled-x": {"rack": "rack-1", "u": 1, "height_u": 2}}}"#.to_owned(),
        format!(r#"{one}, "sled_items": {{"inventory:a": {{"sled": "sled-2", "slot": 0}}}}}}"#),
        format!(r#"{one}, "sled_items": {{"inventory:a": {{"sled": "sled-1", "slot": 8}}}}}}"#),
        format!(r#"{one}, "sled_items": {{"cat:a": {{"sled": "sled-1", "slot": 0}}}}}}"#),
        format!(
            r#"{one}, "sled_items": {{"inventory:a": {{"sled": "sled-1", "slot": 0}}, "inventory:b": {{"sled": "sled-1", "slot": 0}}}}}}"#
        ),
        format!(
            r#"{one}, "placements": {{"inventory:a": {{"rack": "rack-1", "u": 5, "height_u": 1}}}}, "sled_items": {{"inventory:a": {{"sled": "sled-1", "slot": 0}}}}}}"#
        ),
        r#""sleds": {"sled-1": {"rack": "rack-1", "u": 1, "height_u": 2}, "sled-2": {"rack": "rack-1", "u": 2, "height_u": 2}}}"#.to_owned(),
    ];
    for tail in &bad {
        let text = format!("{base} {tail}");
        let refused = parse_file(path, text.as_bytes()).expect_err(tail);
        assert!(refused.to_string().starts_with("layout: "), "{refused}");
    }
    let unknown_key = format!("{base} \"sleds\": {{}}, \"a_later_key\": {{\"x\": 1}}}}");
    parse_file(path, unknown_key.as_bytes()).expect("a key a later build adds is skipped");

    let place = Path::new("database:lab");
    let rack = vec![(
        "rack-1".to_owned(),
        serde_json::json!({"row": "row-1", "number": 1, "height_u": 10}),
    )];
    let rows = vec![("row-1".to_owned(), serde_json::json!({"number": 1}))];
    let records = Records {
        devices: vec![],
        sleds: vec![(
            "sled-1".to_owned(),
            serde_json::json!({"rack": "rack-1", "u": 1, "height_u": 2}),
        )],
        items: vec![(
            "host:node-1".to_owned(),
            serde_json::json!({"sled": "sled-1", "slot": 0}),
        )],
    };
    let read = Layout::of_records(
        place,
        String::new(),
        rows.clone(),
        rack.clone(),
        vec![],
        records,
    )
    .expect("reads");
    assert_eq!((read.sleds().len(), read.sled_items().len()), (1, 1));
    for broken in [
        Records {
            sleds: vec![("sled-1".to_owned(), serde_json::json!({"height_u": "x"}))],
            ..Records::default()
        },
        Records {
            items: vec![("host:n".to_owned(), serde_json::json!({"sled": 5}))],
            ..Records::default()
        },
    ] {
        let why = Layout::of_records(
            place,
            String::new(),
            rows.clone(),
            rack.clone(),
            vec![],
            broken,
        )
        .expect_err("not a record");
        assert!(why.to_string().contains("the record "), "{why}");
    }
}

/// Proves: a sled declares 1 to 8 slots (0 and 9 are refused), a record with no `slots` reads as
/// a one-slot sled and a one-slot sled writes none, a sled made smaller than the slot of a subject
/// on it is `SledInUse` naming the subject, one made smaller than that is fine while the slots
/// above it are empty, and a full sled of 2 slots refuses a third.
///
/// Catches: a sled that holds more than it declares, or a shrink that strands a server in a bay
/// that no longer exists.
#[test]
fn a_sled_declares_its_slots_and_cannot_shrink_under_a_subject() {
    let base = racks();
    for slots in [0, 9] {
        assert!(matches!(
            refused(base.with_sled("", "rack-1", 5, 3, slots, "")),
            LayoutError::Invalid(why) if why.contains("slots are 1 to 8")
        ));
    }
    let (two, _) = base
        .with_sled("", "rack-1", 5, 2, 2, "")
        .expect("two slots");
    let on_one = on(&two, "a", "sled-1", 1);
    assert!(matches!(
        refused(on_one.with_sled("sled-1", "rack-1", 5, 2, 1, "")),
        LayoutError::SledInUse { sled, items } if sled == "sled-1" && items == ["inventory:a"]
    ));
    let (grown, kept) = on_one
        .with_sled("sled-1", "rack-1", 5, 2, 3, "")
        .expect("more slots keep the subject in its slot");
    assert_eq!(kept.slots, 3);
    assert_eq!(grown.sled_item(&subject("a")).map(|i| i.slot), Some(1));
    let (one, _) = two
        .with_sled("sled-1", "rack-1", 5, 2, 1, "")
        .expect("empty");
    assert_eq!(one.sled("sled-1").map(|s| s.slots), Some(1));
    let full = on(&on(&two, "a", "sled-1", 0), "b", "sled-1", 1);
    assert!(matches!(
        refused(full.with_sled_placement(&subject("c"), "sled-1", None)),
        LayoutError::Capacity(what) if what == "2 slots on sled-1"
    ));
    let text = br#"{"rows":{"row-1":{"number":1}},"racks":{"rack-1":{"row":"row-1","number":1,"height_u":10}},"sleds":{"sled-1":{"rack":"rack-1","u":2,"height_u":1}}}"#;
    let read = parse_file(Path::new("layout"), text).expect("a record without slots");
    assert_eq!(read.sled("sled-1").map(|s| s.slots), Some(1));
    let json: serde_json::Value =
        serde_json::from_slice(&encode_file(&read).expect("encodes")).expect("json");
    assert_eq!(json["sleds"]["sled-1"].get("slots"), None);
}
