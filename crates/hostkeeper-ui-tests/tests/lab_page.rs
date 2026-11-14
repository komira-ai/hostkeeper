//! The Home page, the lab: the floor plan of rows, the racks of a row as front elevations, one rack
//! with its hosts, the breadcrumbs, the strip's scrolling, and the edit mode's calls and refusals,
//! each run in the JavaScript engine against the fake gateway of `support`.

mod support;

#[path = "lab_page/views.rs"]
mod views;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// The lab of the tests: the cluster `Lab 1`, row 1 with a rack of 42 U and one of 24 U, row 2 with
/// a rack of 42 U. In the first rack `web-1` (a host no entry matches) stands on the one slot of a
/// 2 U sled at U 41 and 42 (address C0-R0-R0-S1-S0), `db-1` (an inventory entry a host is enrolled
/// against) on a 1 U sled at U 1 (C0-R0-R0-S0-S0).
fn layout() -> Value {
    json!({
        "name": "Lab 1",
        "rows": [{"id": "row-1", "number": 0}, {"id": "row-2", "number": 1}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 0, "heightU": 42},
            {"id": "rack-2", "row": "row-1", "number": 1, "heightU": 24},
            {"id": "rack-3", "row": "row-2", "number": 0, "heightU": 42},
        ],
        "sleds": [
            {"id": "sled-1", "rack": "rack-1", "number": 1, "u": 41, "heightU": 2, "slots": 1,
             "address": "C0-R0-R0-S1"},
            {"id": "sled-2", "rack": "rack-1", "u": 1, "heightU": 1, "slots": 1,
             "address": "C0-R0-R0-S0"},
        ],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-1"},
             "rack": "rack-1", "u": 41, "heightU": 2, "sled": "sled-1", "address": "C0-R0-R0-S1-S0"},
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
             "rack": "rack-1", "u": 1, "heightU": 1, "sled": "sled-2", "address": "C0-R0-R0-S0-S0"},
        ],
    })
}

/// Four hosts (healthy and placed, updating and placed through its entry, healthy and not placed,
/// offline and not placed) and one inventory entry no host is enrolled against.
fn hosts() -> Value {
    json!({"hosts": [
        {"nodeId": "web-1", "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
         "hostGroup": "web",
         "services": [{"service": "hostkeeper", "installedVersion": "1.4.0", "health": "HEALTH_HEALTHY"}]},
        {"nodeId": "db-1", "role": "db", "live": true, "lastHeartbeatUnix": "1800000000",
         "hostGroup": "storage", "inventoryRecordId": "db-1",
         "services": [{"service": "hostkeeper", "installedVersion": "1.4.0", "holdsSlot": true}]},
        {"nodeId": "web-2", "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
         "services": [{"service": "hostkeeper", "installedVersion": "1.3.0"}]},
        {"nodeId": "db-9", "role": "db", "live": false, "lastHeartbeatUnix": "1799990000"},
    ]})
}

fn records() -> Value {
    json!({"records": [
        {"hostId": "db-1", "role": "db", "enrolled": true},
        {"hostId": "new-1", "role": "db", "enrolled": false},
    ]})
}

/// A gateway that answers the reads of one rendering but the layout, which the test scripts.
fn base() -> Ui {
    let ui = Ui::start();
    reads(&ui);
    ui
}

/// The answers of the reads of one rendering but the layout's.
fn reads(ui: &Ui) {
    ui.reply(&m("Hosts/List"), hosts());
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), records());
}

/// Four successes of each write of `Layout`.
fn writes(ui: &Ui) {
    for method in [
        "AddRow",
        "PutName",
        "PutRack",
        "RemoveRack",
        "RemoveRow",
        "PutSled",
        "RemoveSled",
        "Place",
        "Unplace",
    ] {
        for _ in 0..4 {
            ui.reply(&m(&format!("Layout/{method}")), json!({}));
        }
    }
}

/// The lab of [`layout`] and the answers of every write.
fn start() -> Ui {
    let ui = base();
    ui.reply(&m("Layout/Get"), layout());
    writes(&ui);
    ui
}

/// Renders Home with `params`, then runs `then` (see `page_driver`), after the helpers below.
fn home(ui: &mut Ui, params: &str, then: &str) -> Value {
    let helpers = r#"
const cls = (tree, c) => nodes(tree).filter((n) => (n.attrs.class || "").split(" ").includes(c));
const words = (n) => text(n).replace(/\s+/g, " ").trim();
const last = () => seen.shown[seen.shown.length - 1];
"#;
    ui.run(&page_driver(
        "home",
        params,
        "[]",
        &format!("{helpers}{then}"),
    ))["out"]
        .clone()
}

#[test]
fn the_floor_plan_shows_each_row_with_its_racks_and_the_states() {
    // Catches: a floor plan that leaves out a row or a rack, counts a server of another rack, shows
    // a state with no count, or lists a placed server in the Unplaced tray.
    let mut ui = start();
    let out = home(
        &mut ui,
        "{}",
        r#"
return {
  title: words(nodes(tree).find((n) => n.tag === "h2")),
  rows: named(tree, "open-row").map(words),
  feet: cls(tree, "foot").map((n) => n.attrs.title),
  pills: cls(tree, "pill").map(words),
  tray: cls(tree, "chip").map((n) => n.attrs.title),
  crumbs: cls(tree, "crumbs").length,
};"#,
    );
    assert_eq!(out["title"], "Lab 1");
    assert_eq!(
        out["crumbs"], 0,
        "the floor plan is the top: no breadcrumbs"
    );
    let rows = out["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert!(rows[0].as_str().unwrap().starts_with("Row 0"), "{rows:?}");
    assert!(rows[0].as_str().unwrap().contains("2 servers"), "{rows:?}");
    assert!(rows[1].as_str().unwrap().starts_with("Row 1"), "{rows:?}");
    assert!(rows[1].as_str().unwrap().contains("0 servers"), "{rows:?}");
    assert_eq!(
        out["feet"],
        json!([
            "Row 0 / Rack 0: 3/42 U",
            "Row 0 / Rack 1: 0/24 U",
            "Row 1 / Rack 0: 0/42 U"
        ])
    );
    assert_eq!(
        out["pills"],
        json!([
            "healthy 2",
            "updating 1",
            "failed 0",
            "offline 1",
            "pending 1"
        ])
    );
    assert_eq!(
        out["tray"],
        json!([
            "web-2 | version 1.3.0 | healthy",
            "db-9 | offline",
            "new-1 | pending"
        ])
    );
    assert_eq!(
        ui.methods(),
        [
            m("Hosts/List"),
            m("Releases/Status"),
            m("Alerts/List"),
            m("Layout/Get"),
            m("Enrollment/ListRecords")
        ]
    );
}

#[test]
fn a_click_on_a_row_opens_it_and_the_crumbs_lead_back() {
    // Catches: a row that does not open, a crumb that goes to the wrong level, and a trail that
    // does not name where the view is.
    let mut ui = start();
    let out = home(
        &mut ui,
        "{}",
        r#"
await click(tree, "open-row", 1);
return seen.navigations;"#,
    );
    assert_eq!(out, json!([{"name": "home", "params": {"row": "row-2"}}]));
    reads(&ui);
    ui.reply(&m("Layout/Get"), layout());
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "open-rack", 1);
return seen.navigations;"#,
    );
    assert_eq!(
        out,
        json!([{"name": "home", "params": {"rack": "rack-2", "edit": true}}]),
        "a rack opens alone, in the mode the operator is in"
    );
    reads(&ui);
    ui.reply(&m("Layout/Get"), layout());
    let out = home(
        &mut ui,
        r#"{ rack: "rack-2" }"#,
        r#"
const trail = words(cls(tree, "crumbs")[0]);
await click(tree, "crumb-lab");
await click(tree, "crumb-row");
return { trail, navigations: seen.navigations, title: words(nodes(tree).find((n) => n.tag === "h2")) };"#,
    );
    assert_eq!(out["trail"], "Lab Row 0 Rack 1");
    assert_eq!(out["title"], "Row 0 / Rack 1");
    assert_eq!(
        out["navigations"],
        json!([
            {"name": "home", "params": {}},
            {"name": "home", "params": {"row": "row-1"}}
        ])
    );
}

#[test]
fn a_row_shows_its_racks_as_elevations_with_each_server_in_its_slots() {
    // Catches: a server drawn at the wrong U or the wrong height, a blank panel over a U that is
    // taken, a missing U number, a tooltip without the group, the version or the state, and a
    // click that does not open the host.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
const racks = cls(tree, "rack");
const plate = (n) => ({ title: n.attrs.title, r: String(n.attrs.vars["--r"]), s: String(n.attrs.vars["--s"]), class: n.attrs.class });
const servers = named(tree, "open-server");
await click(tree, "open-server", 0);
return {
  racks: racks.map((n) => n.attrs["aria-label"]),
  units: cls(tree, "u-num").length,
  servers: servers.map(plate),
  blanks: cls(tree, "blank").map((n) => [String(n.attrs.vars["--r"]), String(n.attrs.vars["--s"])]),
  heads: cls(tree, "rack-head").map(words),
  strip: cls(tree, "rack-strip")[0].attrs.tabindex,
  navigations: seen.navigations,
  tray: words(cls(tree, "tray")[0]),
};"#,
    );
    assert_eq!(out["racks"], json!(["Row 0 / Rack 0", "Row 0 / Rack 1"]));
    assert_eq!(out["units"], 66);
    assert_eq!(
        out["servers"],
        json!([
            {"title": "web-1 | group web | version 1.4.0 | healthy | C0-R0-R0-S1-S0", "r": "1", "s": "2",
             "class": "server s-healthy tall"},
            {"title": "db-1 | group storage | version 1.4.0 | updating | C0-R0-R0-S0-S0", "r": "42", "s": "1",
             "class": "server s-updating"},
        ])
    );
    assert_eq!(
        out["blanks"],
        json!([["3", "39"], ["1", "24"]]),
        "a free run is one panel, over the U it covers"
    );
    assert_eq!(out["heads"], json!(["Rack 0 3/42 U", "Rack 1 0/24 U"]));
    assert_eq!(out["strip"], "0");
    assert_eq!(
        out["navigations"],
        json!([{"name": "host", "params": {"nodeId": "web-1"}}])
    );
    assert!(out["tray"].as_str().unwrap().contains("Unplaced (3)"));
}

#[test]
fn the_strip_scrolls_by_arrow_keys_and_by_dragging_but_not_when_a_button_is_pressed() {
    // Catches: a strip the keyboard cannot move, a drag that moves the wrong way or scrolls
    // when a server was pressed, and an arrow that is not consumed.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
const strip = cls(tree, "rack-strip")[0];
const moves = [];
const element = { scrollLeft: 50, scrollBy: (o) => moves.push(o.left) };
let prevented = 0;
strip.on.keydown({ key: "ArrowRight", currentTarget: element, preventDefault: () => { prevented += 1; } });
strip.on.keydown({ key: "ArrowLeft", currentTarget: element, preventDefault: () => { prevented += 1; } });
strip.on.keydown({ key: "a", currentTarget: element, preventDefault: () => { prevented += 1; } });
strip.on.pointerdown({ target: { closest: () => ({}) }, clientX: 100, currentTarget: element });
strip.on.pointermove({ clientX: 40, currentTarget: element });
const pressed = element.scrollLeft;
strip.on.pointerdown({ target: { closest: () => null }, clientX: 100, currentTarget: element });
strip.on.pointermove({ clientX: 102, currentTarget: element });
const small = element.scrollLeft;
strip.on.pointermove({ clientX: 60, currentTarget: element });
const dragged = element.scrollLeft;
strip.on.pointerup({});
strip.on.pointermove({ clientX: 0, currentTarget: element });
return { moves, prevented, pressed, small, dragged, after: element.scrollLeft };"#,
    );
    assert_eq!(out["moves"], json!([280, -280]));
    assert_eq!(out["prevented"], 2);
    assert_eq!(out["pressed"], 50, "a press on a button starts no drag");
    assert_eq!(out["small"], 50, "two pixels are a click");
    assert_eq!(
        out["dragged"], 90,
        "40 pixels to the left scroll 40 to the right"
    );
    assert_eq!(out["after"], 90, "no drag after the release");
}

#[test]
fn a_rack_shows_its_servers_in_a_table_and_a_pending_entry_opens_the_inventory() {
    // Catches: a rack page that lists servers out of U order, drops a state, or sends a click on a
    // server with no host (an inventory entry) to a host page that does not exist.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ rack: "rack-1" }"#,
        r#"
return { rows: rows(tree), big: cls(tree, "rack").map((n) => n.attrs.class) };"#,
    );
    assert_eq!(
        out["rows"],
        json!([
            ["Server", "Address", "U", "State", "Group", "Version"],
            [
                "db-1",
                "C0-R0-R0-S0-S0",
                "U1",
                "updating",
                "storage",
                "1.4.0"
            ],
            ["web-1", "C0-R0-R0-S1-S0", "U41", "healthy", "web", "1.4.0"],
        ])
    );
    assert_eq!(out["big"], json!(["rack big"]));

    let ui = base();
    ui.reply(
        &m("Layout/Get"),
        json!({
            "rows": [{"id": "row-1", "number": 0}],
            "racks": [{"id": "rack-1", "row": "row-1", "number": 0, "heightU": 10}],
            "sleds": [{"id": "sled-1", "rack": "rack-1", "u": 5, "heightU": 1, "slots": 1,
                       "address": "C0-R0-R0-S0"}],
            "placements": [{"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "new-1"},
                            "rack": "rack-1", "u": 5, "heightU": 1, "sled": "sled-1",
                            "address": "C0-R0-R0-S0-S0"}],
        }),
    );
    let mut ui = ui;
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
await click(tree, "open-server", 0);
return seen.navigations;"#,
    );
    assert_eq!(
        out,
        json!([{"name": "inventory", "params": {"record": "new-1"}}])
    );
}

#[test]
fn an_empty_layout_says_so_and_offers_a_first_row() {
    // Catches: an empty lab that shows a blank page, or whose first row needs the edit mode first.
    let mut ui = base();
    ui.reply(&m("Layout/Get"), json!({}));
    writes(&ui);
    let out = home(
        &mut ui,
        "{}",
        r#"
const first = words(cls(tree, "empty")[0]);
await click(tree, "add-row");
return { first, title: words(nodes(tree).find((n) => n.tag === "h2")), reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert!(
        out["first"]
            .as_str()
            .unwrap()
            .starts_with("No lab layout yet.")
    );
    assert_eq!(out["title"], "Lab");
    assert_eq!(out["refreshes"], 1);
    assert_eq!(out["reports"], json!(["A row is added."]));
    assert!(bodies(&ui).contains(&(m("Layout/AddRow"), json!({"reason": "add row"}))));
}

#[test]
fn a_row_or_a_rack_that_is_gone_says_so_and_a_failed_layout_leaves_the_hosts() {
    // Catches: a page that fails blank on an id the layout no longer holds, and one that hides the
    // hosts when the layout cannot be read.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ rack: "rack-9" }"#,
        r#"
await click(tree, "to-lab");
return { text: words(tree), navigations: seen.navigations };"#,
    );
    assert!(
        out["text"]
            .as_str()
            .unwrap()
            .contains("not in the lab any more")
    );
    assert_eq!(out["navigations"], json!([{"name": "home", "params": {}}]));

    let mut ui = base();
    ui.refuse(&m("Layout/Get"), 503, "UNAVAILABLE", "store_failed");
    let out = home(&mut ui, "{}", "return words(tree);");
    let text = out.as_str().unwrap();
    assert!(
        text.contains("The lab could not be read: UNAVAILABLE store_failed"),
        "{text}"
    );
    assert!(text.contains("No alerts"), "{text}");
}

#[test]
fn the_edit_mode_adds_a_row_a_rack_and_names_the_cluster() {
    // Catches: an edit control that sends the wrong call or body, a rack added at no height, and a
    // name that is not trimmed or is sent empty.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ edit: true }"#,
        r#"
type(tree, "cluster-name", "  Farm 2 ");
await click(tree, "save-name");
type(tree, "cluster-name", "   ");
await click(tree, "save-name");
await click(tree, "add-row");
return { reports: seen.reports, refreshes: seen.refreshes, toggle: named(tree, "edit-layout")[0].attrs["aria-pressed"] };"#,
    );
    assert_eq!(out["toggle"], "true");
    assert_eq!(
        out["reports"],
        json!([
            "The cluster is called Farm 2.",
            "Give the cluster a name.",
            "A row is added."
        ])
    );
    assert_eq!(out["refreshes"], 2);
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/PutName"),
        json!({"name": "Farm 2", "reason": "rename"})
    )));
    assert!(sent.contains(&(m("Layout/AddRow"), json!({"reason": "add row"}))));

    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-2", edit: true }"#,
        r#"
const before = named(tree, "new-rack-height")[0].children.map((o) => o.attrs.value);
await click(tree, "add-rack");
choose(tree, "new-rack-height", "48");
await click(tree, "add-rack");
return { before, reports: seen.reports };"#,
    );
    assert_eq!(out["before"][0], "42", "42 U is the default");
    assert_eq!(
        out["reports"],
        json!(["A rack is added.", "A rack is added."])
    );
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/PutRack"),
        json!({"row": "row-2", "heightU": 42, "reason": "add rack"})
    )));
    assert!(sent.contains(&(
        m("Layout/PutRack"),
        json!({"row": "row-2", "heightU": 48, "reason": "add rack"})
    )));
}

#[test]
fn a_row_or_a_rack_that_holds_servers_is_not_removed_and_names_them() {
    // Catches: a removal that is sent for a row with racks or a rack with servers, a refusal that
    // does not name the servers, and an empty rack or row that cannot be removed.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ edit: true }"#,
        r#"
await click(tree, "remove-row", 0);
await click(tree, "remove-row", 1);
return seen.reports;"#,
    );
    assert_eq!(
        out,
        json!([
            "Row 0 still holds 2 racks with web-1, db-1: remove the racks first.",
            "Row 1 still holds 1 racks: remove the racks first."
        ])
    );
    assert!(!ui.methods().contains(&m("Layout/RemoveRow")));

    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "remove-rack", 0);
await click(tree, "remove-rack", 1);
return { reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(
        out["reports"],
        json!([
            "Row 0 / Rack 0 still holds 2 sleds with web-1, db-1: remove the sleds first.",
            "Row 0 / Rack 1 is removed."
        ])
    );
    assert_eq!(out["refreshes"], 1);
    assert!(bodies(&ui).contains(&(
        m("Layout/RemoveRack"),
        json!({"id": "rack-2", "reason": "remove"})
    )));

    // A removed row the coordinator refuses anyway (a rack was added since the page was read).
    let mut ui = base();
    ui.reply(
        &m("Layout/Get"),
        json!({"rows": [{"id": "row-1", "number": 0}]}),
    );
    ui.refuse(&m("Layout/RemoveRow"), 412, "FAILED_PRECONDITION", "in_use");
    let out = home(
        &mut ui,
        r#"{ edit: true }"#,
        r#"
await click(tree, "remove-row", 0);
return seen.reports;"#,
    );
    assert_eq!(
        out,
        json!(["It still holds something: take what stands on it away first."])
    );
}

#[test]
fn a_rack_is_resized_and_moved_to_another_row_from_its_head() {
    // Catches: a resize that drops the row or a move that drops the height, either of which the
    // coordinator would refuse or apply wrongly.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
choose(tree, "rack-height", "48", 1);
await click(tree, "resize-rack", 1);
choose(tree, "rack-row", "row-2", 1);
await click(tree, "move-rack", 1);
return seen.reports;"#,
    );
    assert_eq!(out, json!(["Row 0 / Rack 1 is now 48U.", "Rack moved."]));
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/PutRack"),
        json!({"id": "rack-2", "row": "row-1", "heightU": 48, "reason": "resize"})
    )));
    assert!(sent.contains(&(
        m("Layout/PutRack"),
        json!({"id": "rack-2", "row": "row-2", "heightU": 24, "reason": "move"})
    )));
}

#[test]
fn the_tray_opens_a_host_outside_the_edit_mode_and_the_inventory_for_an_entry() {
    // Catches: a tray chip that does nothing, or one that opens a host page for an entry no host
    // is enrolled against.
    let mut ui = start();
    let out = home(
        &mut ui,
        "{}",
        r#"
await click(tree, "open-unplaced", 0);
await click(tree, "open-unplaced", 2);
return seen.navigations;"#,
    );
    assert_eq!(
        out,
        json!([
            {"name": "host", "params": {"nodeId": "web-2"}},
            {"name": "inventory", "params": {"record": "new-1"}}
        ])
    );
}

#[test]
fn a_placement_whose_server_is_gone_is_shown_offline_and_opens_nothing() {
    // Catches: a placement left in a rack that nothing in the lab explains, invisible to the page.
    let mut ui = base();
    ui.reply(
        &m("Layout/Get"),
        json!({
            "rows": [{"id": "row-1", "number": 0}],
            "racks": [{"id": "rack-1", "row": "row-1", "number": 0, "heightU": 10}],
            "sleds": [{"id": "sled-1", "rack": "rack-1", "u": 3, "heightU": 1, "slots": 1,
                       "address": "C0-R0-R0-S0"}],
            "placements": [{"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "ghost"},
                            "rack": "rack-1", "u": 3, "heightU": 1, "sled": "sled-1",
                            "address": "C0-R0-R0-S0-S0"}],
        }),
    );
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
await click(tree, "open-server", 0);
return { title: named(tree, "open-server")[0].attrs.title, navigations: seen.navigations };"#,
    );
    assert_eq!(out["title"], "ghost | offline | C0-R0-R0-S0-S0");
    assert_eq!(out["navigations"], json!([]));
}

#[test]
fn the_elevation_is_mounted_with_custom_properties_and_never_a_style_attribute() {
    // Catches: a position handed to the stylesheet as a `style` attribute, which the page's content
    // security policy refuses (and the scan of the sources forbids), a custom property set that is
    // not a number or has a name outside the pattern, and an element that is built without them.
    let mut ui = start();
    let out = ui.run(
        r#"
import { api, recorder, mounted, elements } from "../test/kit.js";
import { h } from "./view.js";
import { render } from "./pages/home.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), { row: "row-1" }, []);
  const tree = await render(ctx);
  const doc = mounted(tree);
  const all = elements(doc.root);
  const plates = all.filter((e) => (e.attributes["class"] || "").startsWith("server "));
  const styled = all.filter((e) => "style" in e.attributes).length;
  const first = plates[0].style.custom;
  const refused = [];
  for (const [name, value] of [["--r", "x"], ["r", 1], ["--r;color", 1], ["--r", Infinity]]) {
    try {
      mounted(h("div", { vars: { [name]: value } }));
      refused.push(false);
    } catch (error) {
      refused.push(true);
    }
  }
  return { plates: plates.length, styled, first, refused };
}"#,
    );
    assert_eq!(out["plates"], 2);
    assert_eq!(out["styled"], 0);
    assert_eq!(out["first"], json!({"--r": "1", "--s": "2"}));
    assert_eq!(out["refused"], json!([true, true, true, true]));
}

#[test]
fn the_state_pills_and_chips_tell_offline_from_failed_and_pending() {
    // Proves: failed is the `bad` (red) pill, offline the `muted` (grey) one and pending the `warn`
    // (amber) one, in the count strip of the floor plan and in the tray chips' own state class.
    // Catches: an offline server drawn in the failed red, which reads as a fault when the server is
    // only silent.
    let mut ui = start();
    let out = home(
        &mut ui,
        "{}",
        r#"
const tone = (n) => (n.attrs.class || "").split(" ").filter((c) => c !== "pill")[0];
return {
  pills: cls(tree, "pill").map((n) => [words(n).split(" ")[0], tone(n)]),
  chips: cls(tree, "chip").map((n) => n.attrs.class.split(" ")[1]),
};"#,
    );
    assert_eq!(
        out["pills"],
        json!([
            ["healthy", "ok"],
            ["updating", "info"],
            ["failed", "bad"],
            ["offline", "muted"],
            ["pending", "warn"]
        ])
    );
    assert_eq!(out["chips"], json!(["s-healthy", "s-offline", "s-pending"]));
}

#[test]
fn the_strip_arrows_follow_the_width_on_load_on_resize_and_on_scroll() {
    // Proves: the strip brings its two arrows and its position up to date when it is mounted and
    // when the window is resized, not only when it is scrolled: next is disabled when every rack
    // fits (or the strip is at its right end), previous when it is at the left end, and both are
    // enabled in between; the number says which rack is first in view.
    // Catches: a next arrow that stays enabled on a row whose racks all fit until the first scroll,
    // and one that stays disabled after a narrower window makes a rack run off the right edge.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
const strip = cls(tree, "rack-strip")[0];
const prev = { disabled: false };
const next = { disabled: false };
const pos = { textContent: "" };
const classes = new Set();
const frame = {
  querySelector: (q) => ({ ".strip-pos": pos, "[name=strip-prev]": prev, "[name=strip-next]": next })[q],
  classList: { toggle: (c, on) => (on ? classes.add(c) : classes.delete(c)) },
};
const element = { parentNode: frame, scrollLeft: 0, scrollWidth: 700, clientWidth: 800, firstElementChild: { offsetWidth: 300 } };
const state = () => ({ prev: prev.disabled, next: next.disabled, pos: pos.textContent, classes: [...classes].sort() });
const steps = {};
strip.on.mounted({ currentTarget: element });
steps.mounted = state();
element.clientWidth = 400;
strip.on.resize({ currentTarget: element });
steps.narrow = state();
element.scrollLeft = 310;
strip.on.scroll({ currentTarget: element });
steps.scrolled = state();
element.clientWidth = 1000;
element.scrollLeft = 0;
strip.on.resize({ currentTarget: element });
steps.wide = state();
return steps;"#,
    );
    assert_eq!(
        out["mounted"],
        json!({"prev": true, "next": true, "pos": "Rack 0 of 2", "classes": ["at-end", "at-start"]}),
        "every rack fits: no arrow can move"
    );
    assert_eq!(
        out["narrow"],
        json!({"prev": true, "next": false, "pos": "Rack 0 of 2", "classes": ["at-start"]}),
        "a narrower window: a rack runs off the right edge"
    );
    assert_eq!(
        out["scrolled"],
        json!({"prev": false, "next": true, "pos": "Rack 1 of 2", "classes": ["at-end"]}),
        "at the right end: next is disabled, previous enabled"
    );
    assert_eq!(
        out["wide"],
        json!({"prev": true, "next": true, "pos": "Rack 0 of 2", "classes": ["at-end", "at-start"]})
    );
}

#[test]
fn a_mounted_tree_runs_its_mounted_hook_and_keeps_one_resize_listener_per_view() {
    // Proves: `mount` calls a `mounted` handler once the elements are in the document, with the
    // element as `currentTarget`; a `resize` handler is attached to the window (not the element)
    // and calls its handler with the element; a second mount takes the first one's listener off.
    // Catches: a resize listener left on the window for every redraw, or a hook run before its
    // element is in the page (where the layout is unknown).
    let mut ui = start();
    let out = ui.run(
        r#"
import { fakeDocument, fire } from "../test/kit.js";
import { h, mount } from "./view.js";
export async function main() {
  const doc = fakeDocument();
  const calls = [];
  const tree = (name) => h("div", { class: name, on: {
    mounted: (e) => calls.push(["mounted", name, doc.root.children.includes(e.currentTarget)]),
    resize: (e) => calls.push(["resize", name, e.currentTarget.attributes.class]),
  } });
  mount(doc, doc.root, tree("one"));
  const after_first = doc.defaultView.listeners.resize.length;
  for (const listener of doc.defaultView.listeners.resize) { listener({}); }
  mount(doc, doc.root, tree("two"));
  const after_second = doc.defaultView.listeners.resize.length;
  for (const listener of doc.defaultView.listeners.resize) { listener({}); }
  return { calls, after_first, after_second, own: doc.created.map((e) => Object.keys(e.listeners)) };
}"#,
    );
    assert_eq!(
        out["calls"],
        json!([
            ["mounted", "one", true],
            ["resize", "one", "one"],
            ["mounted", "two", true],
            ["resize", "two", "two"]
        ])
    );
    assert_eq!(out["after_first"], 1);
    assert_eq!(out["after_second"], 1);
    assert_eq!(
        out["own"],
        json!([[], [], []]),
        "the hooks are no element events"
    );
}
