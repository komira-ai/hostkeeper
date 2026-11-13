//! Where a server stands, set from the two pages that place it: the approval of an enrollment
//! request (Row, Rack, Height and U position, all optional, sent with the approval) and the Host
//! page (Place or Move, and Unplace). Each choice is from the lab's layout, never typed; each
//! refusal is told in words.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// Row 1 with a rack of 10 U (a 2 U server at U 4 and 5), and row 2 with a rack of 4 U.
fn layout() -> Value {
    json!({
        "rows": [{"id": "row-1", "number": 1}, {"id": "row-2", "number": 2}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10},
            {"id": "rack-2", "row": "row-2", "number": 1, "heightU": 4},
        ],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
             "rack": "rack-1", "u": 4, "heightU": 2},
        ],
    })
}

fn pending(matched: bool) -> Value {
    let mut request = json!({
        "requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "image": "debian-hk",
        "firstSeenUnix": "1800000000",
        "claims": {"machineId": "m-0001", "addresses": ["192.0.2.10"]},
    });
    if matched {
        request["inventoryMatch"] = json!("INVENTORY_MATCH_MATCHED");
        request["recordHostId"] = json!("db-1");
    }
    json!({"requests": [request]})
}

fn enrollment_ui(matched: bool) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Enrollment/List"), pending(matched));
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "storage"}}]}),
    );
    ui.reply(&m("Layout/Get"), layout());
    ui
}

const HELPERS: &str = r#"
const last = () => seen.shown[seen.shown.length - 1];
"#;

#[test]
fn an_approval_without_a_placement_lands_unplaced_and_sends_no_rack() {
    // Catches: a placement sent for an approval that chose none, a Row control that has no
    // "Unplaced" and cannot be left alone, and a rack or U choice offered with no row chosen.
    let ui = enrollment_ui(false);
    ui.reply(&m("Enrollment/Approve"), json!({"hostId": "web-1"}));
    let mut ui = ui;
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const rows = choices(tree, "place-row", 0);
const racks = choices(tree, "place-rack", 0);
type(tree, "host-id", "web-1", 0);
await click(tree, "approve", 0);
return {{ rows, racks, reports: seen.reports }};"#
        ),
    ));
    assert_eq!(out["out"]["rows"], json!(["Unplaced", "Row 1", "Row 2"]));
    assert_eq!(out["out"]["racks"], json!([]));
    assert_eq!(
        out["out"]["reports"],
        json!([
            "Approved web-1 with no host group (it runs the packages of all until it is put in one): the host gets its certificate at its next poll."
        ])
    );
    let approve = bodies(&ui)
        .into_iter()
        .find(|(method, _)| *method == m("Enrollment/Approve"))
        .expect("an approval")
        .1;
    assert_eq!(
        approve,
        json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "hostId": "web-1",
               "hostGroup": "", "replace": false})
    );
}

#[test]
fn an_approval_names_the_row_the_rack_the_height_and_a_free_u() {
    // Catches: a U position offered that the height chosen would overlap (the free slots are
    // those that fit), a choice that is lost when the form is drawn again, and an approval that
    // sends the rack without the U and the height the coordinator needs with it.
    let mut ui = enrollment_ui(false);
    ui.reply(&m("Enrollment/Approve"), json!({"hostId": "web-1"}));
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
type(tree, "host-id", "web-1", 0);
choose(tree, "place-row", "row-1", 0);
let now = last();
const racks = choices(now, "place-rack", 0);
const starts1 = choices(now, "place-u", 0);
choose(now, "place-height", "3", 0);
now = last();
const starts3 = choices(now, "place-u", 0);
choose(now, "place-u", "7", 0);
const kept = [named(now, "host-id")[0].attrs.value, chosen(now, "place-height", 0), chosen(now, "place-row", 0)];
await click(now, "approve", 0);
return {{ racks, starts1, starts3, kept, reports: seen.reports }};"#
        ),
    ));
    let out = &out["out"];
    assert_eq!(out["racks"], json!(["Rack 1"]));
    // The 10 U rack holds a server at U 4 and 5: a 1 U server fits at 1, 2, 3 and 6 to 10; a 3 U
    // server only at 1, and at 6, 7 and 8.
    assert_eq!(
        out["starts1"],
        json!(["U1", "U2", "U3", "U6", "U7", "U8", "U9", "U10"])
    );
    assert_eq!(out["starts3"], json!(["U1", "U6", "U7", "U8"]));
    assert_eq!(out["kept"], json!(["web-1", "3", "row-1"]));
    assert!(
        out["reports"][0]
            .as_str()
            .unwrap()
            .contains("at Row 1 / Rack 1, U7")
    );
    let approve = bodies(&ui)
        .into_iter()
        .find(|(method, _)| *method == m("Enrollment/Approve"))
        .expect("an approval")
        .1;
    assert_eq!(
        approve,
        json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "hostId": "web-1",
               "hostGroup": "", "replace": false,
               "rack": "rack-1", "placementU": 7, "placementHeightU": 3})
    );
}

#[test]
fn a_matched_server_says_where_it_stands_and_a_refused_placement_is_told_in_words() {
    // Catches: a matched request that hides where its server stood before the wipe, an overlap
    // refusal shown as a bare token, and a refusal that unrelated to the placement is hidden.
    let mut ui = enrollment_ui(true);
    ui.refuse(
        &m("Enrollment/Approve"),
        412,
        "FAILED_PRECONDITION",
        "placement_overlap",
    );
    ui.refuse(
        &m("Enrollment/Approve"),
        412,
        "FAILED_PRECONDITION",
        "not_pending",
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const text0 = text(tree);
choose(tree, "place-row", "row-1", 0);
let now = last();
choose(now, "place-u", "1", 0);
await click(now, "approve", 0);
await click(now, "approve", 0);
return {{ text0, reports: seen.reports }};"#
        ),
    ));
    let out = &out["out"];
    assert!(
        out["text0"].as_str().unwrap().contains(
            "db-1 stands at Row 1 / Rack 1, U4, and stays there unless another place is chosen."
        ),
        "{out}"
    );
    assert_eq!(
        out["reports"],
        json!([
            "Another server already stands in one of those U.",
            "FAILED_PRECONDITION not_pending"
        ])
    );
}

#[test]
fn an_approval_whose_layout_cannot_be_read_still_goes_unplaced() {
    // Catches: an approval form that fails when the layout cannot be read, or one that sends a
    // placement it never offered.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), pending(false));
    ui.reply(&m("HostGroups/List"), json!({"hostGroups": []}));
    ui.refuse(&m("Layout/Get"), 503, "UNAVAILABLE", "store_failed");
    ui.reply(&m("Enrollment/Approve"), json!({"hostId": "web-1"}));
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
type(tree, "host-id", "web-1", 0);
await click(tree, "approve", 0);
return text(tree);"#,
    ));
    assert!(
        out["out"]
            .as_str()
            .unwrap()
            .contains("The lab layout cannot be read (UNAVAILABLE store_failed): the host can be approved unplaced.")
    );
    let approve = bodies(&ui)
        .into_iter()
        .find(|(method, _)| *method == m("Enrollment/Approve"))
        .expect("an approval")
        .1;
    assert!(approve.get("rack").is_none(), "{approve}");
}

fn host_ui(node: &str, entry: &str) -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": node, "role": "db", "live": true, "lastHeartbeatUnix": "1800000000",
                        "inventoryRecordId": entry}}),
    );
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), layout());
    ui
}

#[test]
fn the_host_page_places_an_unplaced_host_from_choices_of_the_layout() {
    // Catches: a host placed with a U that does not fit, a U choice that is not made again when
    // the height changes, a subject of the wrong kind, and a place that is not reported.
    let mut ui = host_ui("web-2", "");
    ui.reply(&m("Layout/Place"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const standing = text(named(tree, "standing")[0]);
const heights = choices(tree, "place-height", 0);
const starts1 = choices(tree, "place-u", 0);
choose(tree, "place-height", "3", 0);
const starts3 = choices(tree, "place-u", 0);
const shows = seen.shown.length;
choose(tree, "place-row", "row-2", 0);
const rack2 = choices(tree, "place-rack", 0);
const starts2 = choices(tree, "place-u", 0);
choose(tree, "place-row", "row-1", 0);
choose(tree, "place-u", "6", 0);
const label = text(named(tree, "place")[0]);
await click(tree, "place");
return { standing, heights, starts1, starts3, shows, rack2, starts2, label, reports: seen.reports,
         unplace: nodes(tree).filter((n) => n.attrs["name"] === "unplace").length };"#,
    ));
    let out = &out["out"];
    assert_eq!(out["standing"], "Unplaced: it stands in no rack.");
    assert_eq!(out["heights"], json!(["1U", "2U", "3U", "4U"]));
    assert_eq!(
        out["starts1"],
        json!(["U1", "U2", "U3", "U6", "U7", "U8", "U9", "U10"])
    );
    assert_eq!(out["starts3"], json!(["U1", "U6", "U7", "U8"]));
    assert_eq!(out["shows"], 1, "the page is drawn again after a choice");
    assert_eq!(out["rack2"], json!(["Rack 1"]));
    assert_eq!(
        out["starts2"],
        json!(["U1", "U2"]),
        "a 3 U server in a 4 U rack"
    );
    assert_eq!(out["label"], "Place");
    assert_eq!(out["unplace"], 0);
    assert_eq!(out["reports"], json!(["web-2 is at Row 1 / Rack 1, U6."]));
    assert!(bodies(&ui).contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"},
               "rack": "rack-1", "heightU": 3, "u": 6, "reason": "placed web-2"})
    )));
}

#[test]
fn the_host_page_moves_a_placed_server_by_its_entry_and_unplaces_it() {
    // Catches: a server placed by its host id when an inventory entry matches it (which a wipe
    // would lose), a move that counts the server's own U as taken, a missing Unplace, and a
    // refusal told as a token.
    let mut ui = host_ui("db-1", "db-1");
    ui.refuse(
        &m("Layout/Place"),
        412,
        "FAILED_PRECONDITION",
        "out_of_rack",
    );
    ui.reply(&m("Layout/Place"), json!({}));
    ui.reply(&m("Layout/Unplace"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "db-1" }"#,
        "[]",
        r#"
const standing = text(named(tree, "standing")[0]);
const own = choices(tree, "place-u", 0);
const label = text(named(tree, "place")[0]);
const crumbs = text(nodes(tree).find((n) => (n.attrs.class || "") === "crumbs"));
choose(tree, "place-u", "5", 0);
await click(tree, "place");
choose(tree, "place-u", "4", 0);
await click(tree, "place");
await click(tree, "unplace");
return { standing, own, label, crumbs, reports: seen.reports };"#,
    ));
    let out = &out["out"];
    assert_eq!(out["standing"], "Stands at Row 1 / Rack 1, U4, 2U.");
    assert_eq!(out["label"], "Move");
    assert_eq!(
        out["crumbs"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        "Lab Row 1 Rack 1 db-1"
    );
    // Its own U 4 and 5 count as free for a 2 U server, so U 4 stays a choice.
    assert!(
        out["own"].as_array().unwrap().contains(&json!("U4")),
        "{out}"
    );
    assert_eq!(
        out["reports"],
        json!([
            "That does not fit in the rack: it would reach past U 1 or the top U.",
            "db-1 is at Row 1 / Rack 1, U4.",
            "db-1 is unplaced."
        ])
    );
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
               "rack": "rack-1", "heightU": 2, "u": 4, "reason": "moved db-1"})
    )));
    assert!(sent.contains(&(
        m("Layout/Unplace"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
               "reason": "unplaced db-1"})
    )));
}

#[test]
fn the_host_page_without_a_rack_or_a_layout_says_why_it_cannot_place() {
    // Catches: a Place button that sends nothing and says nothing when there is no rack to choose
    // from, and a host page that fails when the layout cannot be read.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": "web-2"}}));
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
await click(tree, "place");
return seen.reports;"#,
    ));
    assert_eq!(
        out["out"],
        json!(["There is no rack yet: add rows and racks on Home, in Edit layout."])
    );
    assert!(!ui.methods().contains(&m("Layout/Place")));

    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": "web-2"}}));
    ui.reply(&m("Services/List"), json!({}));
    ui.refuse(&m("Layout/Get"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&page_driver("host", r#"{ nodeId: "web-2" }"#, "[]", ""));
    assert!(
        out["text"]
            .as_str()
            .unwrap()
            .contains("The lab layout cannot be read: UNAVAILABLE store_failed")
    );
}
