//! Where a server stands, set from the two pages that place it besides Home: the approval of an
//! enrollment request (Host group and one optional Sled slot, sent with the approval) and the Host
//! page (Place or Move, and Unplace). The slot is chosen from the free slots of the lab by address,
//! never typed, and no page asks for a height, row, rack or U; each refusal is told in words.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// Row 0 with a rack of 10 U that holds a 2 U one-slot sled at U 4 and 5 (C0-R0-R0-S0, with `db-1`
/// on its slot) and an empty 1 U sled of two slots at U 7 (C0-R0-R0-S1), and row 1 with a rack of
/// 4 U that holds an empty one-slot sled at U 1 (C0-R1-R0-S0).
fn layout() -> Value {
    json!({
        "rows": [{"id": "row-1", "number": 1}, {"id": "row-2", "number": 2}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10},
            {"id": "rack-2", "row": "row-2", "number": 1, "heightU": 4},
        ],
        "sleds": [
            {"id": "sled-1", "rack": "rack-1", "u": 4, "heightU": 2, "slots": 1, "address": "C0-R0-R0-S0"},
            {"id": "sled-2", "rack": "rack-1", "number": 1, "u": 7, "heightU": 1, "slots": 2,
             "address": "C0-R0-R0-S1"},
            {"id": "sled-3", "rack": "rack-2", "u": 1, "heightU": 1, "slots": 1, "address": "C0-R1-R0-S0"},
        ],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
             "rack": "rack-1", "u": 4, "heightU": 2, "sled": "sled-1", "address": "C0-R0-R0-S0-S0"},
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

fn approval(ui: &Ui) -> Value {
    bodies(ui)
        .into_iter()
        .find(|(method, _)| *method == m("Enrollment/Approve"))
        .expect("an approval")
        .1
}

/// Proves: the approval's controls are Host id, Host group, Sled slot and Replace: the Sled slot
/// is one choice whose first entry is Unplaced and whose others are the free slots of the lab by
/// address in order (the slot of db-1 is taken, so it is not offered); no Height, Row, Rack or U
/// control exists; approving with Unplaced left sends no placement at all.
/// Catches: a placement sent for an approval that chose none, and the old height, row, rack and U
/// controls left on the form.
#[test]
fn an_approval_offers_only_a_host_group_and_a_sled_slot_and_lands_unplaced_by_default() {
    let mut ui = enrollment_ui(false);
    ui.reply(&m("Enrollment/Approve"), json!({"hostId": "web-1"}));
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
const selects = nodes(tree).filter((n) => n.tag === "select").map((n) => n.attrs.name);
const slots = choices(tree, "place-slot", 0);
const first = chosen(tree, "place-slot", 0);
type(tree, "host-id", "web-1", 0);
await click(tree, "approve", 0);
return { selects, slots, first, reports: seen.reports };"#,
    ));
    assert_eq!(out["out"]["selects"], json!(["host-group", "place-slot"]));
    assert_eq!(
        out["out"]["slots"],
        json!(["Unplaced", "C0-R0-R0-S1-S0", "C0-R0-R0-S1-S1", "C0-R1-R0-S0-S0"])
    );
    assert_eq!(out["out"]["first"], "");
    assert_eq!(
        approval(&ui),
        json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "hostId": "web-1",
               "hostGroup": "", "replace": false})
    );
}

/// Proves: choosing a slot by address sends placementSled and placementSlot (the slot as a number)
/// beside the approval, and the report says where the host stands by address.
/// Catches: an approval that sends the old rack, placementU and placementHeightU, or that names the
/// sled without the slot.
#[test]
fn an_approval_names_the_sled_and_the_slot_chosen() {
    let mut ui = enrollment_ui(false);
    ui.reply(&m("Enrollment/Approve"), json!({"hostId": "web-1"}));
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"
type(tree, "host-id", "web-1", 0);
choose(tree, "place-slot", "sled-2:1", 0);
const kept = [named(tree, "host-id")[0].attrs.value, chosen(tree, "place-slot", 0)];
await click(tree, "approve", 0);
return { kept, reports: seen.reports };"#,
    ));
    assert_eq!(out["out"]["kept"], json!(["web-1", "sled-2:1"]));
    assert!(
        out["out"]["reports"][0]
            .as_str()
            .unwrap()
            .contains("at C0-R0-R0-S1-S1"),
        "{out}"
    );
    assert_eq!(
        approval(&ui),
        json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "hostId": "web-1",
               "hostGroup": "", "replace": false, "placementSled": "sled-2", "placementSlot": 1})
    );
}

/// Proves: a matched request says at which address its server stood before the wipe, and that it
/// stays there unless another slot is chosen (the slot is among the choices, since it is the
/// server's own); a refused placement is told in words and any other refusal by its token.
/// Catches: a matched request that hides where its server stood, a taken slot shown as a bare
/// token, and a refusal unrelated to the placement that is hidden.
#[test]
fn a_matched_server_says_where_it_stands_and_a_refused_placement_is_told_in_words() {
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
        r#"
const text0 = text(tree);
const slots = choices(tree, "place-slot", 0);
choose(tree, "place-slot", "sled-2:0", 0);
await click(tree, "approve", 0);
await click(tree, "approve", 0);
return { text0, slots, reports: seen.reports };"#,
    ));
    let out = &out["out"];
    assert!(
        out["text0"].as_str().unwrap().contains(
            "db-1 stands at C0-R0-R0-S0-S0, and stays there unless another slot is chosen."
        ),
        "{out}"
    );
    assert_eq!(out["slots"][1], "C0-R0-R0-S0-S0");
    assert_eq!(
        out["reports"],
        json!(["That slot is taken.", "FAILED_PRECONDITION not_pending"])
    );
}

/// Catches: an approval form that fails when the layout cannot be read, or one that sends a
/// placement it never offered.
#[test]
fn an_approval_whose_layout_cannot_be_read_still_goes_unplaced() {
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
    let approve = approval(&ui);
    assert!(approve.get("placementSled").is_none(), "{approve}");
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

/// Proves: an unplaced host's Placement section says so, offers the free slots by address in a
/// Sled slot choice (no height, row, rack or U), and Place sends one Place with the host subject,
/// the sled and the slot, then reports the address.
/// Catches: a host placed under a guessed height or U, a subject of the wrong kind, and a place
/// that is not reported.
#[test]
fn the_host_page_places_an_unplaced_host_on_a_free_sled_slot() {
    let mut ui = host_ui("web-2", "");
    ui.reply(&m("Layout/Place"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const standing = text(named(tree, "standing")[0]);
const slots = choices(tree, "place-slot", 0);
const old = ["place-height", "place-row", "place-rack", "place-u"].filter((n) => nodes(tree).some((x) => x.attrs.name === n));
choose(tree, "place-slot", "sled-3:0", 0);
const label = text(named(tree, "place")[0]);
await click(tree, "place");
return { standing, slots, old, label, reports: seen.reports,
         unplace: nodes(tree).filter((n) => n.attrs["name"] === "unplace").length };"#,
    ));
    let out = &out["out"];
    assert_eq!(out["standing"], "Unplaced: it stands on no sled.");
    assert_eq!(
        out["slots"],
        json!(["C0-R0-R0-S1-S0", "C0-R0-R0-S1-S1", "C0-R1-R0-S0-S0"])
    );
    assert_eq!(out["old"], json!([]));
    assert_eq!(out["label"], "Place");
    assert_eq!(out["unplace"], 0);
    assert_eq!(out["reports"], json!(["web-2 is at C0-R1-R0-S0-S0."]));
    assert!(bodies(&ui).contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"},
               "sled": "sled-3", "slot": 0, "reason": "placed web-2"})
    )));
}

/// Proves: a placed server says its address, rack and U; its Sled slot choice starts at the slot it
/// stands on (which counts as free for it); Move sends Place under the inventory subject of its
/// entry (not the host id) with the sled and slot chosen; a refusal is told in words; Unplace sends
/// Unplace for that subject.
/// Catches: a server placed by its host id when an inventory entry matches it (which a wipe would
/// lose), a move that counts the server's own slot as taken, a missing Unplace, and a refusal told
/// as a token.
#[test]
fn the_host_page_moves_a_placed_server_by_its_entry_and_unplaces_it() {
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
const own = choices(tree, "place-slot", 0);
const start = chosen(tree, "place-slot", 0);
const label = text(named(tree, "place")[0]);
const crumbs = text(nodes(tree).find((n) => (n.attrs.class || "") === "crumbs"));
choose(tree, "place-slot", "sled-2:1", 0);
await click(tree, "place");
await click(tree, "place");
await click(tree, "unplace");
return { standing, own, start, label, crumbs, reports: seen.reports };"#,
    ));
    let out = &out["out"];
    assert_eq!(
        out["standing"],
        "Stands at C0-R0-R0-S0-S0 (Row 0 / Rack 0, U4, a sled 2U high)."
    );
    assert_eq!(out["label"], "Move");
    assert_eq!(out["start"], "sled-1:0");
    assert_eq!(out["own"][0], "C0-R0-R0-S0-S0");
    assert_eq!(
        out["crumbs"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        "Lab Row 0 Rack 0 db-1"
    );
    assert_eq!(
        out["reports"],
        json!([
            "That does not fit in the rack: it would reach past U 1 or the top U.",
            "db-1 is at C0-R0-R0-S1-S1.",
            "db-1 is unplaced."
        ])
    );
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
               "sled": "sled-2", "slot": 1, "reason": "moved db-1"})
    )));
    assert!(sent.contains(&(
        m("Layout/Unplace"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
               "reason": "unplaced db-1"})
    )));
}

/// Proves: with no free slot the section says to add a sled on Home and Place sends nothing,
/// telling the operator to choose a slot; an unreadable layout is told in the section.
/// Catches: a Place button that sends nothing and says nothing when there is no slot to choose
/// from, and a host page that fails when the layout cannot be read.
#[test]
fn the_host_page_without_a_free_slot_or_a_layout_says_why_it_cannot_place() {
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": "web-2"}}));
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const shown = text(tree);
await click(tree, "place");
return { shown, reports: seen.reports };"#,
    ));
    assert!(
        out["out"]["shown"]
            .as_str()
            .unwrap()
            .contains("No sled slot is free: add a sled on Home, in Edit layout."),
        "{out}"
    );
    assert_eq!(out["out"]["reports"], json!(["Choose a sled slot."]));
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
