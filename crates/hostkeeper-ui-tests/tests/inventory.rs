//! The host inventory in the app: the Inventory page's calls (`Enrollment.ListRecords`,
//! `AddRecord`, `RemoveRecord`) and what it checks before sending, and the Enrollment page's
//! match line and the host id and role a match fills in.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

fn records_reply() -> Value {
    json!({"records": [
        {"hostId": "db-1", "role": "storage", "dataMacs": ["0c:c4:7a:00:00:02", "0c:c4:7a:00:00:03"],
         "bmcMac": "0c:c4:7a:00:00:01", "serial": "SN-1", "notes": "rack 4", "addedBy": "alice",
         "addedUnix": "1800000000"},
        {"hostId": "mac-1", "serial": "C02X1"},
     ],
     "nextPageToken": "mac-1"})
}

#[test]
fn inventory_page_lists_records_and_records_a_server() {
    // Catches: a record's field left off its row, an add sent with an input off its grammar
    // (host id, role, a data MAC, the BMC MAC, the serial, the notes) or with neither a data MAC
    // nor a serial, more data MACs than a record holds, a refusal taken as success, and paging
    // that does not go on from the token the list gave.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    ui.refuse(
        &m("Enrollment/AddRecord"),
        409,
        "ALREADY_EXISTS",
        "record_conflict",
    );
    ui.reply(
        &m("Enrollment/AddRecord"),
        json!({"record": {"hostId": "db-2"}, "entry": {"seq": "3", "action": "record_add"}}),
    );
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
const fill = (hostId, role, dataMacs, bmcMac, serial, notes) => {
  type(tree, "record-host-id", hostId);
  type(tree, "record-role", role);
  type(tree, "record-data-macs", dataMacs);
  type(tree, "record-bmc-mac", bmcMac);
  type(tree, "record-serial", serial);
  type(tree, "record-notes", notes);
};
const seventeen = Array.from({ length: 17 }, (_, n) => `0c:c4:7a:00:01:${n.toString(16).padStart(2, "0")}`);
const attempts = [
  ["", "", "0c:c4:7a:00:00:12", "", "", ""],
  ["DB-2", "", "0c:c4:7a:00:00:12", "", "", ""],
  ["db-2", "7db", "0c:c4:7a:00:00:12", "", "", ""],
  ["db-2", "db-", "0c:c4:7a:00:00:12", "", "", ""],
  ["db-2", "d--b", "0c:c4:7a:00:00:12", "", "", ""],
  ["db-2", "", "0c:c4:7a:00:00", "", "", ""],
  ["db-2", "", "0c:c4:7a:00:00:12", "0c:c4:7a-00-00-11", "", ""],
  ["db-2", "", seventeen.join(","), "", "", ""],
  ["db-2", "", "0c:c4:7a:00:00:12", "", "SN 2", ""],
  ["db-2", "", "", "0c:c4:7a:00:00:11", "", ""],
  ["db-2", "", "0c:c4:7a:00:00:12", "", "", "line\nbreak"],
];
for (const attempt of attempts) {
  fill(...attempt);
  await click(tree, "add-record");
}
fill(" db-2 ", " storage ", "0C-C4-7A-00-00-12, 0cc47a000013", " 0C:C4:7A:00:00:11 ", " SN-2 ", " rack 5 ");
await click(tree, "add-record");
await click(tree, "add-record");
await click(tree, "next-page");
"#,
    ));
    let sent = json!({"hostId": "db-2", "role": "storage",
        "dataMacs": ["0C-C4-7A-00-00-12", "0cc47a000013"], "bmcMac": "0C:C4:7A:00:00:11",
        "serial": "SN-2", "notes": "rack 5"});
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Enrollment/ListRecords"),
                json!({"limit": 50, "pageToken": ""})
            ),
            (m("Enrollment/AddRecord"), sent.clone()),
            (m("Enrollment/AddRecord"), sent),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Name the host id.",
            "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.",
            "A role is 1 to 32 of a-z, 0-9 and -, a letter first, not ending with - and with no --.",
            "A role is 1 to 32 of a-z, 0-9 and -, a letter first, not ending with - and with no --.",
            "A role is 1 to 32 of a-z, 0-9 and -, a letter first, not ending with - and with no --.",
            "0c:c4:7a:00:00 is not a MAC: six pairs of hex digits separated by : or by -.",
            "0c:c4:7a-00-00-11 is not a MAC: six pairs of hex digits separated by : or by -.",
            "A record holds at most 16 data MACs.",
            "A serial is 1 to 64 of A-Z, a-z, 0-9, ., _ and -.",
            "Give a data MAC, or a serial for a Mac.",
            "A note has no control character (a tab or a line break, for example).",
            "ALREADY_EXISTS record_conflict",
            "Recorded db-2.",
        ])
    );
    assert_eq!(
        out["seen"]["refreshes"], 1,
        "a refused add refreshes nothing"
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "inventory", "params": {"pageToken": "mac-1"}}])
    );
    assert_shows(
        &out,
        &[
            "db-1",
            "storage",
            "0c:c4:7a:00:00:02, 0c:c4:7a:00:00:03",
            "0c:c4:7a:00:00:01",
            "SN-1",
            "rack 4",
            "alice",
            "2027-01-15 08:00:00 UTC",
            "mac-1",
            "C02X1",
        ],
    );
}

#[test]
fn inventory_page_removes_a_record_only_with_a_reason_and_pages_back() {
    // Catches: a remove sent without a reason or with one off its rule, a remove of another
    // record than the row's, an empty list shown as a table, and a first page that keeps the
    // token.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), records_reply());
    ui.reply(
        &m("Enrollment/RemoveRecord"),
        json!({"entry": {"seq": "4", "action": "record_remove"}}),
    );
    let out = ui.run(&page_driver(
        "inventory",
        r#"{ pageToken: "db-0" }"#,
        "[]",
        r#"
await click(tree, "remove-record", 1);
type(tree, "remove-reason", "x".repeat(257));
await click(tree, "remove-record", 1);
type(tree, "remove-reason", " returned to vendor ");
await click(tree, "remove-record", 1);
await click(tree, "first-page");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Enrollment/ListRecords"),
                json!({"limit": 50, "pageToken": "db-0"})
            ),
            (
                m("Enrollment/RemoveRecord"),
                json!({"hostId": "mac-1", "reason": "returned to vendor"})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason for the removal first.",
            "A reason is at most 256 bytes; this one is 257.",
            "Removed the record of mac-1.",
        ])
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "inventory", "params": {}}])
    );

    let mut empty = Ui::start();
    empty.reply(&m("Enrollment/ListRecords"), json!({}));
    let out = empty.run(&page_driver("inventory", "{}", "[]", ""));
    assert_shows(&out, &["No server is recorded.", "Record a server"]);
    assert!(!shown(&out).contains("Reason for a removal"));
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

fn matched_reply() -> Value {
    json!({"requests": [
       {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
        "claims": {"addresses": ["0c:c4:7a:00:00:02"], "bmcMac": "0c:c4:7a:00:00:01"},
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1", "recordRole": "storage"},
       {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32),
        "inventoryMatch": "INVENTORY_MATCH_AMBIGUOUS", "candidateHostIds": ["db-1", "db-2"]},
       {"requestId": "3d".repeat(32), "keySha256": "ef".repeat(32),
        "inventoryMatch": "INVENTORY_MATCH_UNMATCHED"},
       {"requestId": "4c".repeat(32), "keySha256": "12".repeat(32)},
       {"requestId": "5b".repeat(32), "keySha256": "34".repeat(32),
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "mac-1"},
    ]})
}

#[test]
fn enrollment_page_shows_each_match_and_fills_in_only_the_one_record() {
    // Catches: a match left off the request, a matched record's host id or role not filled in
    // (or filled in from another record), an ambiguous match that fills in one of its
    // candidates, and an approval that sends anything but the listed key hash.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), matched_reply());
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"hostId": "db-1", "certificateIssuedAtPoll": true}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
const filled = [0, 1, 2, 3, 4].map((i) => [named(tree, "host-id")[i].attrs.value, named(tree, "role")[i].attrs.value]);
type(tree, "reason", "built today", 0);
await click(tree, "approve", 0);
type(tree, "reason", "built today", 1);
await click(tree, "approve", 1);
return filled;
"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            ["db-1", "storage"],
            ["", ""],
            ["", ""],
            ["", ""],
            ["mac-1", ""]
        ])
    );
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (
                m("Enrollment/Approve"),
                json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
                       "hostId": "db-1", "role": "storage", "services": [], "replace": false,
                       "reason": "built today"})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Approved db-1: the host gets its certificate at its next poll.",
            "Name the host id.",
        ])
    );
    assert_shows(
        &out,
        &[
            "Matches the record of db-1 (role storage).",
            "Matches several records: db-1, db-2. None is chosen.",
            "Matches no inventory record.",
            "Not matched against the inventory.",
            "Matches the record of mac-1 (no role).",
            "BMC MAC",
            "0c:c4:7a:00:00:01",
        ],
    );
}
