//! The Home page, the lab: the floor plan of rows, the racks of a row as front elevations, one rack
//! with its hosts, the breadcrumbs, the strip's scrolling, and the edit mode's calls and refusals,
//! each run in the JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// The lab of the tests: the cluster `Lab 1`, row 1 with a rack of 42 U and one of 24 U, row 2 with
/// a rack of 42 U. `web-1` (a host no entry matches) stands at U 41 and 42, `db-1` (an inventory
/// entry a host is enrolled against) at U 1.
fn layout() -> Value {
    json!({
        "name": "Lab 1",
        "rows": [{"id": "row-1", "number": 1}, {"id": "row-2", "number": 2}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 1, "heightU": 42},
            {"id": "rack-2", "row": "row-1", "number": 2, "heightU": 24},
            {"id": "rack-3", "row": "row-2", "number": 1, "heightU": 42},
        ],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-1"},
             "rack": "rack-1", "u": 41, "heightU": 2},
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
             "rack": "rack-1", "u": 1, "heightU": 1},
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
  crumbs: words(cls(tree, "crumbs")[0]),
};"#,
    );
    assert_eq!(out["title"], "Lab 1");
    assert_eq!(out["crumbs"], "Lab");
    let rows = out["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert!(rows[0].as_str().unwrap().starts_with("Row 1"), "{rows:?}");
    assert!(rows[0].as_str().unwrap().ends_with("2 servers"), "{rows:?}");
    assert!(rows[1].as_str().unwrap().starts_with("Row 2"), "{rows:?}");
    assert!(rows[1].as_str().unwrap().ends_with("0 servers"), "{rows:?}");
    assert_eq!(
        out["feet"],
        json!([
            "Row 1 / Rack 1: 3/42 U",
            "Row 1 / Rack 2: 0/24 U",
            "Row 2 / Rack 1: 0/42 U"
        ])
    );
    assert_eq!(
        out["pills"],
        json!(["healthy 2", "updating 1", "offline 1", "pending 1"])
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
    assert_eq!(out["trail"], "Lab Row 1 Rack 2");
    assert_eq!(out["title"], "Row 1 / Rack 2");
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
    assert_eq!(out["racks"], json!(["Row 1 / Rack 1", "Row 1 / Rack 2"]));
    assert_eq!(out["units"], 66);
    assert_eq!(
        out["servers"],
        json!([
            {"title": "web-1 | group web | version 1.4.0 | healthy", "r": "1", "s": "2",
             "class": "server s-healthy tall"},
            {"title": "db-1 | group storage | version 1.4.0 | updating", "r": "42", "s": "1",
             "class": "server s-updating"},
        ])
    );
    assert_eq!(
        out["blanks"],
        json!([["3", "39"], ["1", "24"]]),
        "a free run is one panel, over the U it covers"
    );
    assert_eq!(out["heads"], json!(["Rack 1 3/42 U", "Rack 2 0/24 U"]));
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
            ["Server", "U", "Height", "State", "Group", "Version"],
            ["db-1", "U1", "1U", "updating", "storage", "1.4.0"],
            ["web-1", "U41", "2U", "healthy", "web", "1.4.0"],
        ])
    );
    assert_eq!(out["big"], json!(["rack big"]));

    let ui = base();
    ui.reply(
        &m("Layout/Get"),
        json!({
            "rows": [{"id": "row-1", "number": 1}],
            "racks": [{"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10}],
            "placements": [{"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "new-1"},
                            "rack": "rack-1", "u": 5, "heightU": 1}],
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
    assert_eq!(out["reports"], json!(["Row 1 is added."]));
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
    assert!(text.contains("Hosts (4)"), "{text}");
    assert!(text.contains("Alerts (0)"), "{text}");
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
            "Row 1 still holds 2 racks with web-1, db-1: remove the racks first.",
            "Row 2 still holds 1 racks: remove the racks first."
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
            "Row 1 / Rack 1 still holds web-1, db-1: move them out first.",
            "Row 1 / Rack 2 is removed."
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
        json!({"rows": [{"id": "row-1", "number": 1}]}),
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
        json!(["It still holds something: remove what stands in it first."])
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
    assert_eq!(out, json!(["Row 1 / Rack 2 is now 48U.", "Rack moved."]));
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
fn an_unplaced_server_is_picked_and_put_on_a_free_u_by_click_and_by_drag() {
    // Catches: a pick that is lost, a slot that places at another U, a height that is not the one
    // chosen, a drop that places nothing, and a drag over a slot that is not allowed.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const slotAt = (t, u) => cls(t, "slot").find((n) => n.attrs.title.endsWith(`, U${u}`));
await click(tree, "pick-unplaced", 0);
let now = last();
const banner = words(cls(now, "moving")[0]);
choose(now, "tray-height", "2");
await slotAt(now, 20).on.click();
let prevented = 0;
const picked = named(now, "pick-server")[0];
picked.on.dragstart({});
slotAt(now, 5).on.dragover({ preventDefault: () => { prevented += 1; } });
await slotAt(now, 5).on.drop({ preventDefault: () => { prevented += 1; } });
return { banner, reports: seen.reports, prevented, slots: cls(now, "slot").length };"#,
    );
    assert_eq!(
        out["banner"],
        "web-2 is not placed: click a free U of a rack. Cancel"
    );
    assert_eq!(
        out["reports"],
        json!([
            "web-2 is at Row 1 / Rack 1, U20.",
            "web-1 is at Row 1 / Rack 1, U5."
        ])
    );
    assert_eq!(out["prevented"], 2);
    // 42 + 24 U in the two racks, less the 3 U of web-1 and db-1.
    assert_eq!(out["slots"], 63);
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"},
               "rack": "rack-1", "heightU": 2, "u": 20, "reason": "placed web-2"})
    )));
    assert!(sent.contains(&(
        m("Layout/Place"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-1"},
               "rack": "rack-1", "heightU": 2, "u": 5, "reason": "placed web-1"})
    )));
}

#[test]
fn a_placed_server_is_taken_out_or_the_move_is_cancelled_and_a_refusal_is_told_in_words() {
    // Catches: an unplace that names the wrong subject, a cancel that leaves the server picked, a
    // placement on a taken U that is not told as such, and a click on a slot with nothing picked.
    let mut ui = base();
    ui.reply(&m("Layout/Get"), layout());
    ui.refuse(&m("Layout/Place"), 412, "FAILED_PRECONDITION", "overlap");
    ui.reply(&m("Layout/Unplace"), json!({}));
    let out = home(
        &mut ui,
        r#"{ rack: "rack-1", edit: true }"#,
        r#"
const slotAt = (t, u) => cls(t, "slot").find((n) => n.attrs.title.endsWith(`, U${u}`));
await slotAt(tree, 10).on.click();
await click(tree, "pick-server", 1);
let now = last();
const banner = words(cls(now, "moving")[0]);
await click(now, "cancel-move");
now = last();
const cancelled = cls(now, "moving").length;
await click(now, "pick-server", 1);
now = last();
await slotAt(now, 10).on.click();
await click(now, "unplace");
return { banner, cancelled, reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(
        out["banner"],
        "db-1 is at Row 1 / Rack 1, U1: click a free U to move it. Cancel Take out of its rack"
    );
    assert_eq!(out["cancelled"], 0);
    assert_eq!(
        out["reports"],
        json!([
            "Another server already stands in one of those U.",
            "db-1 is unplaced."
        ])
    );
    assert_eq!(out["refreshes"], 1);
    let sent = bodies(&ui);
    assert!(sent.contains(&(
        m("Layout/Unplace"),
        json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "db-1"},
               "reason": "unplaced db-1"})
    )));
    assert_eq!(
        sent.iter()
            .filter(|(method, _)| *method == m("Layout/Place"))
            .count(),
        1,
        "a click with nothing picked sends nothing"
    );
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
fn a_placement_whose_server_is_gone_is_shown_offline_and_can_be_taken_out() {
    // Catches: a placement left in a rack that nothing in the lab explains, invisible to the page.
    let mut ui = base();
    ui.reply(
        &m("Layout/Get"),
        json!({
            "rows": [{"id": "row-1", "number": 1}],
            "racks": [{"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10}],
            "placements": [{"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "ghost"},
                            "rack": "rack-1", "u": 3, "heightU": 1}],
        }),
    );
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
await click(tree, "open-server", 0);
return { title: named(tree, "open-server")[0].attrs.title, navigations: seen.navigations };"#,
    );
    assert_eq!(out["title"], "ghost | offline");
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
  const first = plates[0].style.properties;
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
