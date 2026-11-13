//! The host inventory in the app: the Inventory page's calls (`Enrollment.ListRecords`,
//! `AddRecord`, `RemoveRecord`) and what it checks before sending, its Enrolled column and
//! filter, the Host page's link to a host's record, and the Enrollment page's match line and
//! the host id and role a match fills in.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

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
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "storage"}}]}),
    );
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"hostId": "db-1", "certificateIssuedAtPoll": true}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
const filled = [0, 1, 2, 3, 4].map((i) => [named(tree, "host-id")[i].attrs.value, chosen(tree, "host-group", i)]);
await click(tree, "approve", 0);
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
            (m("HostGroups/List"), json!({})),
            (m("Layout/Get"), json!({})),
            (
                m("Enrollment/Approve"),
                json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
                       "hostId": "db-1", "hostGroup": "storage", "replace": false})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Approved db-1 into host group storage: the host gets its certificate at its next poll.",
            "Name the host id.",
        ])
    );
    assert_shows(
        &out,
        &[
            "Matches the record of db-1 (host group storage).",
            "Matches several records: db-1, db-2. None is chosen.",
            "Not in inventory.",
            "Not matched against the inventory.",
            "Matches the record of mac-1 (no host group).",
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
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "storage"}}]}),
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
  choose(tree, "host-group", "storage", i);
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
               "hostGroup": "storage", "replace": replace})
    };
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (m("HostGroups/List"), json!({})),
            (m("Layout/Get"), json!({})),
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
            "Approved db-1 into host group storage: the host gets its certificate at its next poll.",
            "This request matches the record of db-1: approve it as db-1, or tick Replace to approve it as db-9.",
            "Approved db-9 into host group storage: the host gets its certificate at its next poll.",
            "Approved s6 into host group storage: the host gets its certificate at its next poll.",
            "Name the host id.",
            "Approved web-1 into host group storage: the host gets its certificate at its next poll.",
            "Approved db-4 into host group storage: the host gets its certificate at its next poll.",
        ])
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
