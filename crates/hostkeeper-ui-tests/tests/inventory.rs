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
                "Window ends",
                "State",
                "Used by",
                "Used at",
                "",
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
                "-",
                "-",
                "-",
                "-",
                "",
                "Remove"
            ],
            [
                "db-2", "-", "-", "-", "SN-2", "-", "-", "-", "no", "-", "-", "-", "-", "",
                "Remove"
            ],
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
    // empty link instead of "none matched" and "not enrolled"; and the first heartbeat of an
    // enrolled host not heard since its approval shown as "-" instead of "not yet", or that of a
    // host not enrolled as "not yet" instead of "-".
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
        r#"await click(tree, "open-record"); return cell(tree, "First heartbeat");"#,
    ));
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "inventory", "params": {"enrolled": "true", "record": "db-1"}}])
    );
    assert_eq!(out["out"], json!("not yet"));

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
                   first: cell(tree, "First heartbeat"),
                   links: nodes(tree).filter((n) => n.attrs["name"] === "open-record").length };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"record": "none matched", "enrolled": "not enrolled", "first": "-", "links": 0})
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

/// Four records, one in each state a record shows, and one closed without a role.
fn windows_reply() -> Value {
    json!({"records": [
        {"hostId": "open-1", "role": "storage", "dataMacs": ["0c:c4:7a:00:00:02"],
         "approveUntilUnix": "1800003600", "state": "RECORD_STATE_OPEN"},
        {"hostId": "closed-1", "role": "storage", "dataMacs": ["0c:c4:7a:00:00:03"],
         "approveUntilUnix": "1799990000", "state": "RECORD_STATE_CLOSED"},
        {"hostId": "used-1", "role": "storage", "dataMacs": ["0c:c4:7a:00:00:04"],
         "approveUntilUnix": "1800003600", "state": "RECORD_STATE_USED",
         "usedByRequestId": "ab".repeat(32), "usedUnix": "1800000100"},
        {"hostId": "bare-1", "dataMacs": ["0c:c4:7a:00:00:05"], "state": "RECORD_STATE_CLOSED"},
    ]})
}

#[test]
fn inventory_page_shows_each_records_window_and_state() {
    // Proves: a row shows the end of its window, its state in a word (open, closed or used), and
    // for a used record the request and the time; a record with no state or window reads "-";
    // only a closed record that names a role has an Open window button.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), windows_reply());
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
return { rows: rows(tree).slice(1).map((r) => r.slice(0, 1).concat(r.slice(9))),
         opens: named(tree, "open-record").length };
"#,
    ));
    let used = "ab".repeat(32);
    assert_eq!(
        out["out"]["rows"],
        json!([
            [
                "open-1",
                "2027-01-15 09:00:00 UTC",
                "open",
                "-",
                "-",
                "",
                "Remove"
            ],
            [
                "closed-1",
                "2027-01-15 05:13:20 UTC",
                "closed",
                "-",
                "-",
                "Open window",
                "Remove"
            ],
            [
                "used-1",
                "2027-01-15 09:00:00 UTC",
                "used",
                used,
                "2027-01-15 08:01:40 UTC",
                "",
                "Remove"
            ],
            ["bare-1", "-", "closed", "-", "-", "", "Remove"],
        ])
    );
    assert_eq!(out["out"]["opens"], 1);
}

#[test]
fn inventory_page_opens_a_closed_records_window() {
    // Proves: Open window sends OpenRecord for its own row's host id with the window typed
    // (hours as approveForSecs, an end time in UTC as approveUntilUnix, none as neither) and the
    // trimmed reason, refuses a window that is neither and a reason off its rule before sending,
    // and says until when the record is open.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), windows_reply());
    ui.reply(
        &m("Enrollment/OpenRecord"),
        json!({"record": {"hostId": "closed-1", "approveUntilUnix": "1800007200"}}),
    );
    ui.reply(
        &m("Enrollment/OpenRecord"),
        json!({"record": {"hostId": "closed-1", "approveUntilUnix": "1800007200"}}),
    );
    ui.reply(&m("Enrollment/OpenRecord"), json!({}));
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
for (const [window, reason] of [["soon", ""], ["0", ""], ["2", "x".repeat(257)], ["2", " rebuilt "],
    ["2027-01-15 10:00", ""], ["", ""]]) {
  type(tree, "open-window", window);
  type(tree, "open-reason", reason);
  await click(tree, "open-record");
}
"#,
    ));
    assert_eq!(
        bodies(&ui)[1..],
        [
            (
                m("Enrollment/OpenRecord"),
                json!({"hostId": "closed-1", "reason": "rebuilt", "approveForSecs": "7200"})
            ),
            (
                m("Enrollment/OpenRecord"),
                json!({"hostId": "closed-1", "reason": "", "approveUntilUnix": "1800007200"})
            ),
            (
                m("Enrollment/OpenRecord"),
                json!({"hostId": "closed-1", "reason": ""})
            ),
        ]
    );
    let problem = "A window is a number of hours (2 or 1.5), or an end time in UTC (2027-01-15 08:00 or 2027-01-15T08:00:00Z).";
    assert_eq!(
        out["seen"]["reports"],
        json!([
            problem,
            problem,
            "A reason is at most 256 bytes; this one is 257.",
            "Opened the record of closed-1 until 2027-01-15 10:00:00 UTC.",
            "Opened the record of closed-1 until 2027-01-15 10:00:00 UTC.",
            "Opened the record of closed-1 until -.",
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 3);
}

#[test]
fn inventory_page_sends_the_window_of_a_new_record() {
    // Proves: the Record form's window is sent as approveForSecs for hours (fractions
    // rounded to the second), as approveUntilUnix for an end time in UTC (also in the spelling
    // the page shows times in), and not at all when empty; an input that is neither, an end time
    // that is not a date or a time of day (a minute or second past 59), and a window for a record with no role are refused before sending.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    for _ in 0..4 {
        ui.reply(
            &m("Enrollment/AddRecord"),
            json!({"record": {"hostId": "db-2"}}),
        );
    }
    let out = ui.run(&page_driver(
        "inventory",
        "{}",
        "[]",
        r#"
for (const [role, window] of [["storage", "1.5"], ["storage", "2027-01-15T08:00:00Z"],
    ["storage", "2027-01-15 08:00:30 UTC"], ["storage", ""], ["storage", "tomorrow"],
    ["storage", "2027-02-30 08:00"], ["storage", "2027-01-15 08:60"],
    ["storage", "2027-01-15 08:00:60"], ["storage", "0"], ["", "4"]]) {
  type(tree, "record-host-id", "db-2");
  type(tree, "record-role", role);
  type(tree, "record-data-macs", "0c:c4:7a:00:00:12");
  type(tree, "record-window", window);
  await click(tree, "add-record");
}
"#,
    ));
    let base = json!({"hostId": "db-2", "role": "storage", "dataMacs": ["0c:c4:7a:00:00:12"],
        "bmcMac": "", "serial": "", "notes": ""});
    let with = |extra: Value| {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let sent: Vec<Value> = bodies(&ui)[1..]
        .iter()
        .map(|(_, body)| body.clone())
        .collect();
    assert_eq!(
        sent,
        [
            with(json!({"approveForSecs": "5400"})),
            with(json!({"approveUntilUnix": "1800000000"})),
            with(json!({"approveUntilUnix": "1800000030"})),
            with(json!({})),
        ]
    );
    let problem = "A window is a number of hours (2 or 1.5), or an end time in UTC (2027-01-15 08:00 or 2027-01-15T08:00:00Z).";
    let reports = out["seen"]["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 10);
    assert_eq!(
        reports[4..],
        [
            json!(problem),
            json!(problem),
            json!(problem),
            json!(problem),
            json!(problem),
            json!("A window needs a role: a record with no role approves nothing."),
        ]
    );
}

#[test]
fn enrollment_page_shows_the_window_and_state_of_a_matched_record() {
    // Proves: a request matched to one record says whether that record is open, closed or used
    // and when its window ends (or that it has none); a request that matches no record, or whose
    // coordinator says no state, gets no such line.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [
            {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
             "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1",
             "recordState": "RECORD_STATE_CLOSED", "recordApproveUntilUnix": "1799990000"},
            {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32),
             "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-2",
             "recordState": "RECORD_STATE_USED", "recordApproveUntilUnix": "1800003600"},
            {"requestId": "3d".repeat(32), "keySha256": "ef".repeat(32),
             "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-3",
             "recordState": "RECORD_STATE_CLOSED"},
            {"requestId": "4c".repeat(32), "keySha256": "12".repeat(32),
             "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-4"},
            {"requestId": "5b".repeat(32), "keySha256": "34".repeat(32),
             "inventoryMatch": "INVENTORY_MATCH_UNMATCHED", "recordState": "RECORD_STATE_OPEN"},
        ]}),
    );
    let out = ui.run(&page_driver("enrollment", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "The record is closed: its window ends 2027-01-15 05:13:20 UTC.",
            "The record is used: its window ends 2027-01-15 09:00:00 UTC.",
            "The record is closed (it has no window).",
        ],
    );
    assert_eq!(shown(&out).matches("The record is").count(), 3);
}
