//! The network devices of the layout ([`super`]): each field's rule, the names, the ids, what a
//! change of a device does to its placement, and what a file or a record holds. No clock.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Device, MAX_DEVICES, Segment};
use crate::layout::{Kind, Layout, LayoutError, Subject, encode_file, parse_file};

fn device(name: &str) -> Device {
    Device {
        id: String::new(),
        name: name.to_owned(),
        role: Segment::Data,
        console_host: String::new(),
        console_device: String::new(),
        management_mac: String::new(),
        notes: String::new(),
    }
}

fn with(edit: impl FnOnce(&mut Device)) -> Device {
    let mut d = device("sw");
    edit(&mut d);
    d
}

fn rack_layout() -> Layout {
    let (next, row) = Layout::default().with_next_row().expect("a row");
    next.with_rack("", &row.id, 24).expect("a rack").0
}

fn invalid(layout: &Layout, d: Device) -> String {
    match layout.with_device("", d).expect_err("refused") {
        LayoutError::Invalid(why) => why,
        other => panic!("not an invalid field: {other}"),
    }
}

/// Proves: a device is added under the next `net.<n>` id (1, then one past the highest, so a gap
/// is not reused downward), its MAC is kept in one spelling and its notes trimmed, and an id it is
/// given replaces every field of that device.
///
/// Catches: a second device that takes the first's id, or a MAC kept as typed.
#[test]
fn a_device_is_added_under_the_next_id_and_replaced_by_its_id() {
    let (one, first) = Layout::default()
        .with_device(
            "",
            with(|d| {
                d.management_mac = "AA-BB-CC-DD-EE-FF".into();
                d.notes = "  top of rack \n".into();
            }),
        )
        .expect("added");
    assert_eq!(first.id, "net.1");
    assert_eq!(first.management_mac, "aa:bb:cc:dd:ee:ff");
    assert_eq!(first.notes, "top of rack");
    let (two, second) = one.with_device("", device("sw-2")).expect("added");
    assert_eq!(second.id, "net.2");
    let without = two.without_device("net.1").expect("removed");
    let (_, third) = without.with_device("", device("sw-3")).expect("added");
    assert_eq!(third.id, "net.3");

    let (replaced, kept) = two
        .with_device("net.1", with(|d| d.role = Segment::Bmc))
        .expect("replaced");
    assert_eq!((kept.id.as_str(), kept.role), ("net.1", Segment::Bmc));
    assert!(kept.management_mac.is_empty(), "every field is replaced");
    assert_eq!(replaced.devices().len(), 2);
    assert_eq!(
        replaced.device("net.2").map(|d| d.name.as_str()),
        Some("sw-2")
    );
    assert!(replaced.device("net.9").is_none());
}

/// Proves: each field's rule is refused with the field named: the name's set and length, the
/// console's host and device together and each in its form, the MAC, the notes' length and
/// control characters; and a device whose name another has is `NameTaken`, one past 256 is
/// `Capacity`, an id that is not `net.<n>` is invalid, and an unknown id is `UnknownDevice`.
///
/// Catches: a field a switch could carry that a console or a file would choke on later.
#[test]
fn each_field_of_a_device_is_checked() {
    let layout = Layout::default();
    assert!(invalid(&layout, device("")).contains("the name"));
    assert!(invalid(&layout, device("bad/name")).contains("the name"));
    assert!(invalid(&layout, device(&"n".repeat(33))).contains("the name"));
    assert!(layout.with_device("", device(&"n".repeat(32))).is_ok());

    let together = "give the console's host and device together";
    assert!(invalid(&layout, with(|d| d.console_host = "node-1".into())).contains(together));
    assert!(invalid(&layout, with(|d| d.console_device = "/dev/ttyS0".into())).contains(together));
    let form = "the console is a host id";
    for (host, path) in [
        ("Node_1", "/dev/ttyS0"),
        ("-node", "/dev/ttyS0"),
        ("node-", "/dev/ttyS0"),
        (&*"n".repeat(64), "/dev/ttyS0"),
        ("node-1", "ttyS0"),
        ("node-1", "/dev/"),
        ("node-1", "/dev/../etc/passwd"),
        ("node-1", "/dev/tty S0"),
        ("node-1", &*format!("/dev/{}", "t".repeat(44))),
    ] {
        let why = invalid(
            &layout,
            with(|d| {
                d.console_host = host.to_owned();
                d.console_device = path.to_owned();
            }),
        );
        assert!(why.contains(form), "{host} {path}: {why}");
    }
    let (_, ok) = layout
        .with_device(
            "",
            with(|d| {
                d.console_host = "node-1".into();
                d.console_device = "/dev/serial/by-id/usb-1.0_x".into();
            }),
        )
        .expect("a console");
    assert_eq!(ok.console_device, "/dev/serial/by-id/usb-1.0_x");

    assert!(invalid(&layout, with(|d| d.management_mac = "aa:bb".into())).contains("MAC"));
    assert!(invalid(&layout, with(|d| d.notes = "x".repeat(257))).contains("notes"));
    assert!(invalid(&layout, with(|d| d.notes = "a\u{7}b".into())).contains("notes"));
    let (_, line) = layout
        .with_device("", with(|d| d.notes = "a\nb".into()))
        .expect("a line break is allowed");
    assert_eq!(line.notes, "a\nb");
    let (_, blank) = layout
        .with_device("", with(|d| d.notes = " \n ".into()))
        .expect("blank notes are none");
    assert!(blank.notes.is_empty());

    let (held, _) = layout.with_device("", device("sw")).expect("added");
    assert!(matches!(
        held.with_device("", device("sw")),
        Err(LayoutError::NameTaken(name)) if name == "sw"
    ));
    assert!(matches!(
        held.with_device("net.7", device("other")),
        Err(LayoutError::UnknownDevice(id)) if id == "net.7"
    ));
    assert!(matches!(
        held.without_device("net.7"),
        Err(LayoutError::UnknownDevice(_))
    ));

    let many: BTreeMap<String, Device> = (1..=MAX_DEVICES)
        .map(|n| (format!("net.{n}"), device(&format!("sw-{n}"))))
        .collect();
    let full = Layout::build(
        many.clone(),
        String::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("256 devices");
    assert!(matches!(
        full.with_device("", device("one-more")),
        Err(LayoutError::Capacity(_))
    ));
    for bad in ["switch-1", "net.", "net.1234567890", "net.x", "1"] {
        let mut one = BTreeMap::new();
        one.insert(bad.to_owned(), device("sw"));
        let refused = Layout::build(
            one,
            String::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        assert!(matches!(refused, Err(LayoutError::Invalid(_))), "{bad}");
    }
}

/// Proves: a device is placed by the inventory subject of its id like a server entry, shares no U
/// with another placement, keeps its place and its record across a change of another part of the
/// layout (a rename, a new row), and removing it takes its placement with it while another
/// subject's placement stays.
///
/// Catches: a device lost by an edit of the layout that does not touch it, a placement left for a
/// device that is gone, or a switch that may sit on a server's U.
#[test]
fn a_device_is_placed_as_an_inventory_entry_and_goes_with_its_placement() {
    let layout = rack_layout();
    let (layout, switch) = layout.with_device("", device("sw")).expect("added");
    let subject = Subject {
        kind: Kind::Inventory,
        id: switch.id.clone(),
    };
    let place = |at: u32, subject: &Subject| {
        crate::operator_layout::placement_of(subject.clone(), "rack-1", at, 1)
    };
    let (layout, _) = layout.with_placement(place(24, &subject)).expect("placed");
    let server = Subject {
        kind: Kind::Inventory,
        id: "node-1".into(),
    };
    assert!(matches!(
        layout.with_placement(place(24, &server)),
        Err(LayoutError::Overlap { .. })
    ));
    let (layout, _) = layout
        .with_placement(place(1, &server))
        .expect("a server placed");

    let renamed = layout.with_name("Lab").expect("named");
    let (renamed, _) = renamed.with_next_row().expect("a row");
    assert_eq!(renamed.devices().len(), 1);
    assert!(renamed.placement(&subject).is_some());

    let removed = renamed.without_device(&switch.id).expect("removed");
    assert!(removed.devices().is_empty());
    assert!(removed.placement(&subject).is_none());
    assert!(removed.placement(&server).is_some());
}

/// Proves: the layout file holds the devices under `devices`, reads them back, reads a file that has
/// none (what the release before wrote) as a layout with none, writes none when there is none, and
/// refuses a file whose device breaks a rule, naming the file; and a record of `lab_devices` is
/// read as strictly.
///
/// Catches: devices that do not survive a restart, or an old file the new build cannot start on.
#[test]
fn the_file_and_the_records_hold_the_devices() {
    let (layout, _) = Layout::default()
        .with_device(
            "",
            with(|d| {
                d.console_host = "node-3".into();
                d.console_device = "/dev/ttyUSB0".into();
            }),
        )
        .expect("added");
    let path = Path::new("layout");
    let text = encode_file(&layout).expect("encodes");
    let json: serde_json::Value = serde_json::from_slice(&text).expect("json");
    assert_eq!(
        json["devices"]["net.1"],
        serde_json::json!({
            "name": "sw", "role": "data",
            "console_host": "node-3", "console_device": "/dev/ttyUSB0"
        })
    );
    assert_eq!(parse_file(path, &text).expect("reads"), layout);

    let old = br#"{"name": "Lab", "rows": {"row-1": {"number": 1}}}"#;
    assert!(
        parse_file(path, old)
            .expect("an old file")
            .devices()
            .is_empty()
    );
    let plain = encode_file(&Layout::default()).expect("encodes");
    assert!(!String::from_utf8_lossy(&plain).contains("devices"));

    let broken = br#"{"devices": {"net.1": {"name": "", "role": "bmc"}}}"#;
    let refused = parse_file(path, broken).expect_err("a bad device");
    assert!(refused.to_string().starts_with("layout: "), "{refused}");
    let unknown_role = br#"{"devices": {"net.1": {"name": "x", "role": "optical"}}}"#;
    assert!(parse_file(path, unknown_role).is_err());

    let record = serde_json::json!({"name": "sw", "role": "bmc"});
    let read = Layout::of_records(
        Path::new("database:lab"),
        String::new(),
        vec![],
        vec![],
        vec![],
        vec![("net.1".to_owned(), record)],
    )
    .expect("reads");
    assert_eq!(read.devices().len(), 1);
    let bad = Layout::of_records(
        Path::new("database:lab"),
        String::new(),
        vec![],
        vec![],
        vec![],
        vec![("net.1".to_owned(), serde_json::json!({"name": 5}))],
    )
    .expect_err("not a device");
    assert!(bad.to_string().contains("the record net.1"), "{bad}");
}
