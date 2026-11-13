//! The Inventory page: one table of four fields per server (Server name, BMC MAC, Data MAC,
//! Notes), an add row, and Edit and Remove on each row; what it sends (MACs normalised, nothing
//! of a record's other fields) and what it says of a field that is wrong.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// Two records that hold more than the page shows: a role, a host group, a serial, a platform, a
/// window, who added it, and a use.
fn records_reply() -> Value {
    json!({"records": [
        {"hostId": "db-1", "role": "storage", "hostGroup": "storage-group",
         "dataMacs": ["0c:c4:7a:00:00:02", "0c:c4:7a:00:00:03"], "bmcMac": "0c:c4:7a:00:00:01",
         "serial": "SN-HIDDEN", "notes": "rack 4\nslot 2", "addedBy": "alice-token",
         "addedUnix": "1800000000", "enrolled": true, "hostHeld": "HOST_HELD_APPROVED",
         "approveUntilUnix": "1800086400", "state": "RECORD_STATE_USED",
         "usedByRequestId": "req-hidden", "platform": "PLATFORM_LINUX_ARM64"},
        {"hostId": "mac-1", "serial": "C02X1"},
        {"hostId": "web-1", "dataMacs": ["0c:c4:7a:00:00:31"]},
     ],
     "nextPageToken": "mac-1"})
}

#[test]
fn inventory_page_shows_exactly_four_fields_in_one_table() {
    // Proves: a record a host is enrolled against says "enrolled" beside its name; one table with the headings Server name, BMC MAC, Data MAC and Notes (and an empty
    // one for the buttons), a row per record with those four fields and Edit and Remove, a last
    // row to add a server with an input for each of the four fields, and nothing of a record's
    // other fields: not its role, host group, serial, platform, window, state, who added it, nor
    // whether it is enrolled.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
return {
  tables: nodes(tree).filter((n) => n.tag === "table").length,
  rows: rows(tree),
  inputs: nodes(tree).filter((n) => n.tag === "input" || n.tag === "textarea").map((n) => n.attrs["name"]),
  buttons: nodes(tree).filter((n) => n.tag === "button").map((n) => n.attrs["name"]),
};
"#,
    ));
    assert_eq!(out["out"]["tables"], json!(1));
    assert_eq!(
        out["out"]["rows"],
        json!([
            ["Server name", "BMC MAC", "Data MAC", "Notes", ""],
            [
                "db-1 enrolled",
                "0c:c4:7a:00:00:01",
                "0c:c4:7a:00:00:02, 0c:c4:7a:00:00:03",
                "rack 4 slot 2",
                "Edit Remove"
            ],
            ["mac-1", "-", "-", "-", "Edit Remove"],
            ["web-1", "-", "0c:c4:7a:00:00:31", "-", "Edit Remove"],
            ["", "", "", "", "Add"],
        ])
    );
    assert_eq!(
        out["out"]["inputs"],
        json!(["add-server", "add-bmcMac", "add-dataMac", "add-notes"])
    );
    assert_eq!(
        out["out"]["buttons"],
        json!([
            "edit-record",
            "remove-record",
            "edit-record",
            "remove-record",
            "edit-record",
            "remove-record",
            "add-record",
            "next-page"
        ])
    );
    let text = shown(&out);
    for hidden in [
        "storage",
        "SN-HIDDEN",
        "C02X1",
        "alice-token",
        "req-hidden",
        "arm64",
        "ARM64",
        "Enrolled",
        "Role",
        "Serial",
        "Platform",
        "Window",
        "State",
    ] {
        assert!(!text.contains(hidden), "{hidden:?} is shown in: {text}");
    }
}

#[test]
fn inventory_page_adds_a_server_with_its_macs_normalised() {
    // Proves: MACs typed in the common spellings (colons, dashes, dots, none, upper case, spaces,
    // several data MACs separated by commas) are sent in the one lower-case colon spelling, the
    // name and notes trimmed, a note's line break kept, and nothing but the four fields sent; a
    // BMC MAC may be left out; a good add refreshes the page.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    ui.reply(
        &m("Enrollment/AddRecord"),
        json!({"record": {"hostId": "db-2"}}),
    );
    ui.reply(
        &m("Enrollment/AddRecord"),
        json!({"record": {"hostId": "db-3"}}),
    );
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
type(tree, "add-server", " db-2 ");
type(tree, "add-bmcMac", " 0C-C4-7A-00-00-11 ");
type(tree, "add-dataMac", "0CC47A000012, 0cc4.7a00.0013 ,0c:c4:7a:00:00:12");
type(tree, "add-notes", " rack 5\r\nslot 1 ");
await click(tree, "add-record");
type(tree, "add-server", "db-3");
type(tree, "add-bmcMac", "");
type(tree, "add-dataMac", "0c:c4:7a:00:00:14");
type(tree, "add-notes", "");
await click(tree, "add-record");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Enrollment/ListRecords"),
                json!({"limit": 50, "pageToken": ""})
            ),
            (
                m("Enrollment/AddRecord"),
                json!({"hostId": "db-2", "bmcMac": "0c:c4:7a:00:00:11",
                       "dataMacs": ["0c:c4:7a:00:00:12", "0c:c4:7a:00:00:13"],
                       "notes": "rack 5\nslot 1"})
            ),
            (
                m("Enrollment/AddRecord"),
                json!({"hostId": "db-3", "bmcMac": "",
                       "dataMacs": ["0c:c4:7a:00:00:14"], "notes": ""})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Added db-2.", "Added db-3."])
    );
    assert_eq!(out["seen"]["refreshes"], 2);
}

#[test]
fn inventory_page_says_which_field_is_wrong_and_sends_nothing() {
    // Proves: a name off its grammar, a MAC that is not twelve hex digits (in the BMC field and in
    // the data field, naming the entry), no data MAC, a BMC MAC that is also a data MAC, more
    // than 16 data MACs and notes with a tab or past 256 bytes each put a sentence under the
    // field that is wrong, and only that one; nothing is sent; a corrected form clears the
    // sentences and sends.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    ui.reply(
        &m("Enrollment/AddRecord"),
        json!({"record": {"hostId": "db-2"}}),
    );
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
const errors = () => Object.fromEntries(["server", "bmcMac", "dataMac", "notes"]
  .map((key) => [key, text(named(tree, `add-${key}-error`)[0])]));
const fill = (name, bmc, data, notes) => {
  type(tree, "add-server", name);
  type(tree, "add-bmcMac", bmc);
  type(tree, "add-dataMac", data);
  type(tree, "add-notes", notes);
};
const seventeen = Array.from({ length: 17 }, (_, n) => `0c:c4:7a:00:01:${n.toString(16).padStart(2, "0")}`);
const seen = [];
for (const attempt of [
  ["", "", "0c:c4:7a:00:00:12", ""],
  ["DB-2", "", "0c:c4:7a:00:00:12", ""],
  ["db-2", "0c:c4:7a:00:00", "0c:c4:7a:00:00:12", ""],
  ["db-2", "", "0c:c4:7a:00:00:12, nope", ""],
  ["db-2", "", " ", ""],
  ["db-2", "0c:c4:7a:00:00:12", "0c:c4:7a:00:00:12", ""],
  ["db-2", "", seventeen.join(","), ""],
  ["db-2", "", "0c:c4:7a:00:00:12", "a\tb"],
  ["db-2", "", "0c:c4:7a:00:00:12", "n".repeat(257)],
  ["", "xx", "", "a\tb"],
]) {
  fill(...attempt);
  await click(tree, "add-record");
  seen.push(errors());
}
fill("db-2", "", "0c:c4:7a:00:00:12", "n".repeat(256));
await click(tree, "add-record");
seen.push(errors());
return seen;
"#,
    ));
    let none = "";
    let name = "A server name is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
    let mac = "is not a MAC: twelve hex digits, with or without : - . between them.";
    let notes = "Notes are at most 256 bytes, with no control character but the line break.";
    let e = |name: &str, bmc: &str, data: &str, notes: &str| json!({"server": name, "bmcMac": bmc, "dataMac": data, "notes": notes});
    assert_eq!(
        out["out"],
        json!([
            e("Name the server.", none, none, none),
            e(name, none, none, none),
            e(none, &format!("0c:c4:7a:00:00 {mac}"), none, none),
            e(none, none, &format!("nope {mac}"), none),
            e(none, none, "Give the data MAC.", none),
            e(none, "The BMC MAC is also a data MAC.", none, none),
            e(none, none, "At most 16 data MACs.", none),
            e(none, none, none, notes),
            e(none, none, none, notes),
            e(
                "Name the server.",
                &format!("xx {mac}"),
                "Give the data MAC.",
                notes
            ),
            e(none, none, none, none),
        ])
    );
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Enrollment/ListRecords"),
                json!({"limit": 50, "pageToken": ""})
            ),
            (
                m("Enrollment/AddRecord"),
                json!({"hostId": "db-2", "bmcMac": "", "dataMacs": ["0c:c4:7a:00:00:12"],
                       "notes": "n".repeat(256)})
            ),
        ]
    );
    let reports = out["seen"]["reports"].as_array().expect("reports");
    assert_eq!(reports.len(), 11);
    assert!(
        reports[..10]
            .iter()
            .all(|r| r == "Correct the marked fields.")
    );
    assert_eq!(reports[10], "Added db-2.");
}

#[test]
fn inventory_page_edits_the_macs_and_notes_and_sends_only_those() {
    // Proves: Edit opens the row of that record for editing (a navigation naming it, the page's
    // other parameters kept); the open row shows the record's MACs and notes in inputs and the
    // name as text; Save sends an UpdateRecord with the host id, the data MACs, the BMC MAC and the
    // notes and none of the record's other fields (its role, host group, serial, platform, window
    // and use stay on the coordinator), the MACs normalised and a note's line break kept; a bad
    // MAC says so under its field and sends nothing; a record that keeps a serial may save with
    // no data MAC while one without may not; Cancel leaves the edit.
    let mut ui = Ui::start();
    for _ in 0..4 {
        ui.reply(&m("Enrollment/ListRecords"), records_reply());
    }
    ui.reply(
        &m("Enrollment/UpdateRecord"),
        json!({"record": {"hostId": "db-1"}}),
    );
    ui.reply(
        &m("Enrollment/UpdateRecord"),
        json!({"record": {"hostId": "mac-1"}}),
    );
    let first = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"await click(tree, "edit-record", 1);"#,
    ));
    assert_eq!(
        first["seen"]["navigations"],
        json!([{"name": "inventory", "params": {"edit": "mac-1"}}])
    );
    let out = ui.run(&page_driver(
        "inventory",
        r#"{ edit: "db-1", pageToken: "db-0" }"#,
        "[]",
        r#"
const shownRows = rows(tree);
const values = ["bmcMac", "dataMac", "notes"].map((key) => named(tree, `edit-${key}`)[0].attrs.value);
type(tree, "edit-bmcMac", "0C-C4-7A-00-00-21");
type(tree, "edit-dataMac", "0CC47A000022");
type(tree, "edit-notes", "moved\r\nto rack 9");
await click(tree, "save-record");
type(tree, "edit-bmcMac", "bad");
await click(tree, "save-record");
const error = text(named(tree, "edit-bmcMac-error")[0]);
await click(tree, "cancel-edit");
return { shownRows, values, error };
"#,
    ));
    assert_eq!(
        out["out"]["values"],
        json!([
            "0c:c4:7a:00:00:01",
            "0c:c4:7a:00:00:02, 0c:c4:7a:00:00:03",
            "rack 4\nslot 2"
        ])
    );
    assert_eq!(out["out"]["shownRows"][1][0], json!("db-1"));
    assert_eq!(
        out["out"]["error"],
        json!("bad is not a MAC: twelve hex digits, with or without : - . between them.")
    );
    assert_eq!(
        bodies(&ui)[2],
        (
            m("Enrollment/UpdateRecord"),
            json!({"hostId": "db-1", "dataMacs": ["0c:c4:7a:00:00:22"],
                   "bmcMac": "0c:c4:7a:00:00:21", "notes": "moved\nto rack 9"})
        )
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "inventory", "params": {"pageToken": "db-0"}},
            {"name": "inventory", "params": {"pageToken": "db-0"}}
        ])
    );
    let reports = out["seen"]["reports"].as_array().expect("reports");
    assert_eq!(reports[0], "Saved db-1.");
    assert_eq!(reports[1], "Correct the marked fields.");

    let serial = ui.run(&page_driver(
        "inventory",
        r#"{ edit: "mac-1" }"#,
        "[]",
        r#"
type(tree, "edit-notes", "a Mac");
await click(tree, "save-record");
"#,
    ));
    assert_eq!(serial["seen"]["reports"], json!(["Saved mac-1."]));
    assert_eq!(
        bodies(&ui)[4],
        (
            m("Enrollment/UpdateRecord"),
            json!({"hostId": "mac-1", "dataMacs": [], "bmcMac": "", "notes": "a Mac"})
        )
    );
    let plain = ui.run(&page_driver(
        "inventory",
        r#"{ edit: "web-1" }"#,
        "[]",
        r#"
type(tree, "edit-dataMac", " ");
await click(tree, "save-record");
return text(named(tree, "edit-dataMac-error")[0]);
"#,
    ));
    assert_eq!(plain["out"], json!("Give the data MAC."));
    assert_eq!(bodies(&ui).len(), 6, "a refused save sends nothing");
}

#[test]
fn inventory_page_removes_a_record_after_a_confirm() {
    // Proves: Remove asks first and sends nothing on a no; on a yes it removes the record of that
    // row (and no other) with no reason typed; a refusal is reported by its code.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    ui.reply(
        &m("Enrollment/RemoveRecord"),
        json!({"entry": {"seq": "4", "action": "record_remove"}}),
    );
    ui.refuse(&m("Enrollment/RemoveRecord"), 404, "NOT_FOUND", "no_record");
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[false, true, true]",
        r#"
await click(tree, "remove-record", 1);
await click(tree, "remove-record", 1);
await click(tree, "remove-record", 0);
"#,
    ));
    assert_eq!(
        bodies(&ui)[1..],
        [
            (
                m("Enrollment/RemoveRecord"),
                json!({"hostId": "mac-1", "reason": "removed on the Inventory page"})
            ),
            (
                m("Enrollment/RemoveRecord"),
                json!({"hostId": "db-1", "reason": "removed on the Inventory page"})
            ),
        ]
    );
    assert_eq!(out["seen"]["questions"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        out["seen"]["reports"],
        json!(["Removed mac-1.", "NOT_FOUND no_record"])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn inventory_page_pages_marks_a_linked_record_and_says_when_it_cannot_load() {
    // Proves: the next page goes on from the token the list gave, the first page drops it, the
    // record the Host page links to (`record`) is the row marked, an empty inventory still shows
    // the add row, and a refused list shows the reason.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    let out = ui.run(&page_driver(
        "inventory",
        r#"{ record: "mac-1", pageToken: "x" }"#,
        "[]",
        r#"
await click(tree, "next-page");
await click(tree, "first-page");
return nodes(tree).filter((n) => n.tag === "tr" && n.attrs["aria-current"] === "true")
  .map((n) => text(n.children[0]));
"#,
    ));
    assert_eq!(out["out"], json!(["mac-1"]));
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "inventory", "params": {"pageToken": "mac-1"}},
            {"name": "inventory", "params": {}}
        ])
    );

    let mut empty = Ui::start();
    empty.reply(&m("Enrollment/ListRecords"), json!({}));
    let out = empty.run(&page_driver("inventory", "{}", "[]", ""));
    assert_shows(&out, &["Inventory", "Server name", "Add"]);
    let mut refused = Ui::start();
    refused.refuse(
        &m("Enrollment/ListRecords"),
        500,
        "INTERNAL",
        "store_failed",
    );
    let out = refused.run(&page_driver("inventory", "{}", "[]", ""));
    assert_shows(&out, &["Inventory", "store_failed"]);
}

#[test]
fn inventory_page_names_the_host_enrolled_against_a_server() {
    // Proves: a row says "enrolled" when the host has the server's name or the coordinator does not
    // say which host, "enrolled as <host id>" when it is another, and nothing for a server no host is
    // enrolled against.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/ListRecords"),
        json!({"records": [
            {"hostId": "a-1", "dataMacs": ["0c:c4:7a:00:00:02"], "enrolled": true, "enrolledHostId": "a-1"},
            {"hostId": "b-1", "dataMacs": ["0c:c4:7a:00:00:03"], "enrolled": true, "enrolledHostId": "web-9"},
            {"hostId": "c-1", "dataMacs": ["0c:c4:7a:00:00:04"], "enrolled": true},
            {"hostId": "d-1", "dataMacs": ["0c:c4:7a:00:00:05"]},
        ]}),
    );
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"return rows(tree).slice(1, 5).map((row) => row[0]);"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            "a-1 enrolled",
            "b-1 enrolled as web-9",
            "c-1 enrolled",
            "d-1"
        ])
    );
}

#[test]
fn the_data_mac_is_one_input_in_the_add_row_and_in_an_edited_row() {
    // Proves: a server has one data MAC field: the add row and an edited row each hold exactly one
    // input named for it and no button to add another; the edited row shows the stored MAC in it.
    // Catches: a second data MAC input or an "add port" button creeping in before the API has
    // several ports.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    let out = ui.run(&page_driver(
        "inventory",
        r#"{ edit: "web-1" }"#,
        "[]",
        r#"
const inputs = nodes(tree).filter((n) => n.tag === "input" || n.tag === "textarea");
return {
  data: inputs.filter((n) => /dataMac/i.test(n.attrs["name"])).map((n) => [n.attrs["name"], n.attrs.value || ""]),
  buttons: nodes(tree).filter((n) => n.tag === "button").map((n) => n.attrs["name"]).filter((n) => /mac|port/i.test(n)),
};
"#,
    ));
    assert_eq!(
        out["out"]["data"],
        json!([["edit-dataMac", "0c:c4:7a:00:00:31"], ["add-dataMac", ""]])
    );
    assert_eq!(out["out"]["buttons"], json!([]));
}

#[test]
fn remove_on_an_inventory_row_is_the_danger_button_and_edit_is_not() {
    // Proves: the Remove button of each record is of class `danger`; Edit carries none and Add is primary.
    // Catches: a Remove that looks like Edit, one click from a mistake.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
const classes = (name) => named(tree, name).map((n) => n.attrs.class || "");
return { remove: classes("remove-record"), edit: classes("edit-record"), add: classes("add-record") };
"#,
    ));
    assert_eq!(
        out["out"],
        json!({"remove": ["danger", "danger", "danger"], "edit": ["", "", ""], "add": ["primary"]})
    );
}
