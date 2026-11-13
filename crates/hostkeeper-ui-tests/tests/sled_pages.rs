//! Sleds on Home (docs/design/rack-sleds.md): adding a sled, the drawing of a sled as a faceplate or
//! a tray of bays, assigning and unassigning items, dragging a sled or an item, the keyboard and the
//! Move... forms, resize and remove, and the address in the list. Each runs in the JavaScript engine
//! against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// Two racks of 10 U in row 0. Rack 0: a 4 U one-slot sled at U1 with `web-1` (C0-R0-R0-S0), a 3 U
/// two-slot sled at U6 with `mac-studio-1` on slot 0 and slot 1 empty (S1), a 2 U three-slot sled at U9
/// with `nuc-1` and `mini-2` on slots 0 and 1 (S2). Rack 1: an empty 2 U two-slot sled at U1 (S0) and
/// two empty one-slot sleds at U4 (S1) and U6 (S2). `web-2` stands on no sled.
fn layout() -> Value {
    let host = |id: &str| json!({"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": id});
    let on = |id: &str, rack: &str, u: u32, h: u32, sled: &str, slot: u32, address: &str| {
        json!({"subject": host(id), "rack": rack, "u": u, "heightU": h, "sled": sled,
               "slot": slot, "address": address})
    };
    json!({
        "name": "Lab 1",
        "rows": [{"id": "row-1", "number": 1}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10},
            {"id": "rack-2", "row": "row-1", "number": 2, "heightU": 10},
        ],
        "sleds": [
            {"id": "sled-1", "rack": "rack-1", "u": 1, "heightU": 4, "slots": 1, "address": "C0-R0-R0-S0"},
            {"id": "sled-2", "rack": "rack-1", "number": 1, "u": 6, "heightU": 3, "slots": 2, "address": "C0-R0-R0-S1"},
            {"id": "sled-3", "rack": "rack-1", "number": 2, "u": 9, "heightU": 2, "slots": 3, "address": "C0-R0-R0-S2"},
            {"id": "sled-4", "rack": "rack-2", "u": 1, "heightU": 2, "slots": 2, "address": "C0-R0-R1-S0"},
            {"id": "sled-5", "rack": "rack-2", "number": 1, "u": 4, "heightU": 1, "slots": 1, "address": "C0-R0-R1-S1"},
            {"id": "sled-6", "rack": "rack-2", "number": 2, "u": 6, "heightU": 1, "slots": 1, "address": "C0-R0-R1-S2"},
        ],
        "placements": [
            on("web-1", "rack-1", 1, 4, "sled-1", 0, "C0-R0-R0-S0-S0"),
            on("mac-studio-1", "rack-1", 6, 3, "sled-2", 0, "C0-R0-R0-S1-S0"),
            on("nuc-1", "rack-1", 9, 2, "sled-3", 0, "C0-R0-R0-S2-S0"),
            on("mini-2", "rack-1", 9, 2, "sled-3", 1, "C0-R0-R0-S2-S1"),
        ],
    })
}

fn hosts() -> Value {
    let one = |id: &str| {
        json!({"nodeId": id, "role": "build", "live": true, "lastHeartbeatUnix": "1800000000",
               "services": [{"service": "hostkeeper", "installedVersion": "1.4.0"}]})
    };
    json!({"hosts": [one("web-1"), one("mac-studio-1"), one("nuc-1"), one("mini-2"), one("web-2")]})
}

/// A gateway that answers the reads of `renders` renderings.
fn reads(renders: usize) -> Ui {
    let ui = Ui::start();
    for _ in 0..renders {
        ui.reply(&m("Hosts/List"), hosts());
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(&m("Layout/Get"), layout());
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
    }
    ui
}

/// The reads of `renders` renderings and three successes of each write of Layout.
fn start(renders: usize) -> Ui {
    let ui = reads(renders);
    for method in ["PutSled", "RemoveSled", "Place", "Unplace"] {
        for _ in 0..3 {
            ui.reply(&m(&format!("Layout/{method}")), json!({}));
        }
    }
    ui
}

fn home(ui: &mut Ui, params: &str, then: &str) -> Value {
    let helpers = r#"
const cls = (tree, c) => nodes(tree).filter((n) => (n.attrs.class || "").split(" ").includes(c));
const words = (n) => text(n).replace(/\s+/g, " ").trim();
const last = () => seen.shown[seen.shown.length - 1];
const freeU = (t, rack, u) => named(t, "free-u").find((n) => n.attrs["aria-label"] === `free U${u} of Row 0 / Rack ${rack}`);
const bayAt = (t, address) => named(t, "empty-bay").find((n) => n.attrs["aria-label"] === `empty slot ${address}`);
const press = (key) => ({ key, preventDefault: () => { seen.prevented = (seen.prevented || 0) + 1; } });
const drag = () => ({ preventDefault: () => { seen.prevented = (seen.prevented || 0) + 1; }, stopPropagation: () => {} });
"#;
    ui.run(&page_driver(
        "home",
        params,
        "[]",
        &format!(
            "{helpers}const __r = await (async () => {{ {then} }})(); seen.shown = []; return __r;"
        ),
    ))["out"]
        .clone()
}

fn sent(ui: &Ui, method: &str) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(name, _)| *name == m(method))
        .map(|(_, body)| body)
        .collect()
}

/// Proves: Add sled asks Size (1 to 60 U from a list), Slots (1 to 8, default 1) and U location, and
/// offers only the U where that size fits in the rack chosen (rack 0 has one free U, rack 1 has
/// U3, U5 and U7 to 10); Add sled sends one PutSled with no id, the rack, the U, the size and the
/// slots; a size that fits nowhere sends nothing and says so.
/// Catches: a U offered where the sled would overlap another or pass the top, a typed size, and an
/// add that sends the old height-and-U placement.
#[test]
fn add_sled_sends_size_slots_and_u_and_offers_only_the_u_where_it_fits() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const sizes = choices(tree, "new-sled-height", 0);
const slots = choices(tree, "new-sled-slots", 0);
const firstSlots = chosen(tree, "new-sled-slots", 0);
const starts1 = choices(tree, "new-sled-u", 0);
choose(tree, "new-sled-rack", "rack-2", 0);
let now = last();
const starts1b = choices(now, "new-sled-u", 0);
choose(now, "new-sled-height", "3", 0);
now = last();
const starts3 = choices(now, "new-sled-u", 0);
choose(now, "new-sled-slots", "2", 0);
choose(now, "new-sled-u", "7", 0);
await click(now, "add-sled", 0);
choose(now, "new-sled-rack", "rack-1", 0);
now = last();
const none = choices(now, "new-sled-u", 0);
await click(now, "add-sled", 0);
return { sizes: [sizes[0], sizes[59], sizes.length], slots: [slots[0], slots[7], slots.length], firstSlots,
         starts1, starts1b, starts3, none, reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(out["sizes"], json!(["1U", "60U", 60]));
    assert_eq!(out["slots"], json!(["1", "8", 8]));
    assert_eq!(out["firstSlots"], "1");
    assert_eq!(out["starts1"], json!(["U5"]));
    assert_eq!(
        out["starts1b"],
        json!(["U3", "U5", "U7", "U8", "U9", "U10"])
    );
    assert_eq!(out["starts3"], json!(["U7", "U8"]));
    assert_eq!(out["none"], json!([]));
    assert_eq!(out["refreshes"], 1);
    assert_eq!(
        out["reports"],
        json!([
            "A sled 3U high with 2 slots is added at Row 0 / Rack 1, U7.",
            "No U of Row 0 / Rack 0 has room for a sled 3U high."
        ])
    );
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "", "rack": "rack-2", "u": 7, "heightU": 3, "slots": 2, "label": "",
                    "reason": "add sled"})
        ]
    );
}

/// Proves: an empty sled is drawn in its rack as a tray with one bay for each slot (an empty
/// two-slot sled has two, an empty one-slot sled one, none a faceplate), each bay a gap that the
/// edit mode makes a button named for its address.
/// Catches: an empty sled that is invisible, drawn as a server, or with the wrong number of bays.
#[test]
fn an_empty_sled_draws_one_bay_for_each_slot() {
    let mut ui = start(2);
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
const trays = cls(tree, "sled").filter((n) => cls(n, "bays").length === 1);
const sizeOf = (n) => [n.attrs.vars["--r"], n.attrs.vars["--s"], cls(n, "bay").length, nodes(n).filter((c) => c.tag === "button").length];
return { empty: trays.filter((n) => (n.attrs.class || "").includes("vacant")).map(sizeOf),
         servers: cls(tree, "server").length };"#,
    );
    // Rack 1 (10 U): the 2 U sled at U1 starts at row 9, the one-slot sleds at U4 and U6 at rows 7 and 5.
    assert_eq!(
        out["empty"],
        json!([[5, 1, 1, 0], [7, 1, 1, 0], [9, 2, 2, 0]])
    );
    assert_eq!(out["servers"], 1, "only web-1 is a faceplate");
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
return named(tree, "empty-bay").map((n) => n.attrs["aria-label"]);"#,
    );
    assert_eq!(
        out,
        json!([
            "empty slot C0-R0-R0-S2-S2",
            "empty slot C0-R0-R0-S1-S1",
            "empty slot C0-R0-R1-S2-S0",
            "empty slot C0-R0-R1-S1-S0",
            "empty slot C0-R0-R1-S0-S0",
            "empty slot C0-R0-R1-S0-S1"
        ])
    );
}

/// Proves: a one-slot sled with its item is exactly a server faceplate (class server, no tray, the
/// state in its class, the address in its tooltip); a sled of more than one slot is a tray with a
/// bay for each slot, an item's box looks like a Mac Studio when its name holds "studio", a NUC when
/// it holds "nuc", else a plain box; a box opens the host and shows its address on hover.
/// Catches: a lone server drawn inside a visible tray, a multi-slot sled drawn as one server, a look
/// chosen by anything but the name, and a box that does not open its host.
#[test]
fn a_one_slot_sled_is_a_plain_faceplate_and_a_multi_slot_sled_is_a_tray_of_bays() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ rack: "rack-1" }"#,
        r#"
const plate = named(tree, "open-server").filter((n) => n.attrs.class.startsWith("server "));
const boxes = cls(tree, "box").map((n) => [n.attrs.class.split(" ").slice(0, 3).join(" "), n.attrs.title]);
const trays = cls(tree, "sled").map((n) => [n.attrs.class.split(" ")[1], cls(n, "bay").length, cls(n, "box").length]);
await click(tree, "open-server", 0);
return { plate: plate.map((n) => [n.attrs.class, n.attrs.title]), boxes, trays, nav: seen.navigations,
         plateInTray: cls(tree, "sled").some((n) => cls(n, "server").length > 0) };"#,
    );
    assert_eq!(
        out["plate"][0],
        json!([
            "server s-healthy tall",
            "web-1 | version 1.4.0 | healthy | C0-R0-R0-S0-S0"
        ])
    );
    assert_eq!(out["plateInTray"], false);
    assert_eq!(
        out["boxes"],
        json!([
            [
                "box nuc s-healthy",
                "nuc-1 | version 1.4.0 | healthy | C0-R0-R0-S2-S0"
            ],
            [
                "box generic s-healthy",
                "mini-2 | version 1.4.0 | healthy | C0-R0-R0-S2-S1"
            ],
            [
                "box studio s-healthy",
                "mac-studio-1 | version 1.4.0 | healthy | C0-R0-R0-S1-S0"
            ]
        ])
    );
    // A tray for each of the two sleds of several slots: one empty bay and two boxes, one of each.
    assert_eq!(out["trays"], json!([["slots-3", 1, 2], ["slots-2", 1, 1]]));
    // The first open-server button of the rack, from the top, is the first box of the top tray.
    assert_eq!(
        out["nav"],
        json!([{"name": "host", "params": {"nodeId": "nuc-1"}}])
    );
}

/// Proves: in Edit layout a click on an empty bay lists the unplaced items (a server of the lab or
/// a switch), and a click on one sends exactly one Place with the item's subject, the sled and the
/// slot of that bay; the page is read again and says where it stands.
/// Catches: an assignment that sends a rack, U or height, two calls for one click, or offers an item
/// that already stands on a sled.
#[test]
fn assigning_by_clicking_an_empty_bay_sends_one_place() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await bayAt(tree, "C0-R0-R0-S1-S1").on.click();
const panel = words(cls(last(), "assign-panel")[0]);
const offered = named(last(), "assign-item").map(words);
await click(last(), "assign-item", 0);
return { panel, offered, reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(out["offered"], json!(["web-2"]));
    assert!(
        out["panel"]
            .as_str()
            .unwrap()
            .starts_with("C0-R0-R0-S1-S1 is empty"),
        "{out}"
    );
    assert_eq!(out["refreshes"], 1);
    assert_eq!(out["reports"], json!(["web-2 stands at C0-R0-R0-S1-S1."]));
    assert_eq!(
        sent(&ui, "Layout/Place"),
        vec![
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"},
                    "sled": "sled-2", "slot": 1, "reason": "placed web-2"})
        ]
    );
}

/// Proves: picking a placed item shows Unassign, and Unassign sends one Unplace for its subject.
/// Catches: an unassign that removes the sled, or names the wrong subject.
#[test]
fn unassign_sends_unplace() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "pick-item", 0);
const pressed = named(last(), "pick-item")[0].attrs["aria-pressed"];
await click(last(), "unassign", 0);
return { pressed, reports: seen.reports };"#,
    );
    assert_eq!(out["pressed"], "true");
    assert_eq!(out["reports"], json!(["nuc-1 is unplaced."]));
    assert_eq!(
        sent(&ui, "Layout/Unplace"),
        vec![
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "nuc-1"},
                    "reason": "unplaced nuc-1"})
        ]
    );
    assert!(sent(&ui, "Layout/RemoveSled").is_empty());
}

/// Proves: dragging a sled (its handle) over a free U of a rack where it fits shows the target
/// outline over the U it would take (3 U from the lowest start that holds it), accepts the drop, and
/// the drop sends one PutSled with the new rack and U and the sled's other fields (size, slots,
/// label); dragging over a U where it does not fit shows no outline and accepts no drop.
/// Catches: a drop that sends two calls, loses the sled's slots or label, or an outline drawn where
/// the sled would overlap.
#[test]
fn dragging_a_sled_to_another_rack_sends_one_putsled_and_shows_the_outline_only_where_it_fits() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const ghosts = () => cls(tree, "drop-ghost").map((n) => [n.attrs.class, n.attrs.vars["--r"], n.attrs.vars["--s"]]);
const handle = named(tree, "pick-sled")[1];
handle.on.dragstart(drag());
const before = ghosts();
freeU(tree, 1, 9).on.dragover(drag());
const fits = ghosts();
const prevented = seen.prevented;
freeU(tree, 1, 3).on.dragover(drag());
const nofit = ghosts();
const preventedAfter = seen.prevented;
freeU(tree, 1, 9).on.dragover(drag());
await freeU(tree, 1, 9).on.drop(drag());
return { before, fits, nofit, prevented, preventedAfter, reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(
        out["before"],
        json!([["drop-ghost", 1, 1], ["drop-ghost", 1, 1]])
    );
    // Rack 1 (10 U): free U9 holds a 3 U sled from U7 (U7 to U9): rows counted from the top, 2.
    assert_eq!(out["fits"][1], json!(["drop-ghost on", 2.0, 3.0]));
    assert_eq!(out["fits"][0], json!(["drop-ghost", 1, 1]));
    assert_eq!(out["prevented"], 1);
    assert_eq!(out["nofit"][1], json!(["drop-ghost", 1, 1]));
    assert_eq!(
        out["preventedAfter"], 1,
        "no drop is accepted where it does not fit"
    );
    assert_eq!(out["refreshes"], 1);
    assert_eq!(
        out["reports"],
        json!(["C0-R0-R0-S1 is at Row 0 / Rack 1, U7."])
    );
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "sled-2", "rack": "rack-2", "u": 7, "heightU": 3, "slots": 2, "label": "",
                    "reason": "moved C0-R0-R0-S1"})
        ]
    );
}

/// Proves: a drop the sled does not fit sends nothing and tells why; a drop the coordinator then
/// refuses (the layout changed under the page) is told in words and the page is read again.
/// Catches: a refusal shown as a bare token, a page left stale after a refusal, and a call sent for
/// a drop that cannot fit.
#[test]
fn a_drop_that_does_not_fit_sends_nothing_and_a_refusal_is_told_in_words() {
    let mut ui = reads(1);
    ui.refuse(&m("Layout/PutSled"), 412, "FAILED_PRECONDITION", "overlap");
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
named(tree, "pick-sled")[1].on.dragstart(drag());
await freeU(tree, 1, 3).on.drop(drag());
const first = [seen.reports.length, seen.refreshes];
named(tree, "pick-sled")[1].on.dragstart(drag());
await freeU(tree, 1, 9).on.drop(drag());
return { first, reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(out["first"], json!([1, 0]));
    assert_eq!(
        out["reports"],
        json!([
            "C0-R0-R0-S1 does not fit at U3: it needs 3 free U.",
            "That place is taken: another sled stands in one of those U, or another item on that slot."
        ])
    );
    assert_eq!(
        out["refreshes"], 1,
        "the page is read again from the coordinator"
    );
    assert_eq!(sent(&ui, "Layout/PutSled").len(), 1);
}

/// Proves: the outline of a drag is set on the page's own elements while the tree is mounted, with no
/// new drawing of the page (a drag that is redrawn is cancelled by the browser): over a fitting U the
/// outline element gets the class `on` and the rows of the U it would take as custom properties, over
/// a U that does not fit it is hidden again, and the end of the drag hides it.
/// Catches: an outline kept only on the view tree, which the browser never shows, or one left on
/// after the drag ends.
#[test]
fn the_outline_is_painted_on_the_mounted_elements_without_drawing_the_page_again() {
    let mut ui = start(1);
    let out = ui.run(
        r#"
import { api, recorder, mounted, elements, fire } from "../test/kit.js";
import { render } from "./pages/home.js";
export async function main() {
  const { ctx, seen } = recorder(api("operator-token"), { row: "row-1", edit: true }, []);
  const tree = await render(ctx);
  const doc = mounted(tree);
  const all = elements(doc.root);
  const handle = all.filter((e) => e.attributes["name"] === "pick-sled")[1];
  const cell = (u) => all.find((e) => e.attributes["aria-label"] === "free U" + u + " of Row 0 / Rack 1");
  const ghost = all.filter((e) => (e.attributes["class"] || "").startsWith("drop-ghost"))[1];
  const ev = { preventDefault() {}, stopPropagation() {} };
  fire(handle, "dragstart", ev);
  fire(cell(9), "dragover", ev);
  const on = [ghost.attributes["class"], ghost.style.custom["--r"], ghost.style.custom["--s"]];
  fire(cell(3), "dragover", ev);
  const off = ghost.attributes["class"];
  fire(cell(9), "dragover", ev);
  fire(handle, "dragend", ev);
  return { on, off, ended: ghost.attributes["class"], shown: seen.shown.length };
}"#,
    );
    assert_eq!(out["on"], json!(["drop-ghost on", "2", "3"]));
    assert_eq!(out["off"], "drop-ghost");
    assert_eq!(out["ended"], "drop-ghost");
    assert_eq!(out["shown"], 0);
}

/// Proves: dragging an item (a box) to an empty bay sends one Place for it with that sled and slot,
/// and so does dragging an unplaced item from the Unplaced tray; a drop on a bay with nothing
/// dragged sends nothing.
/// Catches: an item drag that moves the sled, a drop that sends the old rack form, and a tray chip
/// that cannot be dragged.
#[test]
fn dragging_an_item_between_slots_and_from_the_tray_sends_one_place() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await bayAt(tree, "C0-R0-R0-S1-S1").on.drop(drag());
const chips = named(tree, "pick-unplaced");
const drags = chips.map((n) => n.attrs.draggable);
const box = named(tree, "pick-item").find((n) => n.attrs.title.startsWith("nuc-1"));
box.on.dragstart(drag());
await bayAt(tree, "C0-R0-R0-S2-S2").on.drop(drag());
chips[0].on.dragstart(drag());
await bayAt(tree, "C0-R0-R0-S1-S1").on.drop(drag());
return { drags, reports: seen.reports };"#,
    );
    assert_eq!(out["drags"], json!(["true"]));
    assert_eq!(
        out["reports"],
        json!([
            "nuc-1 stands at C0-R0-R0-S2-S2.",
            "web-2 stands at C0-R0-R0-S1-S1."
        ])
    );
    assert_eq!(
        sent(&ui, "Layout/Place"),
        vec![
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "nuc-1"},
                   "sled": "sled-3", "slot": 2, "reason": "placed nuc-1"}),
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"},
                   "sled": "sled-2", "slot": 1, "reason": "placed web-2"}),
        ]
    );
}

/// Proves: the keyboard does what the pointer does: a picked sled moves with Up and Down (one U
/// each) and Enter puts it down with the same PutSled (a target that does not fit is told and sends
/// nothing); a picked item moves one slot with Left and Right and Enter sends the same Place; Escape
/// lets go.
/// Catches: a sled or item that can only be moved by dragging, an Enter that sends before the target
/// is chosen, and keys that are not consumed.
#[test]
fn the_keyboard_moves_a_sled_and_an_item_with_the_same_calls() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await named(tree, "pick-sled")[1].on.click();
const pressed = named(last(), "pick-sled")[1].attrs["aria-pressed"];
named(last(), "pick-sled")[1].on.keydown(press("ArrowUp"));
const note1 = words(named(last(), "target-note")[0]);
await named(last(), "pick-sled")[1].on.keydown(press("Enter"));
const refusedFirst = seen.reports.slice();
named(last(), "pick-sled")[1].on.keydown(press("ArrowDown"));
named(last(), "pick-sled")[1].on.keydown(press("ArrowDown"));
const note2 = words(named(last(), "target-note")[0]);
await named(last(), "pick-sled")[1].on.keydown(press("Enter"));
await click(last(), "pick-item", 2);
const itemPressed = named(last(), "pick-item")[2].attrs["aria-pressed"];
named(last(), "pick-item")[2].on.keydown(press("ArrowRight"));
const note3 = words(named(last(), "target-note")[0]);
await named(last(), "pick-item")[2].on.keydown(press("Enter"));
named(last(), "pick-item")[2].on.keydown(press("Escape"));
return { pressed, itemPressed, note1, note2, note3, refusedFirst, prevented: seen.prevented,
         selected: named(last(), "pick-item").map((n) => n.attrs["aria-pressed"]), reports: seen.reports };"#,
    );
    assert_eq!(out["pressed"], "true");
    assert!(
        out["note1"]
            .as_str()
            .unwrap()
            .starts_with("Target: Row 0 / Rack 0, U7.")
    );
    assert!(
        out["note2"]
            .as_str()
            .unwrap()
            .starts_with("Target: Row 0 / Rack 0, U5.")
    );
    assert!(
        out["note3"]
            .as_str()
            .unwrap()
            .starts_with("Target: slot 1.")
    );
    assert_eq!(
        out["refusedFirst"],
        json!([
            "C0-R0-R0-S1 does not fit at U7: another sled stands there or it would reach past the top."
        ])
    );
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "sled-2", "rack": "rack-1", "u": 5, "heightU": 3, "slots": 2, "label": "",
                    "reason": "moved C0-R0-R0-S1"})
        ]
    );
    let places = sent(&ui, "Layout/Place");
    assert_eq!(places.len(), 1, "{places:?}");
    assert_eq!(places[0]["sled"], "sled-2");
    assert_eq!(places[0]["slot"], 1);
    assert_eq!(out["itemPressed"], "true");
}

/// Proves: Move... on a picked sled picks a rack and a U location (only where the sled fits, its own
/// U counting as free) and sends the same PutSled; Move... on a picked item picks a sled slot by
/// address and sends the same Place.
/// Catches: a dialog that offers the sled's own U as taken or a slot that is taken, or that sends a
/// different call from the drag.
#[test]
fn the_move_dialog_picks_a_rack_and_u_for_a_sled_and_a_slot_for_an_item() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await named(tree, "pick-sled")[1].on.click();
await click(last(), "move-sled", 0);
const own = choices(last(), "move-u", 0);
choose(last(), "move-rack", "rack-2", 0);
const other = choices(last(), "move-u", 0);
choose(last(), "move-u", "8", 0);
await click(last(), "move-sled-go", 0);
await click(last(), "pick-item", 2);
await click(last(), "move-item", 0);
const slots = choices(last(), "move-slot", 0);
choose(last(), "move-slot", "sled-4:1", 0);
await click(last(), "move-item-go", 0);
return { own, other, slots, reports: seen.reports };"#,
    );
    assert_eq!(out["own"], json!(["U5", "U6"]));
    assert_eq!(out["other"][0], "U7");
    assert_eq!(out["other"].as_array().unwrap().last().unwrap(), "U8");
    assert!(
        out["slots"]
            .as_array()
            .unwrap()
            .contains(&json!("C0-R0-R1-S0-S1"))
    );
    assert!(
        !out["slots"]
            .as_array()
            .unwrap()
            .contains(&json!("C0-R0-R0-S2-S0"))
    );
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "sled-2", "rack": "rack-2", "u": 8, "heightU": 3, "slots": 2, "label": "",
                    "reason": "moved C0-R0-R0-S1"})
        ]
    );
    assert_eq!(sent(&ui, "Layout/Place").len(), 1);
}

/// Proves: Resize sends one PutSled with the sled's rack and U and the new size, slots and label;
/// Remove sends RemoveSled, and when the coordinator refuses `in_use` the words name what stands on
/// the sled.
/// Catches: a resize that moves the sled, and a remove refusal that does not say what is in the way.
#[test]
fn resize_keeps_the_place_and_a_remove_refusal_names_what_stands_on_the_sled() {
    let mut ui = reads(1);
    ui.refuse(
        &m("Layout/RemoveSled"),
        412,
        "FAILED_PRECONDITION",
        "in_use",
    );
    ui.reply(&m("Layout/PutSled"), json!({}));
    let out = home(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await named(tree, "pick-sled")[1].on.click();
choose(last(), "resize-height", "2", 0);
choose(last(), "resize-slots", "4", 0);
type(last(), "resize-label", " spare ", 0);
await click(last(), "resize-sled", 0);
await click(last(), "remove-sled", 0);
return seen.reports;"#,
    );
    assert_eq!(
        out,
        json!([
            "C0-R0-R0-S1 is 2U with 4 slots.",
            "C0-R0-R0-S1 still holds mac-studio-1: unassign them first."
        ])
    );
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "sled-2", "rack": "rack-1", "u": 6, "heightU": 2, "slots": 4,
                    "label": "spare", "reason": "resized C0-R0-R0-S1"})
        ]
    );
    assert_eq!(sent(&ui, "Layout/RemoveSled").len(), 1);
}

/// Proves: the List view has an Address column and a filter that takes a name or an address: an
/// address matches the sled's slots (`C0-R0-R0-S1` finds `C0-R0-R0-S1-S0`) and no other sled whose
/// number only starts the same.
/// Catches: a list with no address, and a filter that matches `S1` against `S10`.
#[test]
fn the_list_shows_the_address_and_filters_by_an_address() {
    let mut ui = start(1);
    let out = home(
        &mut ui,
        "{ list: true }",
        r#"
const all = rows(tree);
type(tree, "list-filter", "c0-r0-r0-s1");
await click(tree, "apply-filter");
const one = rows(last()).slice(1).map((r) => r[0]);
type(last(), "list-filter", "nuc");
await click(last(), "apply-filter");
const byName = rows(last()).slice(1).map((r) => r[0]);
return { head: all[0], web1: all.find((r) => r[0] === "web-1"), one, byName };"#,
    );
    assert_eq!(
        out["head"],
        json!(["Server", "State", "Address", "Where", "Group", "Version"])
    );
    assert_eq!(out["web1"][2], "C0-R0-R0-S0-S0");
    assert_eq!(out["one"], json!(["mac-studio-1"]));
    assert_eq!(out["byName"], json!(["nuc-1"]));
}

/// Proves: the lab model's helpers: the free U of a rack for a sled height (the sled being moved
/// counts as free), the lowest start a drop on a U would take, the free slots by address in numeric
/// order (S2 before S10), and a slot choice read back.
/// Catches: addresses ordered as text, a sled that cannot be moved over its own U, and a free slot
/// list that includes a taken one.
#[test]
fn the_lab_model_lists_free_u_and_free_slots() {
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ layoutOf, freeSledStarts, dropStart, freeSlots, slotOf, slotOptions, usedU }} from "./lab.js";
export async function main() {{
  const lab = layoutOf({layout});
  const wide = layoutOf({{ racks: [{{ id: "r", row: "w", number: 1, heightU: 4 }}],
    sleds: [{{ id: "a", rack: "r", u: 1, heightU: 1, slots: 2, address: "C0-R0-R0-S10" }},
            {{ id: "b", rack: "r", u: 3, heightU: 1, slots: 1, address: "C0-R0-R0-S2" }}] }});
  return {{
    starts: freeSledStarts(lab, "rack-1", 1),
    own: freeSledStarts(lab, "rack-1", 3, "sled-2"),
    drop: [dropStart(lab, "rack-1", 3, 5), dropStart(lab, "rack-1", 3, 5, "sled-2"), dropStart(lab, "rack-2", 3, 3)],
    free: freeSlots(lab).map((s) => s.address),
    withOwn: freeSlots(lab, "host:nuc-1").map((s) => s.address).includes("C0-R0-R0-S2-S0"),
    order: freeSlots(wide).map((s) => s.address),
    option: slotOptions(lab, undefined, true)[0],
    slot: [slotOf("sled-4:1"), slotOf(""), slotOf("x")],
    used: [usedU(lab, "rack-1"), usedU(lab, "rack-2")],
  }};
}}"#,
        layout = layout()
    ));
    assert_eq!(out["starts"], json!([5]));
    assert_eq!(out["own"], json!([5, 6]));
    assert_eq!(out["drop"], json!([null, 5, null]));
    assert_eq!(
        out["free"],
        json!([
            "C0-R0-R0-S1-S1",
            "C0-R0-R0-S2-S2",
            "C0-R0-R1-S0-S0",
            "C0-R0-R1-S0-S1",
            "C0-R0-R1-S1-S0",
            "C0-R0-R1-S2-S0"
        ])
    );
    assert_eq!(out["withOwn"], true);
    assert_eq!(
        out["order"],
        json!(["C0-R0-R0-S2-S0", "C0-R0-R0-S10-S0", "C0-R0-R0-S10-S1"])
    );
    assert_eq!(out["option"], json!(["", "Unplaced"]));
    assert_eq!(
        out["slot"],
        json!([{"sled": "sled-4", "slot": 1.0}, null, null])
    );
    assert_eq!(out["used"], json!([9, 4]));
}
