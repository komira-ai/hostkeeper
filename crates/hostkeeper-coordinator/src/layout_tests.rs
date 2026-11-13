//! The lab's layout: the rules of a name, a height, a placement and the capacity, the changes that
//! each judge the whole result, the file the layout is written to, and the store that commits each
//! change before memory holds it. No clock and no timing.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use super::{
    Kind, Layout, LayoutError, LayoutStore, MAX_HEIGHT_U, MAX_NAME_CHARS, MAX_PLACEMENTS,
    MAX_RACKS, MAX_ROWS, Placement, Rack, Row, Subject, encode_file, layout_path, parse_file,
};
use crate::operator_layout::placement_of;
use crate::store::conformance_tests::rich_layout;
use crate::store::{Fake, Shared};
use crate::testutil::scratch;

fn host(id: &str) -> Subject {
    Subject {
        kind: Kind::Host,
        id: id.to_owned(),
    }
}

/// A layout of the row `row-1` with the rack `rack-1`, 10 U high.
fn small() -> Layout {
    let (held, _) = Layout::default().with_next_row().expect("a row");
    held.with_rack("", "row-1", 10).expect("a rack").0
}

/// Proves: the cluster's name is 1 to 32 characters of letters, digits, space, `.`, `_` and `-`
/// with no leading or trailing space, and a layout may be unnamed.
///
/// Catches: a name the page cannot draw (markup, a long run).
#[test]
fn the_cluster_name_is_the_short_plain_kind() {
    for bad in [
        "",
        " A",
        "A ",
        "A/B",
        "<b>",
        &"x".repeat(MAX_NAME_CHARS + 1),
    ] {
        assert!(
            matches!(
                Layout::default().with_name(bad),
                Err(LayoutError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    let named = Layout::default().with_name("Lab 1.a_b-c").expect("named");
    assert_eq!(named.name(), "Lab 1.a_b-c");
    assert!(!named.is_empty());
    assert!(Layout::default().is_empty());
    assert!(
        Layout::default()
            .with_name(&"x".repeat(MAX_NAME_CHARS))
            .is_ok()
    );
    assert_eq!(Layout::default().name(), "");
}

/// Proves: a rack is 1 to 60 U high and stands in a row the layout holds; rows and racks are
/// numbered by the coordinator from 1, racks within their row, and a new one takes one more than
/// the highest held, so a removed rack's number is reused only when it was the highest.
///
/// Catches: a rack of 0 U, one in a removed row, or two racks that share a number.
#[test]
fn rows_and_racks_are_numbered_and_a_rack_has_a_height_and_a_row() {
    let held = small();
    for bad in [0, MAX_HEIGHT_U + 1] {
        assert!(matches!(
            held.with_rack("", "row-1", bad),
            Err(LayoutError::Invalid(_))
        ));
    }
    assert!(held.with_rack("", "row-1", 1).is_ok());
    assert!(held.with_rack("", "row-1", MAX_HEIGHT_U).is_ok());
    assert!(matches!(
        held.with_rack("", "row-9", 5),
        Err(LayoutError::UnknownRow(id)) if id == "row-9"
    ));
    let (held, second) = held.with_next_row().expect("a row");
    assert_eq!((second.id.as_str(), second.number), ("row-2", 2));
    let (held, rack2) = held.with_rack("", "row-1", 4).expect("a rack");
    assert_eq!((rack2.id.as_str(), rack2.number), ("rack-2", 2));
    let (held, other) = held.with_rack("", "row-2", 4).expect("a rack of row 2");
    assert_eq!((other.id.as_str(), other.number), ("rack-3", 1));
    let held = held.without_rack("rack-2").expect("removed");
    let (_, again) = held.with_rack("", "row-1", 4).expect("a rack");
    assert_eq!((again.id.as_str(), again.number), ("rack-4", 2));
    assert!(matches!(
        held.with_rack("rack-7", "", 4),
        Err(LayoutError::UnknownRack(_))
    ));
    assert_eq!(held.rows().len(), 2);
}

/// Proves: a rack resized keeps its id and number; a rack moved to another row takes that row's
/// next number and keeps its placements; naming no row keeps the row.
///
/// Catches: a resize that makes a new rack, or a move that leaves two racks of one number.
#[test]
fn a_rack_is_resized_and_moved_between_rows() {
    let (held, _) = small().with_next_row().expect("row 2");
    let (held, _) = held.with_rack("", "row-2", 5).expect("rack in row 2");
    let (held, _) = held
        .with_placement(placement_of(host("n1"), "rack-1", 2, 1))
        .expect("placed");
    let (resized, rack) = held.with_rack("rack-1", "", 20).expect("resized");
    assert_eq!(
        (rack.id.as_str(), rack.number, rack.height_u),
        ("rack-1", 1, 20)
    );
    assert_eq!(rack.row, "row-1");
    let (moved, rack) = held.with_rack("rack-1", "row-2", 10).expect("moved");
    assert_eq!((rack.row.as_str(), rack.number), ("row-2", 2));
    assert!(moved.placement(&host("n1")).is_some());
    assert_eq!(resized.racks().len(), 2);
}

/// Proves: a row that holds a rack and a rack that holds a placement are not removed (`InUse`), an
/// empty one is, and removing what is not held is `UnknownRow` or `UnknownRack`.
///
/// Catches: a removal that strands racks without a row, or hosts without a rack.
#[test]
fn a_row_with_racks_and_a_rack_with_placements_stay() {
    let held = small();
    assert!(matches!(
        held.without_row("row-1"),
        Err(LayoutError::InUse(id)) if id == "row-1"
    ));
    let (placed, _) = held
        .with_placement(placement_of(host("n1"), "rack-1", 3, 1))
        .expect("placed");
    assert!(matches!(
        placed.without_rack("rack-1"),
        Err(LayoutError::InUse(_))
    ));
    let held = held.without_rack("rack-1").expect("an empty rack goes");
    let held = held.without_row("row-1").expect("an empty row goes");
    assert!(held.is_empty());
    assert!(matches!(
        held.without_row("row-1"),
        Err(LayoutError::UnknownRow(_))
    ));
    assert!(matches!(
        held.without_rack("rack-1"),
        Err(LayoutError::UnknownRack(_))
    ));
}

/// Proves: a placement occupies U `u` to `u + height_u - 1` of its rack: the bottom U and the top
/// U are in, U 0 and the U above the rack are out, a height of 0 or one that reaches past the top
/// is out; and a placement in a rack the layout lacks is `UnknownRack`.
///
/// Catches: an off-by-one at either end of a rack, which lets a host hang off its elevation.
#[test]
fn a_placement_is_within_u_one_to_the_height_of_its_rack() {
    let held = small();
    let place = |u, height| {
        held.with_placement(Placement {
            subject: host("n1"),
            rack: "rack-1".into(),
            u,
            height_u: height,
        })
    };
    assert!(place(1, 1).is_ok(), "the bottom U");
    assert!(place(10, 1).is_ok(), "the top U");
    assert!(place(1, 10).is_ok(), "the whole rack");
    assert!(matches!(place(0, 1), Err(LayoutError::Invalid(_))));
    assert!(matches!(place(1, 0), Err(LayoutError::Invalid(_))));
    for (u, height) in [(11, 1), (9, 3), (10, 2)] {
        assert!(
            matches!(
                place(u, height),
                Err(LayoutError::OutOfRack { height: 10, .. })
            ),
            "{u} {height}"
        );
    }
    assert!(matches!(
        held.with_placement(placement_of(host("n1"), "rack-9", 1, 1)),
        Err(LayoutError::UnknownRack(_))
    ));
}

/// Proves: two placements of a rack may not share a U: neighbours that touch are accepted, one U
/// of overlap is `Overlap` naming the rack and both subjects; a subject moved over its own old U
/// is accepted, since it leaves them.
///
/// Catches: two servers drawn in one slot.
#[test]
fn placements_of_a_rack_never_share_a_u() {
    let held = small();
    let (held, _) = held
        .with_placement(placement_of(host("a"), "rack-1", 3, 2))
        .expect("U 3 and 4");
    let (touching, _) = held
        .with_placement(placement_of(host("b"), "rack-1", 1, 2))
        .expect("U 1 and 2 touch U 3");
    assert_eq!(touching.placements().len(), 2);
    let refused = held
        .with_placement(placement_of(host("b"), "rack-1", 2, 2))
        .expect_err("U 2 and 3 share U 3");
    assert!(
        matches!(&refused, LayoutError::Overlap { rack, first, second }
            if rack == "rack-1" && first == "host:b" && second == "host:a"),
        "{refused}"
    );
    assert_eq!(refused.to_string(), "host:b and host:a share a U of rack-1");
    let (_, moved) = held
        .with_placement(placement_of(host("a"), "rack-1", 3, 3))
        .expect("a subject overlaps only others");
    assert_eq!(moved.height_u, 3);
    let (held, _) = touching
        .with_placement(placement_of(host("c"), "rack-1", 8, 1))
        .expect("far away");
    assert!(matches!(
        held.with_placement(placement_of(host("c"), "rack-1", 1, 1)),
        Err(LayoutError::Overlap { .. })
    ));
}

/// Proves: a subject has one placement: placing it again moves it, and unplacing it removes it,
/// leaving `UnknownPlacement` for a second time; a host and an inventory entry of the same id are
/// two subjects; a subject's id is 1 to 128 visible ASCII bytes.
///
/// Catches: a host drawn twice, after a move that added instead of moving.
#[test]
fn a_subject_is_placed_once_and_a_host_and_an_entry_of_one_id_are_two() {
    let held = small();
    let (held, _) = held
        .with_placement(placement_of(host("n1"), "rack-1", 2, 1))
        .expect("placed");
    let (held, _) = held
        .with_placement(placement_of(host("n1"), "rack-1", 9, 1))
        .expect("moved");
    assert_eq!(held.placements().len(), 1);
    assert_eq!(held.placement(&host("n1")).map(|p| p.u), Some(9));
    let entry = Subject {
        kind: Kind::Inventory,
        id: "n1".to_owned(),
    };
    let (held, _) = held
        .with_placement(placement_of(entry.clone(), "rack-1", 2, 1))
        .expect("an entry of the same id");
    assert_eq!(held.placements().len(), 2);
    assert_eq!(entry.key(), "inventory:n1");
    let held = held.without_placement(&host("n1")).expect("unplaced");
    assert!(held.placement(&host("n1")).is_none());
    assert!(matches!(
        held.without_placement(&host("n1")),
        Err(LayoutError::UnknownPlacement(key)) if key == "host:n1"
    ));
    for bad in ["", "has space", "tab\t", "é", &"x".repeat(129)] {
        assert!(
            matches!(
                held.with_placement(placement_of(host(bad), "rack-1", 5, 1)),
                Err(LayoutError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert!(
        held.with_placement(placement_of(host(&"x".repeat(128)), "rack-1", 5, 1))
            .is_ok()
    );
}

/// Proves: a rack made shorter than a placement it holds is `OutOfRack`, and a rack made as tall as
/// its placements need is accepted.
///
/// Catches: a resize that strands a host above its rack's top.
#[test]
fn a_rack_cannot_be_made_shorter_than_what_it_holds() {
    let (held, _) = small()
        .with_placement(placement_of(host("n1"), "rack-1", 7, 2))
        .expect("placed");
    assert!(matches!(
        held.with_rack("rack-1", "", 7),
        Err(LayoutError::OutOfRack { .. })
    ));
    assert!(held.with_rack("rack-1", "", 8).is_ok());
}

/// Proves: the layout holds at most 64 rows, 512 racks and 4096 placements, and says which.
///
/// Catches: a layout grown without bound by a script.
#[test]
fn the_layout_holds_at_most_sixty_four_rows_512_racks_and_4096_placements() {
    let rows = |n: usize| -> BTreeMap<String, Row> {
        (1..=n)
            .map(|i| {
                (
                    format!("row-{i}"),
                    Row {
                        number: u32::try_from(i).expect("small"),
                        ..Row::default()
                    },
                )
            })
            .collect()
    };
    let name = String::new();
    assert!(
        Layout::new(
            name.clone(),
            rows(MAX_ROWS),
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_ok()
    );
    let refused = Layout::new(
        name.clone(),
        rows(MAX_ROWS + 1),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect_err("65 rows");
    assert_eq!(refused.to_string(), "the layout holds at most 64 rows");

    let racks = |n: usize| -> BTreeMap<String, Rack> {
        (1..=n)
            .map(|i| {
                (
                    format!("rack-{i}"),
                    Rack {
                        row: "row-1".into(),
                        number: u32::try_from(i).expect("small"),
                        height_u: 2,
                        ..Rack::default()
                    },
                )
            })
            .collect()
    };
    assert!(Layout::new(name.clone(), rows(1), racks(MAX_RACKS), BTreeMap::new()).is_ok());
    let refused =
        Layout::new(name.clone(), rows(1), racks(MAX_RACKS + 1), BTreeMap::new()).expect_err("513");
    assert_eq!(refused.to_string(), "the layout holds at most 512 racks");

    let many: BTreeMap<String, Placement> = (0..=MAX_PLACEMENTS)
        .map(|i| {
            (
                format!("host:h{i}"),
                Placement {
                    subject: host("x"),
                    rack: "rack-1".into(),
                    u: 1,
                    height_u: 1,
                },
            )
        })
        .collect();
    let refused = Layout::new(name, rows(1), racks(1), many).expect_err("4097");
    assert_eq!(
        refused.to_string(),
        "the layout holds at most 4096 placements"
    );
}

/// Proves: the lists come in the order the page draws: rows by number, racks by row number then
/// rack number, placements by rack and lowest U.
///
/// Catches: a floor plan that reshuffles between reads.
#[test]
fn the_lists_come_in_the_order_the_page_draws() {
    let (held, _) = Layout::default().with_next_row().expect("row");
    let (held, _) = held.with_next_row().expect("row");
    let (held, _) = held.with_rack("", "row-2", 5).expect("rack");
    let (held, _) = held.with_rack("", "row-1", 5).expect("rack");
    let (held, _) = held.with_rack("", "row-1", 7).expect("rack");
    let order: Vec<_> = held
        .racks()
        .iter()
        .map(|r| (r.row.clone(), r.number))
        .collect();
    assert_eq!(
        order,
        [
            ("row-1".to_owned(), 1),
            ("row-1".to_owned(), 2),
            ("row-2".to_owned(), 1)
        ]
    );
    let numbers: Vec<_> = held.rows().iter().map(|r| r.number).collect();
    assert_eq!(numbers, [1, 2]);
    let (held, _) = held
        .with_placement(placement_of(host("high"), "rack-2", 5, 1))
        .expect("p");
    let (held, _) = held
        .with_placement(placement_of(host("low"), "rack-2", 1, 1))
        .expect("p");
    let ids: Vec<_> = held
        .placements()
        .iter()
        .map(|p| p.subject.id.clone())
        .collect();
    assert_eq!(ids, ["low", "high"]);
    assert_eq!(held.row("row-1").map(|r| r.number), Some(1));
    assert_eq!(held.rack("rack-2").map(|r| r.height_u), Some(5));
}

/// Proves: the file is the layout as JSON, written with each empty part left out, read back
/// equal; the path is the state file's with `.layout`; and a file that is not JSON of the shape, or
/// that breaks a rule, is refused naming the file; a key of a later build is skipped.
///
/// Catches: a layout that reads back different, or a bad file that starts the coordinator with a
/// guess.
#[test]
fn the_file_reads_back_the_layout_and_a_bad_file_is_refused_by_name() {
    let held = rich_layout();
    let text = encode_file(&held).expect("encodes");
    assert!(text.ends_with(b"\n"));
    let path = layout_path(std::path::Path::new("/s/desired.json"));
    assert_eq!(path.to_string_lossy(), "/s/desired.json.layout");
    assert_eq!(parse_file(&path, &text).expect("reads"), held);
    assert_eq!(encode_file(&Layout::default()).expect("encodes"), b"{}\n");
    assert_eq!(parse_file(&path, b"{}").expect("empty"), Layout::default());

    for bad in [
        "not json",
        r#"{"name":"A<"}"#,
        r#"{"rows":{"row-1":{"number":0}}}"#,
        r#"{"rows":{"row-1":{"number":1},"row-2":{"number":1}}}"#,
        r#"{"rows":{"row-x":{"number":1}}}"#,
        r#"{"rows":{"row-1234567890":{"number":1}}}"#,
        r#"{"racks":{"rack-1":{"row":"row-1","number":1,"height_u":5}}}"#,
        r#"{"rows":{"row-1":{"number":1}},"racks":{"rack-1":{"row":"row-1","number":1,"height_u":61}}}"#,
        r#"{"rows":{"row-1":{"number":1}},"racks":{"rack-x":{"row":"row-1","number":1,"height_u":5}}}"#,
        r#"{"rows":{"row-1":{"number":1}},"racks":{"rack-1":{"row":"row-1","number":0,"height_u":5}}}"#,
        r#"{"rows":{"row-1":{"number":1}},"racks":{"rack-1":{"row":"row-1","number":1,"height_u":5},"rack-2":{"row":"row-1","number":1,"height_u":5}}}"#,
    ] {
        let refused = parse_file(&path, bad.as_bytes()).expect_err("refused");
        assert!(
            matches!(&refused, LayoutError::Stored { path: p, .. } if p == &path),
            "{refused}"
        );
        assert!(refused.to_string().starts_with("/s/desired.json.layout: "));
        assert!(
            refused
                .to_string()
                .ends_with("so fix or remove the file to start")
        );
    }
    let key = parse_file(
        &path,
        br#"{"placements":{"nokind":{"rack":"r","u":1,"height_u":1}}}"#,
    )
    .expect_err("a key without a kind");
    assert!(key.to_string().contains("not kind:id"), "{key}");
    let kind = parse_file(
        &path,
        br#"{"placements":{"vm:x":{"rack":"r","u":1,"height_u":1}}}"#,
    )
    .expect_err("another kind");
    assert!(kind.to_string().contains("host or inventory"), "{kind}");
    let later = parse_file(&path, br#"{"later":{"x":1},"rows":{"row-1":{"number":1}}}"#)
        .expect("a key of a later build is skipped");
    assert_eq!(later.rows().len(), 1);
}

/// Proves: the records of a database read through the same rules: a record that is not a row is
/// refused naming the record and the table, and a layout that breaks a rule is refused with it.
///
/// Catches: a database loaded more loosely than a file.
#[test]
fn records_are_read_as_strictly_as_the_file() {
    let place = std::path::Path::new("database:lab");
    let rows = vec![("row-1".to_owned(), serde_json::json!({"number": 1}))];
    let ok = Layout::of_records(place, "Lab".into(), rows.clone(), vec![], vec![], vec![])
        .expect("reads");
    assert_eq!((ok.rows().len(), ok.name()), (1, "Lab"));
    let bad = Layout::of_records(
        place,
        String::new(),
        vec![("row-1".to_owned(), serde_json::json!({"number": "x"}))],
        vec![],
        vec![],
        vec![],
    )
    .expect_err("not a row");
    assert!(
        bad.to_string()
            .starts_with("database:lab: the record row-1: "),
        "{bad}"
    );
    let broken = Layout::of_records(
        place,
        String::new(),
        rows,
        vec![(
            "rack-1".to_owned(),
            serde_json::json!({"row": "row-2", "number": 1, "height_u": 5}),
        )],
        vec![],
        vec![],
    )
    .expect_err("a rack in no row");
    assert!(
        broken.to_string().contains("there is no row row-2"),
        "{broken}"
    );
}

/// Proves: a store changes the layout by committing it first: a change the backend refuses leaves
/// memory as it was, a change that leaves the layout as it was writes nothing, and an edit that
/// refuses is its own refusal and writes nothing either.
///
/// Catches: memory that holds a layout the disk does not, so a restart loses a placement.
#[test]
fn a_store_commits_a_change_before_memory_holds_it() {
    let fake = Fake::new();
    let backend: Shared = Arc::new(fake.clone());
    let store = LayoutStore::over(Arc::clone(&backend), Layout::default());
    let row = store.change(Layout::with_next_row).expect("a row");
    assert_eq!(row.id, "row-1");
    assert_eq!(fake.layout(), store.snapshot(), "committed");
    let commits = fake.commits();

    store
        .change(|held| held.with_name("Lab").map(|next| (next, ())))
        .expect("named");
    let commits = fake.commits().max(commits);
    store
        .change(|held| held.with_name("Lab").map(|next| (next, ())))
        .expect("the same name again");
    assert_eq!(
        fake.commits(),
        commits,
        "a change of nothing writes nothing"
    );

    let refused = store.change(|held| held.without_row("row-9").map(|next| (next, ())));
    assert!(matches!(refused, Err(LayoutError::UnknownRow(_))));
    assert_eq!(fake.commits(), commits);

    fake.fail();
    let failed = store.change(Layout::with_next_row);
    assert!(matches!(failed, Err(LayoutError::Io(_))), "{failed:?}");
    assert_eq!(store.snapshot().rows().len(), 1, "memory is as it was");
    fake.heal();
    assert!(store.change(Layout::with_next_row).is_ok());
    assert!(format!("{store:?}").starts_with("LayoutStore"));
    assert!(LayoutStore::volatile().snapshot().is_empty());
}

/// Proves: a store over a file keeps the layout across a restart, starts empty when the file is
/// not there, writes the file at the owner's mode, and is refused when the file is bad or cannot
/// be read.
///
/// Catches: a layout that is lost at a restart.
#[test]
fn a_store_over_a_file_survives_a_restart_and_refuses_a_bad_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("layout-store-file");
    let path = dir.path().join("desired.json.layout");
    let store = LayoutStore::open(&path).expect("no file is an empty layout");
    assert!(store.snapshot().is_empty());
    store.change(Layout::with_next_row).expect("a row");
    let mode = fs::metadata(&path).expect("written").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let again = LayoutStore::open(&path).expect("reads");
    assert_eq!(again.snapshot(), store.snapshot());
    let mut temp = path.clone().into_os_string();
    temp.push(".new");
    fs::create_dir(&temp).expect("a directory where the temporary file goes");
    let failed = store.change(Layout::with_next_row);
    assert!(matches!(failed, Err(LayoutError::Io(_))), "{failed:?}");
    assert_eq!(store.snapshot().rows().len(), 1, "memory is as it was");
    fs::remove_dir(&temp).expect("removed");
    fs::write(&path, "{").expect("a bad file");
    let refused = LayoutStore::open(&path).expect_err("refused");
    assert!(matches!(refused, LayoutError::Stored { .. }));
    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory in its place");
    assert!(matches!(LayoutStore::open(&path), Err(LayoutError::Io(_))));
}
