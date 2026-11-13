//! The Enrollment page's approval screen for network-installed machines (network-install design
//! section 7), run in the JavaScript engine against the fake gateway of `support`: the claims
//! that are hard to copy beside the record's values, "matched" said not to be proof, and a
//! contested request named as one and not approved.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// What the page says beside the match line of a matched Linux request, word for word.
const NOT_PROOF: &str = "Matched means this request claims a data MAC of the record and does not \
    contradict its BMC MAC. The machine wrote the claims itself, and anyone who read the label or \
    the network can write the same. Approve only the key hash shown on this machine's console.";

fn compared_reply() -> Value {
    json!({"requests": [
       {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
        "claims": {"addresses": ["0c:c4:7a:00:00:02"], "dmiSerial": "SN-1",
                   "bmcMac": "0C-C4-7A-00-00-01", "productUuid": "4c4c4544-0001"},
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1", "recordRole": "storage",
        "recordSerial": "sn-1", "recordBmcMac": "0c:c4:7a:00:00:01"},
       {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32),
        "claims": {"addresses": ["0c:c4:7a:00:00:12"]},
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-2",
        "recordSerial": "SN-2", "recordBmcMac": "0c:c4:7a:00:00:11"},
       {"requestId": "3d".repeat(32), "keySha256": "ef".repeat(32),
        "claims": {"addresses": ["0c:c4:7a:00:00:22"], "dmiSerial": "SN-X"},
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-3", "recordSerial": "SN-3"},
       {"requestId": "4c".repeat(32), "keySha256": "12".repeat(32),
        "claims": {"dmiSerial": "SN-4"},
        "inventoryMatch": "INVENTORY_MATCH_AMBIGUOUS", "candidateHostIds": ["db-1", "db-2"]},
       {"requestId": "5b".repeat(32), "keySha256": "34".repeat(32),
        "claims": {"macSerial": "C02XK1ABJG5J"},
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "mac-1",
        "recordSerial": "C02XK1ABJG5J"},
    ]})
}

#[test]
fn a_matched_request_shows_its_hard_to_copy_claims_beside_the_record_and_that_matched_is_not_proof()
{
    // Catches: the serial, BMC MAC or product UUID left off the approval screen, or shown
    // without the record's value; a value the record does not hold, or a claim left out, shown
    // as agreement; a serial or MAC that agree in another spelling shown as differing, and one
    // that differs shown as the same; a recorded BMC MAC the request does not claim shown with
    // no warning; the "matched is not proof" text missing beside a matched Linux request, or
    // shown for an ambiguous or a Mac request, which match by no data MAC.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), compared_reply());
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
return {
  compared: rows(tree).filter((cells) => cells.length === 4),
  warned: nodes(tree).filter((n) => n.tag === "tr" && n.attrs["class"] === "warning").length,
};
"#,
    ));
    assert_eq!(
        out["out"]["compared"],
        json!([
            ["Hard to copy", "This request", "The record", ""],
            ["DMI serial", "SN-1", "sn-1", "same"],
            ["BMC MAC", "0C-C4-7A-00-00-01", "0c:c4:7a:00:00:01", "same"],
            [
                "Product UUID",
                "4c4c4544-0001",
                "not recorded",
                "not recorded"
            ],
            ["Hard to copy", "This request", "The record", ""],
            ["DMI serial", "not claimed", "SN-2", "not claimed"],
            ["BMC MAC", "not claimed", "0c:c4:7a:00:00:11", "not claimed"],
            ["Product UUID", "not claimed", "not recorded", "not claimed"],
            ["Hard to copy", "This request", "The record", ""],
            ["DMI serial", "SN-X", "SN-3", "differs"],
            ["BMC MAC", "not claimed", "not recorded", "not claimed"],
            ["Product UUID", "not claimed", "not recorded", "not claimed"],
        ])
    );
    // Every row but the two that agree.
    assert_eq!(out["out"]["warned"], 7);
    let text = shown(&out);
    assert_eq!(text.matches(NOT_PROOF).count(), 3, "{text}");
    let unclaimed = "The record holds the BMC MAC 0c:c4:7a:00:00:11, and this request does not \
                     claim one: it matched on a data MAC alone.";
    assert_eq!(text.matches(unclaimed).count(), 1, "{text}");
    assert_shows(
        &out,
        &[
            "Matches several records: db-1, db-2. None is chosen.",
            "Matches the record of mac-1 (no host group).",
        ],
    );
}

fn contested_reply() -> Value {
    let (a, b, c, d) = (
        "1f".repeat(32),
        "2e".repeat(32),
        "3d".repeat(32),
        "4c".repeat(32),
    );
    json!({"requests": [
       {"requestId": a, "keySha256": a, "contestedBy": [b],
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1", "recordRole": "storage"},
       {"requestId": b, "keySha256": b, "contestedBy": [a],
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-1", "recordRole": "storage"},
       {"requestId": c, "keySha256": c, "contestedBy": [d, "5b".repeat(32)],
        "inventoryMatch": "INVENTORY_MATCH_AMBIGUOUS", "candidateHostIds": ["db-2", "db-3"]},
       {"requestId": d, "keySha256": d,
        "inventoryMatch": "INVENTORY_MATCH_MATCHED", "recordHostId": "db-4", "recordRole": "storage"},
    ]})
}

#[test]
fn a_contested_request_names_the_others_and_is_not_approved_from_the_page() {
    // Catches: a contested request shown as a plain match, with the others it contests left out;
    // a hurried Approve of it sent to the coordinator (with the record's host id, or another with
    // Replace); and an uncontested request held back by its neighbours' contest.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), contested_reply());
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "storage"}}]}),
    );
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"hostId": "db-4", "certificateIssuedAtPoll": true}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[true]",
        r#"
await click(tree, "approve", 0);
type(tree, "host-id", "db-9", 1);
tick(tree, "replace", true, 1);
await click(tree, "approve", 1);
type(tree, "host-id", "db-2", 2);
choose(tree, "host-group", "storage", 2);
await click(tree, "approve", 2);
await click(tree, "approve", 3);
"#,
    ));
    let (a, b, d) = ("1f".repeat(32), "2e".repeat(32), "4c".repeat(32));
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (m("HostGroups/List"), json!({})),
            (
                m("Enrollment/Approve"),
                json!({"requestId": d, "keySha256": d, "hostId": "db-4", "hostGroup": "storage",
                       "replace": false})
            ),
        ]
    );
    let refused =
        "This request is contested: reject the other requests that match its record first.";
    assert_eq!(
        out["seen"]["reports"],
        json!([
            refused,
            refused,
            refused,
            "Approved db-4 into host group storage: the host gets its certificate at its next poll."
        ])
    );
    let after = "Approval is refused until all but one are rejected. Find this machine's key hash \
                 on its console, and reject the requests that are not it.";
    let text = shown(&out);
    for line in [
        format!("Contested: another pending request matches the same record: {b}. {after}"),
        format!("Contested: another pending request matches the same record: {a}. {after}"),
        format!(
            "Contested: 2 other pending requests match the same record: {d}, {}. {after}",
            "5b".repeat(32)
        ),
    ] {
        assert_eq!(text.matches(line.as_str()).count(), 1, "{line} in {text}");
    }
    assert_eq!(text.matches("Contested:").count(), 3, "{text}");
}
