//! Network gear in the web app (docs/design/network-gear.md): a switch drawn in a rack as a faceplate
//! with no light, the Inventory page's Server | Switch kind and its switch form (what it sends and
//! what it says of a field that is wrong), and the switch page. Each runs in the JavaScript engine
//! against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// A lab of one row, one rack of 27 U and the two switches of the first rack: `bmc-switch` (BMC
/// network, console on `node-1`, `/dev/ttyUSB0`, a management MAC, a note) at U 27 and `data-switch`
/// (data network, no console) at U 26, beside the server `node-1` at U 22 to 25.
fn layout() -> Value {
    json!({
        "name": "Lab 1",
        "rows": [{"id": "row-1", "number": 1}],
        "racks": [{"id": "rack-1", "row": "row-1", "number": 1, "heightU": 27}],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.1"},
             "rack": "rack-1", "u": 27, "heightU": 1},
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.2"},
             "rack": "rack-1", "u": 26, "heightU": 1},
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "node-1"},
             "rack": "rack-1", "u": 22, "heightU": 4},
        ],
        "devices": [
            {"id": "net.1", "name": "bmc-switch", "role": "NETWORK_ROLE_BMC",
             "consoleHost": "node-1", "consoleDevice": "/dev/ttyUSB0",
             "managementMac": "3c:ec:ef:aa:00:01", "notes": "uplink"},
            {"id": "net.2", "name": "data-switch", "role": "NETWORK_ROLE_DATA"},
        ],
    })
}

fn hosts() -> Value {
    json!({"hosts": [
        {"nodeId": "node-1", "role": "build", "live": true, "lastHeartbeatUnix": "1800000000",
         "inventoryRecordId": "node-1",
         "services": [{"service": "hostkeeper", "installedVersion": "1.4.0", "health": "HEALTH_HEALTHY"}]},
        {"nodeId": "node-2", "role": "build", "live": true, "lastHeartbeatUnix": "1800000000"},
    ]})
}

fn start() -> Ui {
    let ui = Ui::start();
    for _ in 0..3 {
        ui.reply(&m("Layout/Get"), layout());
        ui.reply(&m("Hosts/List"), hosts());
    }
    ui
}

const HELPERS: &str = r#"
const cls = (tree, c) => nodes(tree).filter((n) => (n.attrs.class || "").split(" ").includes(c));
const words = (n) => text(n).replace(/\s+/g, " ").trim();
"#;

fn run(ui: &mut Ui, page: &str, params: &str, answers: &str, then: &str) -> Value {
    ui.run(&page_driver(
        page,
        params,
        answers,
        &format!("{HELPERS}{then}"),
    ))
}

#[test]
fn a_switch_is_a_faceplate_with_no_light_that_opens_its_page() {
    // Proves: in a rack a switch is a plate of its own (class gear, its network in its class), with
    // a tag for the network, its name, a strip of ports and a console jack when a console is set, and
    // no light and no state word; hover says name, network and console; a click opens the switch
    // page; the rack's table lists it under Network with its U and console; the summary counts it as
    // a switch and no server count or state includes it.
    // Catches: a switch drawn as a server (a green light, a state), counted as a server, or listed
    // as a server that is gone because its placement has no host.
    let mut ui = start();
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(
        &m("Enrollment/ListRecords"),
        json!({"records": [{"hostId": "node-1", "enrolled": true}]}),
    );
    let out = run(
        &mut ui,
        "home",
        r#"{ rack: "rack-1" }"#,
        "[]",
        r#"
const plates = cls(tree, "gear");
await click(tree, "open-gear", 0);
return {
  count: plates.length,
  classes: plates.map((n) => n.attrs.class),
  titles: plates.map((n) => n.attrs.title),
  words: plates.map(words),
  lights: plates.map((n) => nodes(n).filter((c) => (c.attrs.class || "").split(" ").includes("led")).length),
  ports: plates.map((n) => cls(n, "port").length),
  jacks: plates.map((n) => cls(n, "jack").length),
  servers: cls(tree, "server").length,
  network: rows(tree).filter((r) => r[0] === "bmc-switch" || r[0] === "data-switch"),
  pills: cls(tree, "pill").map(words),
  tray: cls(tree, "chip").length,
};"#,
    );
    let o = &out["out"];
    assert_eq!(o["count"], 2);
    assert_eq!(o["servers"], 1);
    assert_eq!(o["classes"], json!(["gear r-bmc", "gear r-data"]));
    assert_eq!(
        o["titles"],
        json!([
            "bmc-switch | BMC network | console ttyUSB0 on node-1",
            "data-switch | Data network | console none"
        ])
    );
    assert_eq!(o["words"], json!(["BMC bmc-switch", "DATA data-switch"]));
    assert_eq!(o["lights"], json!([0, 0]));
    assert_eq!(o["ports"], json!([12, 12]));
    assert_eq!(o["jacks"], json!([1, 0]));
    assert_eq!(
        o["network"],
        json!([
            ["data-switch", "U26", "1U", "Data network", "none"],
            [
                "bmc-switch",
                "U27",
                "1U",
                "BMC network",
                "ttyUSB0 on node-1"
            ]
        ])
    );
    assert_eq!(
        o["tray"], 1,
        "only the unplaced server is in the tray, no placed switch"
    );
    assert_eq!(
        o["pills"].as_array().map(Vec::len),
        Some(1),
        "one server, one state: {}",
        o["pills"]
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "switch", "params": {"id": "net.1"}}])
    );
}

#[test]
fn the_summary_counts_switches_apart_and_an_unplaced_switch_waits_in_the_tray() {
    // Proves: the summary says how many switches there are after the servers; a switch with no
    // placement is a chip in the Unplaced tray (not a server, no light, with its network) that opens
    // its page; the floor plan's U in use counts a switch's U.
    // Catches: a switch missing from the tray, or one that counts as an unplaced server.
    let mut ui = Ui::start();
    let mut lab = layout();
    lab["placements"]
        .as_array_mut()
        .expect("placements")
        .remove(1);
    ui.reply(&m("Layout/Get"), lab);
    ui.reply(&m("Hosts/List"), hosts());
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(
        &m("Enrollment/ListRecords"),
        json!({"records": [{"hostId": "node-1", "enrolled": true}]}),
    );
    let out = run(
        &mut ui,
        "home",
        "{}",
        "[]",
        r#"
await click(tree, "open-unplaced", 1);
return {
  summary: words(cls(tree, "lab-sum")[0]),
  chips: cls(tree, "chip").map((n) => [n.attrs.class, n.attrs.title, words(n)]),
  row: words(named(tree, "open-row")[0]),
};"#,
    );
    let o = &out["out"];
    assert_eq!(
        o["summary"],
        "2 servers: 1 placed in 1 racks, 1 unplaced; 2 switches"
    );
    assert_eq!(
        o["chips"],
        json!([
            ["chip s-healthy", "node-2 | healthy", "node-2 healthy"],
            [
                "chip s-gear",
                "data-switch | Data network",
                "data-switch Data network"
            ]
        ])
    );
    assert!(
        o["row"].as_str().expect("row").contains("1 servers"),
        "{}",
        o["row"]
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "switch", "params": {"id": "net.2"}}])
    );
}

#[test]
fn inventory_has_a_kind_switch_and_the_server_kind_is_the_servers_page() {
    // Proves: the Inventory page of the app shows Server and Switch, the chosen one pressed; with no
    // kind it is the page of the servers (its four-field table, no call for the layout); a click on
    // Switch goes to the same page with kind switch, and on Server back to it with no kind.
    // Catches: a server entry that gained a field or a call, or a kind that is not kept in the page's
    // parameters.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/ListRecords"),
        json!({"records": [{"hostId": "db-1", "dataMacs": ["0c:c4:7a:00:00:02"]}]}),
    );
    let out = run(
        &mut ui,
        "inventory_kinds",
        "{}",
        "[]",
        r#"
const pressed = () => nodes(tree).filter((n) => n.tag === "button" && n.attrs["aria-pressed"] === "true").map((n) => n.attrs.name);
const before = pressed();
await click(tree, "kind-switch");
await click(tree, "kind-server");
return { before, rows: rows(tree)[0], titles: nodes(tree).filter((n) => n.tag === "h2").map(words) };"#,
    );
    assert_eq!(out["out"]["before"], json!(["kind-server"]));
    assert_eq!(
        out["out"]["rows"],
        json!(["Server name", "BMC MAC", "Data MAC", "Notes", ""])
    );
    assert_eq!(out["out"]["titles"], json!(["Inventory"]));
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "inventory", "params": {"kind": "switch"}},
            {"name": "inventory", "params": {}}
        ])
    );
    assert_eq!(
        bodies(&ui).len(),
        1,
        "the servers page asks for the records only"
    );
}

#[test]
fn the_switch_kind_lists_each_switch_with_its_fields_and_where_it_stands() {
    // Proves: the Switch kind is one table of the switches (Name, Network, Console, Management MAC,
    // Notes, Where) with Open, Edit and Remove on each, and the form to add one under it; Open goes
    // to the switch page and Edit to this page with the switch's id as `edit`, which fills the form
    // and the placement choices with what is kept.
    // Catches: a field of a switch that the table or the edit form leaves out.
    let mut ui = start();
    let out = run(
        &mut ui,
        "inventory_kinds",
        r#"{ kind: "switch" }"#,
        "[]",
        r#"
await click(tree, "open-switch", 1);
await click(tree, "edit-switch", 0);
return { rows: rows(tree).slice(0, 3), pressed: nodes(tree).filter((n) => n.tag === "button" && n.attrs["aria-pressed"] === "true").map((n) => n.attrs.name) };"#,
    );
    assert_eq!(
        out["out"]["rows"],
        json!([
            [
                "Name",
                "Network",
                "Console",
                "Management MAC",
                "Notes",
                "Where",
                ""
            ],
            [
                "bmc-switch",
                "BMC network",
                "ttyUSB0 on node-1",
                "3c:ec:ef:aa:00:01",
                "uplink",
                "Row 1 / Rack 1, U27",
                "Open Edit Remove"
            ],
            [
                "data-switch",
                "Data network",
                "none",
                "-",
                "-",
                "Row 1 / Rack 1, U26",
                "Open Edit Remove"
            ]
        ])
    );
    assert_eq!(out["out"]["pressed"], json!(["kind-switch"]));
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "switch", "params": {"id": "net.2"}},
            {"name": "inventory", "params": {"kind": "switch", "edit": "net.1"}}
        ])
    );

    let edit = run(
        &mut ui,
        "inventory_switch",
        r#"{ kind: "switch", edit: "net.1" }"#,
        "[]",
        r#"
const h3 = nodes(tree).filter((n) => n.tag === "h3").map(words);
return {
  h3,
  name: named(tree, "switch-name")[0].attrs.value,
  role: chosen(tree, "switch-role"),
  host: chosen(tree, "switch-console-host"),
  device: chosen(tree, "switch-console-device"),
  mac: named(tree, "switch-mac")[0].attrs.value,
  hosts: choices(tree, "switch-console-host"),
  rack: chosen(tree, "place-rack"),
  u: chosen(tree, "place-u"),
  row: chosen(tree, "place-row"),
};"#,
    );
    assert_eq!(
        edit["out"],
        json!({
            "h3": ["Edit bmc-switch"], "name": "bmc-switch", "role": "NETWORK_ROLE_BMC",
            "host": "node-1", "device": "/dev/ttyUSB0", "mac": "3c:ec:ef:aa:00:01",
            "hosts": ["none", "node-1", "node-2"], "rack": "rack-1", "u": "27", "row": "row-1"
        })
    );
}

#[test]
fn a_switch_is_added_and_placed_and_a_wrong_field_sends_nothing() {
    // Proves: Add with a name that is wrong, no device for a console host, and a MAC that is none
    // sends nothing and marks each field; with the fields right it sends PutDevice (the name
    // trimmed, the MAC in its one spelling, the console as chosen, the reason) and then Place under
    // the inventory subject of the id the coordinator made, at the row, rack and U chosen, 1U high;
    // with Unplaced left it sends no Place.
    // Catches: a request that carries a field as typed, a switch placed under a guessed id, or a
    // placement made for a switch the operator left unplaced.
    let mut ui = start();
    ui.reply(
        &m("Layout/PutDevice"),
        json!({"device": {"id": "net.3", "name": "spare"}}),
    );
    ui.reply(&m("Layout/PutDevice"), json!({"device": {"id": "net.4"}}));
    ui.reply(&m("Layout/Place"), json!({}));
    let out = run(
        &mut ui,
        "inventory_switch",
        r#"{ kind: "switch" }"#,
        "[]",
        r#"
type(tree, "switch-name", "bad/name");
choose(tree, "switch-console-host", "node-2");
choose(tree, "switch-console-device", "/dev/ttyUSB9");
type(tree, "switch-mac", "zz");
type(tree, "switch-notes", "a\u0007b");
await click(tree, "save-switch");
const first = nodes(tree).filter((n) => n.attrs.role === "alert").map(words).filter((w) => w !== "");
type(tree, "switch-name", "  spare  ");
choose(tree, "switch-role", "NETWORK_ROLE_DATA");
choose(tree, "switch-console-device", "/dev/ttyS1");
type(tree, "switch-mac", "AA-BB-CC-DD-EE-0F");
type(tree, "switch-notes", "spare\nswitch");
choose(tree, "place-row", "row-1");
choose(tree, "place-u", "20");
await click(tree, "save-switch");
type(tree, "switch-name", "plain");
choose(tree, "place-row", "");
await click(tree, "save-switch");
return { first };"#,
    );
    let first = out["out"]["first"].as_array().expect("marks");
    assert_eq!(first.len(), 4, "{first:?}");
    assert_eq!(
        first[0],
        "A name is 1 to 32 of letters, digits, space, . _ and -."
    );
    assert_eq!(
        first[1],
        "Choose the device the console cable is plugged into."
    );
    assert!(
        first[2]
            .as_str()
            .expect("mac")
            .starts_with("zz is not a MAC")
    );
    assert_eq!(
        first[3],
        "Notes are at most 256 bytes, with no control character but the line break."
    );
    let sent: Vec<_> = bodies(&ui).into_iter().skip(2).collect();
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(
        sent[0],
        (
            m("Layout/PutDevice"),
            json!({"id": "", "name": "spare", "role": "NETWORK_ROLE_DATA",
                   "consoleHost": "node-2", "consoleDevice": "/dev/ttyS1",
                   "managementMac": "aa:bb:cc:dd:ee:0f", "notes": "spare\nswitch",
                   "reason": "saved on the Inventory page"})
        )
    );
    assert_eq!(
        sent[1],
        (
            m("Layout/Place"),
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.3"},
                   "rack": "rack-1", "heightU": 1, "u": 20, "reason": "placed spare"})
        )
    );
    assert_eq!(sent[2].0, m("Layout/PutDevice"));
    assert_eq!(out["seen"]["refreshes"], 2);
    assert_eq!(
        out["seen"]["reports"].as_array().map(Vec::len),
        Some(3),
        "{}",
        out["seen"]["reports"]
    );
}

#[test]
fn saving_a_switch_keeps_or_moves_or_clears_its_place_and_a_refusal_is_told_in_words() {
    // Proves: Save of an edit sends PutDevice with the device's id; choosing Unplaced sends Unplace
    // for it; a refused Place is told after the save ("It could not be placed: ..."); a refused
    // PutDevice (a name taken) is told in words and sends no Place; Remove asks first and sends
    // RemoveDevice for that switch only on a yes.
    // Catches: an edit that creates a second switch, a refusal shown as a bare token, a remove with
    // no question.
    let mut ui = start();
    ui.reply(&m("Layout/PutDevice"), json!({"device": {"id": "net.1"}}));
    ui.refuse(&m("Layout/PutDevice"), 409, "ALREADY_EXISTS", "name_taken");
    ui.reply(&m("Layout/PutDevice"), json!({"device": {"id": "net.1"}}));
    ui.reply(&m("Layout/Unplace"), json!({}));
    ui.refuse(&m("Layout/Place"), 412, "FAILED_PRECONDITION", "overlap");
    ui.reply(&m("Layout/RemoveDevice"), json!({}));
    let out = run(
        &mut ui,
        "inventory_switch",
        r#"{ kind: "switch", edit: "net.1" }"#,
        "[false, true]",
        r#"
await click(tree, "save-switch");
await click(tree, "save-switch");
choose(tree, "place-row", "");
await click(tree, "save-switch");
await click(tree, "remove-switch", 1);
await click(tree, "remove-switch", 1);
return null;"#,
    );
    let sent: Vec<_> = bodies(&ui)
        .into_iter()
        .skip(2)
        .map(|(method, body)| (method, body["id"].clone(), body["subject"]["id"].clone()))
        .collect();
    assert_eq!(
        sent,
        vec![
            (m("Layout/PutDevice"), json!("net.1"), Value::Null),
            (m("Layout/Place"), Value::Null, json!("net.1")),
            (m("Layout/PutDevice"), json!("net.1"), Value::Null),
            (m("Layout/PutDevice"), json!("net.1"), Value::Null),
            (m("Layout/Unplace"), Value::Null, json!("net.1")),
            (m("Layout/RemoveDevice"), json!("net.2"), Value::Null),
        ]
    );
    let reports = out["seen"]["reports"].as_array().expect("reports");
    assert!(
        reports[0]
            .as_str()
            .expect("report")
            .contains("It could not be placed: Another server already stands"),
        "{reports:?}"
    );
    assert_eq!(reports[1], "Another switch already has that name.");
    assert_eq!(reports[2], "Saved bmc-switch.");
    assert_eq!(reports[3], "Removed data-switch.");
    assert_eq!(out["seen"]["questions"].as_array().map(Vec::len), Some(2));
}

#[test]
fn the_switch_page_shows_the_fields_and_the_place_and_removes_after_a_confirm() {
    // Proves: the page is headed by the switch's name and its network (no state), has breadcrumbs
    // Lab > Row > Rack > the switch, a table of Network, Console (with a link to the host it is
    // cabled to), Management MAC, Where and Notes, a mini elevation of the rack; Edit goes to the
    // Inventory page's switch form; Remove asks first and, on a yes, removes that switch and goes
    // back to the Switch kind; a switch that is gone says so.
    // Catches: a page that shows another switch's fields, removes with no question, or leaves the
    // operator on a page for a switch that is gone.
    let mut ui = start();
    ui.reply(&m("Layout/RemoveDevice"), json!({}));
    let out = run(
        &mut ui,
        "switch",
        r#"{ id: "net.1" }"#,
        "[false, true]",
        r#"
const facts = rows(tree).filter((r) => r.length === 2);
const crumbs = cls(tree, "crumbs").map(words);
const title = nodes(tree).filter((n) => n.tag === "h2").map(words);
const tag = cls(tree, "pill").map(words);
const mini = cls(tree, "mini-rack").length;
await click(tree, "open-console-host");
await click(tree, "edit-switch");
await click(tree, "remove-switch");
await click(tree, "remove-switch");
return { facts, crumbs, title, tag, mini };"#,
    );
    let o = &out["out"];
    assert_eq!(o["title"], json!(["bmc-switch"]));
    assert_eq!(o["tag"], json!(["BMC network"]));
    assert_eq!(o["crumbs"], json!(["Lab Row 1 Rack 1 bmc-switch"]));
    assert_eq!(o["mini"], 1);
    assert_eq!(
        o["facts"],
        json!([
            ["Network", "BMC network"],
            ["Console", "ttyUSB0 on node-1 open node-1"],
            ["Management MAC", "3c:ec:ef:aa:00:01"],
            ["Where", "Row 1 / Rack 1, U27"],
            ["Notes", "uplink"]
        ])
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "host", "params": {"nodeId": "node-1"}},
            {"name": "inventory", "params": {"kind": "switch", "edit": "net.1"}},
            {"name": "inventory", "params": {"kind": "switch"}}
        ])
    );
    assert_eq!(
        bodies(&ui)[1..],
        [(
            m("Layout/RemoveDevice"),
            json!({"id": "net.1", "reason": "removed on the switch page"})
        )]
    );

    let gone = run(
        &mut ui,
        "switch",
        r#"{ id: "net.9" }"#,
        "[]",
        r#"await click(tree, "to-inventory"); return words(tree);"#,
    );
    assert!(
        gone["out"]
            .as_str()
            .expect("text")
            .contains("not in the inventory any more"),
        "{}",
        gone["out"]
    );
}

#[test]
fn an_unplaced_switch_says_so_and_a_failed_read_is_told() {
    // Proves: a switch with no placement is headed "Unplaced" with breadcrumbs of Lab and its name,
    // and an unreadable layout is an error view, not an empty page.
    // Catches: a page that throws for a switch in no rack.
    let mut ui = Ui::start();
    let mut lab = layout();
    lab["placements"] = json!([]);
    ui.reply(&m("Layout/Get"), lab);
    let out = run(
        &mut ui,
        "switch",
        r#"{ id: "net.2" }"#,
        "[]",
        r#"return { facts: rows(tree).filter((r) => r.length === 2).map((r) => r[1]), crumbs: cls(tree, "crumbs").map(words) };"#,
    );
    assert_eq!(
        out["out"]["facts"],
        json!(["Data network", "none", "-", "Unplaced", "-"])
    );
    assert_eq!(out["out"]["crumbs"], json!(["Lab data-switch"]));

    let mut broken = Ui::start();
    broken.refuse(&m("Layout/Get"), 500, "INTERNAL", "store_failed");
    let failed = run(
        &mut broken,
        "switch",
        r#"{ id: "net.1" }"#,
        "[]",
        "return null;",
    );
    assert!(
        failed["text"]
            .as_str()
            .expect("text")
            .contains("store_failed"),
        "{}",
        failed["text"]
    );
}

#[test]
fn the_lab_model_reads_devices_and_leaves_their_placements_out_of_the_servers() {
    // Proves: layoutOf reads a device's role as bmc or data and its fields; gearOf joins each with
    // its placement; serversOf does not list a placement of a device as a server that is gone, and
    // still lists one of an entry that is gone; consoleWords says "none" or "ttyUSB0 on node-1".
    // Catches: a switch counted as an offline server because no host has its id.
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ layoutOf, gearOf, serversOf, consoleWords }} from "./lab.js";
export async function main() {{
  const layout = layoutOf({});
  const servers = serversOf(layout, [], []);
  return {{
    roles: layout.devices.map((d) => d.role),
    gear: gearOf(layout).map((g) => [g.id, g.placement && g.placement.u]),
    servers: servers.map((s) => [s.key, s.gone === true]),
    words: layout.devices.map(consoleWords),
    none: layoutOf({{}}).devices,
  }};
}}"#,
        layout()
    ));
    assert_eq!(out["roles"], json!(["bmc", "data"]));
    assert_eq!(out["gear"], json!([["net.1", 27.0], ["net.2", 26.0]]));
    assert_eq!(out["servers"], json!([["inventory:node-1", true]]));
    assert_eq!(out["words"], json!(["ttyUSB0 on node-1", "none"]));
    assert_eq!(out["none"], json!([]));
}
