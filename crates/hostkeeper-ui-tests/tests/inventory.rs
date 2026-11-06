//! The host inventory in the app: the Inventory page's calls (`Enrollment.ListRecords`,
//! `AddRecord`, `RemoveRecord`) and what it checks before sending, its Enrolled column and
//! filter, the Host page's link to a host's record, and the Enrollment page's match line and
//! the host id and role a match fills in.

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

/// Two records, `db-1` enrolled and `db-2` not, and a next page after `db-2`.
fn enrolled_reply() -> Value {
    json!({"records": [
        {"hostId": "db-1", "dataMacs": ["0c:c4:7a:00:00:02"], "enrolled": true},
        {"hostId": "db-2", "serial": "SN-2"},
     ],
     "nextPageToken": "db-2"})
}

#[test]
fn inventory_page_shows_and_filters_by_enrolled() {
    // Catches: the Enrolled column missing or reading an absent `enrolled` (false, which the
    // coordinator's JSON leaves out) as anything but "no"; the filter not sent as typed (an
    // absent filter sent as false, or false dropped), a parameter off its two values sent at all,
    // the Show button navigating without the choice, paging that drops the filter, and the
    // record the Host page links to left unmarked.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), enrolled_reply());
    let out = ui.run(&page_driver(
        "inventory",
        r#"{ enrolled: "false", record: "db-2" }"#,
        "[]",
        r#"
choose(tree, "enrolled-filter", "true");
await click(tree, "show");
choose(tree, "enrolled-filter", "");
await click(tree, "show");
await click(tree, "next-page");
const marked = nodes(tree).filter((n) => n.tag === "tr" && n.attrs["aria-current"] === "true");
return { rows: rows(tree), marked: marked.map((row) => text(row.children[0])) };
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [(
            m("Enrollment/ListRecords"),
            json!({"limit": 50, "pageToken": "", "enrolled": false})
        )]
    );
    assert_eq!(
        out["out"]["rows"],
        json!([
            [
                "Host id",
                "Role",
                "Data MACs",
                "BMC MAC",
                "Serial",
                "Notes",
                "Added by",
                "Added at",
                "Enrolled",
                ""
            ],
            [
                "db-1",
                "-",
                "0c:c4:7a:00:00:02",
                "-",
                "-",
                "-",
                "-",
                "-",
                "yes",
                "Remove"
            ],
            ["db-2", "-", "-", "-", "SN-2", "-", "-", "-", "no", "Remove"],
        ])
    );
    assert_eq!(out["out"]["marked"], json!(["db-2"]));
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "inventory", "params": {"enrolled": "true"}},
            {"name": "inventory", "params": {}},
            {"name": "inventory", "params": {"pageToken": "db-2", "enrolled": "false"}},
        ])
    );

    for (params, sent) in [
        (
            r#"{ enrolled: "true", pageToken: "db-0" }"#,
            json!({"limit": 50, "pageToken": "db-0", "enrolled": true}),
        ),
        (
            r#"{ enrolled: "yes" }"#,
            json!({"limit": 50, "pageToken": ""}),
        ),
        ("{}", json!({"limit": 50, "pageToken": ""})),
    ] {
        let mut ui = Ui::start();
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
        let out = ui.run(&page_driver(
            "inventory",
            params,
            "[]",
            r#"await click(tree, "first-page").catch(() => {}); return null;"#,
        ));
        assert_eq!(
            bodies(&ui),
            [(m("Enrollment/ListRecords"), sent.clone())],
            "{params}"
        );
        let empty = match sent.get("enrolled").and_then(Value::as_bool) {
            Some(true) => "No enrolled server is recorded.",
            Some(false) => "No not enrolled server is recorded.",
            None => "No server is recorded.",
        };
        assert_shows(&out, &[empty]);
        let first = if sent["pageToken"] == "db-0" {
            json!([{"name": "inventory", "params": {"enrolled": "true"}}])
        } else {
            json!([])
        };
        assert_eq!(out["seen"]["navigations"], first, "{params}");
    }
}

#[test]
fn host_page_links_its_record_to_the_inventory_page() {
    // Catches: the record id not offered as a link, a link that opens the Inventory page on
    // another record or without the enrolled filter, and a host with no record shown with an
    // empty link instead of "none matched" and "not enrolled".
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "db-1", "role": "storage", "inventoryRecordId": "db-1",
                        "dataMacs": ["0c:c4:7a:00:00:02"], "enrolledUnix": "1800000000"}}),
    );
    ui.reply(&m("Services/List"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "db-1" }"#,
        "[]",
        r#"await click(tree, "open-record"); return null;"#,
    ));
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "inventory", "params": {"enrolled": "true", "record": "db-1"}}])
    );

    let mut bare = Ui::start();
    bare.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-9", "enrolledUnix": "0"}}),
    );
    bare.reply(&m("Services/List"), json!({}));
    let out = bare.run(&page_driver(
        "host",
        r#"{ nodeId: "web-9" }"#,
        "[]",
        r#"return { record: cell(tree, "Inventory record"), enrolled: cell(tree, "Enrolled"),
                   links: nodes(tree).filter((n) => n.attrs["name"] === "open-record").length };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"record": "none matched", "enrolled": "not enrolled", "links": 0})
    );
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

#[test]
fn enrollment_page_approves_a_matched_request_as_its_record_unless_replace() {
    // Catches: a matched request's host id not prefilled from its record, an empty host id for it
    // refused in the page (or sent as anything but empty), the host id the coordinator answers
    // not reported, a host id other than the record's sent without Replace, Replace not letting
    // it through, and a renewal or an unmatched request held to a record.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [
           {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
            "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1", "recordRole": "storage"},
           {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32), "renewalOf": "s6",
            "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-2", "recordRole": "storage"},
           {"requestId": "3d".repeat(32), "keySha256": "ef".repeat(32),
            "inventoryMatch": "INVENTORY_MATCH_UNMATCHED"},
           {"requestId": "4c".repeat(32), "keySha256": "12".repeat(32),
            "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-4"},
        ]}),
    );
    for host in ["db-1", "db-9", "s6", "web-1"] {
        ui.reply(
            &m("Enrollment/Approve"),
            json!({"hostId": host, "certificateIssuedAtPoll": true}),
        );
    }
    // An answer without a host id: the page reports the record's.
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"certificateIssuedAtPoll": true}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[true, true]",
        r#"
const filled = [0, 1, 2, 3].map((i) => named(tree, "host-id")[i].attrs.value);
for (const i of [0, 1, 2, 3]) {
  type(tree, "role", "storage", i);
  type(tree, "reason", "built today", i);
}
type(tree, "host-id", "", 0);
await click(tree, "approve", 0);
type(tree, "host-id", "db-9", 0);
await click(tree, "approve", 0);
tick(tree, "replace", true, 0);
await click(tree, "approve", 0);
await click(tree, "approve", 1);
await click(tree, "approve", 2);
type(tree, "host-id", "web-1", 2);
await click(tree, "approve", 2);
type(tree, "host-id", "", 3);
tick(tree, "replace", true, 3);
await click(tree, "approve", 3);
return filled;
"#,
    ));
    assert_eq!(out["out"], json!(["db-1", "s6", "", "db-4"]));
    let body = |id: &str, key: &str, host: &str, replace: bool| {
        json!({"requestId": id.repeat(32), "keySha256": key.repeat(32), "hostId": host,
               "role": "storage", "services": [], "replace": replace, "reason": "built today"})
    };
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (m("Enrollment/Approve"), body("1f", "ab", "", false)),
            (m("Enrollment/Approve"), body("1f", "ab", "db-9", true)),
            (m("Enrollment/Approve"), body("2e", "cd", "s6", false)),
            (m("Enrollment/Approve"), body("3d", "ef", "web-1", false)),
            (m("Enrollment/Approve"), body("4c", "12", "", true)),
        ]
    );
    let seen = &out["seen"];
    assert_eq!(
        seen["questions"],
        json!([
            format!(
                "Approve request {} as db-9 and revoke the certificate of any host that holds db-9?",
                "1f".repeat(32)
            ),
            format!(
                "Approve request {} as db-4 and revoke the certificate of any host that holds db-4?",
                "4c".repeat(32)
            ),
        ])
    );
    assert_eq!(
        seen["reports"],
        json!([
            "Approved db-1: the host gets its certificate at its next poll.",
            "This request matches the record of db-1: approve it as db-1, or tick Replace to approve it as db-9.",
            "Approved db-9: the host gets its certificate at its next poll.",
            "Approved s6: the host gets its certificate at its next poll.",
            "Name the host id.",
            "Approved web-1: the host gets its certificate at its next poll.",
            "Approved db-4: the host gets its certificate at its next poll.",
        ])
    );
}
