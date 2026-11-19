//! The edit mode's controls on a rack (`ui/rackforms.js`): the "+ Add sled" and "+ Add device"
//! buttons on each rack and the form each opens, titled with what it adds and where; the form keeping
//! its inputs across the page's refresh after an add (the next sled at the next free U, the next device
//! on the next free slot); the preview of where it goes; errors told in the form; the rack's height
//! set by its stepper, its height list and the grip on its bottom edge; and a shrink that would cut a
//! sled refused before any call. Each runs in the JavaScript engine against the fake gateway of
//! `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m};

/// A host subject of the layout.
fn host(id: &str) -> Value {
    json!({"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": id})
}

/// The lab of the tests, with the sleds `extra` added to rack 1 and the placements `placed` added:
/// row 0 holds rack 0 (`rack-1`, 10 U) and rack 1 (`rack-2`, 12 U). In rack 0 `web-1` stands on a 4 U
/// one-slot sled at U1 (C0-R0-R0-S0) and `mac-studio-1` on slot 0 of a 3 U two-slot sled at U6
/// (C0-R0-R0-S1), whose slot 1 is free; rack 0 has U5, U9 and U10 free. Rack 1 is empty but for
/// `extra`. The switch `sw-1` (`net.1`) stands nowhere.
fn layout_with(extra: &[Value], placed: &[Value]) -> Value {
    let mut sleds = vec![
        json!({"id": "sled-1", "rack": "rack-1", "u": 1, "heightU": 4, "slots": 1, "address": "C0-R0-R0-S0"}),
        json!({"id": "sled-2", "rack": "rack-1", "number": 1, "u": 6, "heightU": 3, "slots": 2,
               "address": "C0-R0-R0-S1"}),
    ];
    sleds.extend(extra.iter().cloned());
    let mut placements = vec![
        json!({"subject": host("web-1"), "rack": "rack-1", "u": 1, "heightU": 4, "sled": "sled-1",
               "address": "C0-R0-R0-S0-S0"}),
        json!({"subject": host("mac-studio-1"), "rack": "rack-1", "u": 6, "heightU": 3, "sled": "sled-2",
               "address": "C0-R0-R0-S1-S0"}),
    ];
    placements.extend(placed.iter().cloned());
    json!({
        "name": "Lab 1",
        "rows": [{"id": "row-1", "number": 0}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "number": 0, "heightU": 10},
            {"id": "rack-2", "row": "row-1", "number": 1, "heightU": 12},
        ],
        "sleds": sleds,
        "placements": placements,
        "devices": [{"id": "net.1", "name": "sw-1", "role": "NETWORK_ROLE_DATA"}],
    })
}

fn layout() -> Value {
    layout_with(&[], &[])
}

/// The `n`th 2 U one-slot sled of rack 1, from U1 up (the sleds four adds make).
fn studio_sled(n: u32) -> Value {
    json!({"id": format!("sled-1{n}"), "rack": "rack-2", "number": n, "u": 1 + 2 * n, "heightU": 2,
           "slots": 1, "label": "Mac Studio", "address": format!("C0-R0-R1-S{n}")})
}

/// The empty 2 U four-slot sled of rack 1 at U1 that the device tests place on.
fn tray() -> Value {
    json!({"id": "sled-9", "rack": "rack-2", "u": 1, "heightU": 2, "slots": 4, "address": "C0-R0-R1-S0"})
}

/// `id` on slot `slot` of the tray of [`tray`].
fn on_tray(subject: Value, slot: u32) -> Value {
    json!({"subject": subject, "rack": "rack-2", "u": 1, "heightU": 2, "sled": "sled-9", "slot": slot,
           "address": format!("C0-R0-R1-S0-S{slot}")})
}

/// The hosts: the three placed in [`layout`] and two Mac Studios on no sled.
fn hosts() -> Value {
    let one = |id: &str| {
        json!({"nodeId": id, "role": "build", "live": true, "lastHeartbeatUnix": "1800000000",
               "services": [{"service": "hostkeeper", "installedVersion": "1.4.0"}]})
    };
    json!({"hosts": [one("web-1"), one("mac-studio-1"), one("mac-studio-2"), one("mac-studio-3")]})
}

/// A gateway that answers one rendering for each layout of `layouts`, in order, with `hosts`.
fn reads_of(layouts: &[Value], hosts: &Value) -> Ui {
    let ui = Ui::start();
    for layout in layouts {
        ui.reply(&m("Hosts/List"), hosts.clone());
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(&m("Layout/Get"), layout.clone());
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
    }
    ui
}

/// [`reads_of`] with [`hosts`], and four successes of each write the forms make.
fn start_with(layouts: &[Value]) -> Ui {
    let ui = reads_of(layouts, &hosts());
    for method in ["PutSled", "Place", "PutRack"] {
        for _ in 0..4 {
            ui.reply(&m(&format!("Layout/{method}")), json!({}));
        }
    }
    ui
}

/// Renders Home with `params`, then runs `then` with the tree (`tree`), the recorder (`seen`, `ctx`),
/// `render` to draw the page again as a refresh does, and the helpers below; returns what it returns.
fn drive(ui: &mut Ui, params: &str, then: &str) -> Value {
    ui.run(&format!(
        r#"
import {{ api, recorder, text, named, type, choose, chosen, choices, click, nodes, mounted, fire, domNamed }} from "../test/kit.js";
import {{ render }} from "./pages/home.js";
export async function main() {{
  const {{ ctx, seen }} = recorder(api("operator-token"), {params}, []);
  const tree = await render(ctx);
  const words = (n) => text(n).replace(/\s+/g, " ").trim();
  const last = () => seen.shown[seen.shown.length - 1];
  const all = (t, name) => nodes(t).filter((n) => n.attrs.name === name);
  const one = (t, name) => {{ const found = all(t, name); return found.length === 0 ? null : words(found[0]); }};
  const settle = async () => {{ for (let i = 0; i < 60; i += 1) {{ await null; }} }};
  const out = await (async () => {{ {then} }})();
  return out === undefined ? null : out;
}}"#
    ))
}

fn sent(ui: &Ui, method: &str) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(name, _)| *name == m(method))
        .map(|(_, body)| body)
        .collect()
}

/// Proves: each rack of the edit mode has a "+ Add sled" and a "+ Add device" naming its rack; a click
/// on one opens one form above the racks titled with what it adds, the rack and the place (the first
/// free U, or the first free slot), marks that + expanded and no other, offers only that rack's U or
/// slots, and a second click closes it; outside the edit mode there is no +; the rack page has the +
/// of its one rack.
/// Catches: a + that opens the form of another rack (the old form's rack defaulted to the first rack
/// in view), a form that does not say what it adds or where, and a + outside the edit mode.
#[test]
fn the_plus_on_each_rack_opens_the_form_of_that_rack_titled_with_what_it_adds_and_where() {
    let mut ui = start_with(&[layout(), layout(), layout()]);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const labels = all(tree, "add-sled-open").map((n) => n.attrs["aria-label"])
  .concat(all(tree, "add-device-open").map((n) => n.attrs["aria-label"]));
const before = one(tree, "rack-form-title");
await click(tree, "add-sled-open", 1);
let now = last();
const sled = {
  title: one(now, "rack-form-title"),
  expanded: all(now, "add-sled-open").map((n) => n.attrs["aria-expanded"]),
  starts: choices(now, "new-sled-u"),
  preview: one(now, "rack-form-preview"),
};
await click(now, "add-device-open", 0);
now = last();
const device = {
  title: one(now, "rack-form-title"),
  expanded: all(now, "add-device-open").map((n) => n.attrs["aria-expanded"]),
  sledExpanded: all(now, "add-sled-open").map((n) => n.attrs["aria-expanded"]),
  items: choices(now, "new-device-item"),
  slots: choices(now, "new-device-slot"),
  preview: one(now, "rack-form-preview"),
  marked: all(now, "empty-bay").filter((n) => n.attrs.class.split(" ").includes("target")).map((n) => n.attrs["aria-label"]),
};
await click(now, "add-device-open", 0);
now = last();
const closed = { title: one(now, "rack-form-title"), expanded: all(now, "add-device-open").map((n) => n.attrs["aria-expanded"]) };
ctx.params.edit = false;
const plain = await render(ctx);
const outside = all(plain, "add-sled-open").length + all(plain, "add-device-open").length + all(plain, "rack-resize-grip").length;
ctx.params.edit = true;
delete ctx.params.row;
ctx.params.rack = "rack-2";
const solo = await render(ctx);
const soloLabels = all(solo, "add-sled-open").map((n) => n.attrs["aria-label"]);
await click(solo, "add-sled-open", 0);
return { labels, before, sled, device, closed, outside, soloLabels, soloTitle: one(last(), "rack-form-title") };"#,
    );
    assert_eq!(
        out["labels"],
        json!([
            "Add a sled to Row 0 / Rack 0",
            "Add a sled to Row 0 / Rack 1",
            "Add a device to Row 0 / Rack 0",
            "Add a device to Row 0 / Rack 1"
        ])
    );
    assert_eq!(out["before"], Value::Null, "no form until a + is clicked");
    assert_eq!(
        out["sled"],
        json!({
            "title": "Add a sled to Row 0 / Rack 1 at U1",
            "expanded": ["false", "true"],
            "starts": ["U1", "U2", "U3", "U4", "U5", "U6", "U7", "U8", "U9", "U10", "U11", "U12"],
            "preview": "It takes U1 of Row 0 / Rack 1: 1U, 1 slot. The outline in the rack shows where.",
        })
    );
    assert_eq!(
        out["device"],
        json!({
            "title": "Add a device to Row 0 / Rack 0 on C0-R0-R0-S1-S1",
            "expanded": ["true", "false"],
            "sledExpanded": ["false", "false"],
            "items": ["mac-studio-2 (server)", "mac-studio-3 (server)", "sw-1 (switch)"],
            "slots": ["C0-R0-R0-S1-S1 (U6-8)"],
            "preview": "mac-studio-2 goes on slot 1 of the sled at U6-8: C0-R0-R0-S1-S1. The bay is marked in the rack.",
            "marked": ["empty slot C0-R0-R0-S1-S1"],
        })
    );
    assert_eq!(
        out["closed"],
        json!({"title": null, "expanded": ["false", "false"]})
    );
    assert_eq!(out["outside"], 0, "no + and no grip outside the edit mode");
    assert_eq!(out["soloLabels"], json!(["Add a sled to Row 0 / Rack 1"]));
    assert_eq!(out["soloTitle"], "Add a sled to Row 0 / Rack 1 at U1");
    assert!(sent(&ui, "Layout/PutSled").is_empty());
    assert!(sent(&ui, "Layout/Place").is_empty());
}

/// Proves: after Add sled the page is read again and the form is still open with every input as it
/// was (height, slots, label), its position moved to the next free U above the sled just added, so
/// four 2 U Mac Studio sleds are four clicks of Add sled at U1, U3, U5 and U7, each one PutSled; the
/// form says what the last add did and the rack draws the outline of the next one.
/// Catches: a form reset to its defaults after an add (the old page state was made anew on every
/// read of the page), a position left on a U that is now taken, and a second click that sends twice.
#[test]
fn adding_a_sled_keeps_the_inputs_and_puts_the_next_one_at_the_next_free_u() {
    let layouts: Vec<Value> = (0..5)
        .map(|k| layout_with(&(0..k).map(studio_sled).collect::<Vec<_>>(), &[]))
        .collect();
    let mut ui = start_with(&layouts);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-sled-open", 1);
let now = last();
choose(now, "new-sled-height", "2");
now = last();
type(now, "new-sled-label", "Mac Studio");
const forms = [];
for (let i = 0; i < 4; i += 1) {
  await click(now, "add-sled");
  now = await render(ctx);
  const ghost = nodes(now).filter((n) => (n.attrs.class || "").split(" ").includes("drop-ghost") && n.attrs.class.split(" ").includes("preview"));
  forms.push({
    title: one(now, "rack-form-title"),
    height: chosen(now, "new-sled-height"),
    slots: chosen(now, "new-sled-slots"),
    label: named(now, "new-sled-label")[0].attrs.value,
    u: chosen(now, "new-sled-u"),
    said: one(now, "rack-form-said"),
    outline: ghost.map((n) => Object.entries(n.attrs.vars).map(([k, v]) => `${k}=${v}`).join(" ")),
  });
}
return { forms, refreshes: seen.refreshes, reports: seen.reports };"#,
    );
    let form = |title: &str, u: &str, said: &str, top_row: u32| {
        json!({"title": title, "height": "2", "slots": "1", "label": "Mac Studio", "u": u,
               "said": said, "outline": [format!("--r={top_row} --s=2")]})
    };
    assert_eq!(
        out["forms"],
        json!([
            form(
                "Add a sled to Row 0 / Rack 1 at U3-4",
                "3",
                "Added a 2U sled with 1 slot at Row 0 / Rack 1, U1-2.",
                9
            ),
            form(
                "Add a sled to Row 0 / Rack 1 at U5-6",
                "5",
                "Added a 2U sled with 1 slot at Row 0 / Rack 1, U3-4.",
                7
            ),
            form(
                "Add a sled to Row 0 / Rack 1 at U7-8",
                "7",
                "Added a 2U sled with 1 slot at Row 0 / Rack 1, U5-6.",
                5
            ),
            form(
                "Add a sled to Row 0 / Rack 1 at U9-10",
                "9",
                "Added a 2U sled with 1 slot at Row 0 / Rack 1, U7-8.",
                3
            ),
        ])
    );
    assert_eq!(out["refreshes"], 4);
    // The page's status line announces each add too.
    assert_eq!(
        out["reports"],
        json!([
            "Added a 2U sled with 1 slot at Row 0 / Rack 1, U1-2.",
            "Added a 2U sled with 1 slot at Row 0 / Rack 1, U3-4.",
            "Added a 2U sled with 1 slot at Row 0 / Rack 1, U5-6.",
            "Added a 2U sled with 1 slot at Row 0 / Rack 1, U7-8."
        ])
    );
    let studio = |u: u32| {
        json!({"id": "", "rack": "rack-2", "u": u, "heightU": 2, "slots": 1, "label": "Mac Studio",
               "reason": "add sled"})
    };
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![studio(1), studio(3), studio(5), studio(7)]
    );
}

/// Proves: Add device places the server or switch chosen on the slot chosen with one Place (a
/// switch by its inventory subject), and after the page is read again the form is still open on the
/// next item waiting and the next free slot after the one just used, so each further device is one
/// more click.
/// Catches: a device form that forgets its rack or goes back to the first slot, and a switch sent as
/// a host.
#[test]
fn adding_a_device_keeps_the_form_on_the_next_item_and_the_next_free_slot() {
    let ms2 = on_tray(host("mac-studio-2"), 0);
    let ms3 = on_tray(host("mac-studio-3"), 1);
    let layouts = [
        layout_with(&[tray()], &[]),
        layout_with(&[tray()], std::slice::from_ref(&ms2)),
        layout_with(&[tray()], &[ms2.clone(), ms3.clone()]),
        layout_with(
            &[tray()],
            &[
                ms2,
                ms3,
                on_tray(
                    json!({"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.1"}),
                    2,
                ),
            ],
        ),
    ];
    let mut ui = start_with(&layouts);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-device-open", 1);
let now = last();
const forms = [{ title: one(now, "rack-form-title"), item: chosen(now, "new-device-item") }];
for (let i = 0; i < 3; i += 1) {
  await click(now, "add-device");
  now = await render(ctx);
  forms.push({ title: one(now, "rack-form-title"), item: chosen(now, "new-device-item") ?? null, said: one(now, "rack-form-said") });
}
return { forms, refreshes: seen.refreshes };"#,
    );
    assert_eq!(
        out["forms"],
        json!([
            {"title": "Add a device to Row 0 / Rack 1 on C0-R0-R1-S0-S0", "item": "host:mac-studio-2"},
            {"title": "Add a device to Row 0 / Rack 1 on C0-R0-R1-S0-S1", "item": "host:mac-studio-3",
             "said": "Added mac-studio-2 to Row 0 / Rack 1 at C0-R0-R1-S0-S0."},
            {"title": "Add a device to Row 0 / Rack 1 on C0-R0-R1-S0-S2", "item": "inventory:net.1",
             "said": "Added mac-studio-3 to Row 0 / Rack 1 at C0-R0-R1-S0-S1."},
            {"title": "Add a device to Row 0 / Rack 1 on C0-R0-R1-S0-S3", "item": null,
             "said": "Added sw-1 to Row 0 / Rack 1 at C0-R0-R1-S0-S2."},
        ])
    );
    assert_eq!(out["refreshes"], 3);
    assert_eq!(
        sent(&ui, "Layout/Place"),
        vec![
            json!({"subject": host("mac-studio-2"), "sled": "sled-9", "slot": 0, "reason": "placed mac-studio-2"}),
            json!({"subject": host("mac-studio-3"), "sled": "sled-9", "slot": 1, "reason": "placed mac-studio-3"}),
            json!({"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.1"}, "sled": "sled-9",
                   "slot": 2, "reason": "placed sw-1"}),
        ]
    );
}

/// Proves: the forms say in the form, by exact words, what cannot be added, and send nothing: a sled
/// height with no room in the rack (no position offered), a label over 32 characters, a rack with no
/// free slot (which offers the sled form instead), nothing waiting to be placed (which names
/// Inventory for a new switch); a coordinator refusal is told in the form in words, the page is not
/// read again and the inputs stay.
/// Catches: errors shown only in the status line far from the form, a refused add that clears the
/// form, and an add sent with no position.
#[test]
fn the_forms_tell_in_the_form_what_cannot_be_added_and_send_nothing() {
    let mut ui = reads_of(&[layout()], &hosts());
    ui.refuse(&m("Layout/PutSled"), 412, "FAILED_PRECONDITION", "overlap");
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-sled-open", 0);
let now = last();
choose(now, "new-sled-height", "3");
now = last();
const full = { title: one(now, "rack-form-title"), starts: choices(now, "new-sled-u"), room: one(now, "rack-form-room"),
  preview: one(now, "rack-form-preview") };
await click(now, "add-sled");
now = last();
full.error = one(now, "rack-form-error");
await click(now, "add-sled-open", 1);
now = last();
type(now, "new-sled-label", "x".repeat(33));
await click(now, "add-sled");
now = last();
const long = one(now, "rack-form-error");
type(now, "new-sled-label", "studio");
choose(now, "new-sled-slots", "2");
now = last();
await click(now, "add-sled");
now = last();
const refused = { error: one(now, "rack-form-error"), height: chosen(now, "new-sled-height"), slots: chosen(now, "new-sled-slots"),
  label: named(now, "new-sled-label")[0].attrs.value, refreshes: seen.refreshes };
await click(now, "add-device-open", 1);
now = last();
const noSlot = { title: one(now, "rack-form-title"), room: one(now, "rack-form-room"), slots: choices(now, "new-device-slot") };
await click(now, "add-device");
now = last();
noSlot.error = one(now, "rack-form-error");
await click(now, "to-sled-form");
noSlot.then = one(last(), "rack-form-title");
return { full, long, refused, noSlot, reports: seen.reports };"#,
    );
    assert_eq!(
        out["full"],
        json!({
            "title": "Add a sled to Row 0 / Rack 0",
            "starts": [],
            "room": "No room for a 3U sled in Row 0 / Rack 0: pick a smaller height, or make the rack taller.",
            "preview": null,
            "error": "No U of Row 0 / Rack 0 has room for a sled 3U high: pick a smaller height, or make the rack taller.",
        })
    );
    assert_eq!(out["long"], "A label is at most 32 characters.");
    assert_eq!(
        out["refused"],
        json!({
            "error": "That place is taken: another sled stands in one of those U, or another item on that slot.",
            "height": "3", "slots": "2", "label": "studio", "refreshes": 0,
        })
    );
    assert_eq!(
        out["noSlot"],
        json!({
            "title": "Add a device to Row 0 / Rack 1",
            "room": "Row 0 / Rack 1 has no free slot: add a sled first. Add a sled instead",
            "slots": [],
            "error": "Row 0 / Rack 1 has no free slot: add a sled first.",
            "then": "Add a sled to Row 0 / Rack 1 at U1-3",
        })
    );
    assert_eq!(out["reports"], json!([]));
    // One PutSled, the refused one at U1-3 of rack 1; nothing for the add with no room or the long label.
    assert_eq!(
        sent(&ui, "Layout/PutSled"),
        vec![
            json!({"id": "", "rack": "rack-2", "u": 1, "heightU": 3, "slots": 2, "label": "studio", "reason": "add sled"})
        ]
    );
    assert!(sent(&ui, "Layout/Place").is_empty());

    // Every server and switch on a sled: the device form says so and where a new switch is added.
    let placed = [
        on_tray(host("mac-studio-2"), 0),
        on_tray(host("mac-studio-3"), 1),
        on_tray(
            json!({"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "net.1"}),
            2,
        ),
    ];
    let mut ui = reads_of(&[layout_with(&[tray()], &placed)], &hosts());
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-device-open", 1);
let now = last();
const nothing = { note: one(now, "rack-form-nothing"), items: choices(now, "new-device-item"), preview: one(now, "rack-form-preview") };
await click(now, "add-device");
nothing.error = one(last(), "rack-form-error");
await click(last(), "to-inventory");
return { nothing, navigations: seen.navigations };"#,
    );
    assert_eq!(
        out["nothing"],
        json!({
            "note": "Every server and switch already stands on a sled. A new switch is added in Inventory. Add a switch",
            "items": [],
            "preview": null,
            "error": "Nothing waits to be placed: every server and switch stands on a sled.",
        })
    );
    assert_eq!(
        out["navigations"],
        json!([{"name": "inventory", "params": {"kind": "switch"}}])
    );
    assert!(sent(&ui, "Layout/Place").is_empty());
}

/// Proves: the stepper's + and - make the rack one U taller or shorter, and a height chosen from its
/// list sets that height, each with one PutRack keeping the rack's row; a shrink to exactly the top U
/// of its highest sled is allowed; the stepper's buttons stop at 1 U and at the 60 U limit.
/// Catches: a resize that drops the row, a stepper that steps by more than one U, and a shrink refused
/// at the boundary where nothing is cut.
#[test]
fn the_stepper_and_the_height_list_resize_the_rack_with_one_putrack_each() {
    let mut ui = start_with(&[layout()]);
    ui.reply(&m("Layout/PutRack"), json!({}));
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "rack-grow", 1);
await click(tree, "rack-shrink", 1);
choose(tree, "rack-height", "40", 1);
await settle();
choose(tree, "rack-height", "8", 0);
await settle();
const heights = choices(tree, "rack-height", 0);
return { reports: seen.reports, refreshes: seen.refreshes, errors: all(tree, "rack-size-error").length,
         first: heights[0], lastHeight: heights[heights.length - 1], count: heights.length,
         group: nodes(tree).filter((n) => n.attrs.class === "rack-size").map((n) => n.attrs["aria-label"]) };"#,
    );
    assert_eq!(
        out,
        json!({
            "reports": ["Row 0 / Rack 1 is now 13U.", "Row 0 / Rack 1 is now 11U.", "Row 0 / Rack 1 is now 40U.",
                        "Row 0 / Rack 0 is now 8U."],
            "refreshes": 4, "errors": 0, "first": "60U", "lastHeight": "1U", "count": 60,
            "group": ["Height of Row 0 / Rack 0", "Height of Row 0 / Rack 1"],
        })
    );
    let rack = |id: &str, height: u32| json!({"id": id, "row": "row-1", "heightU": height, "reason": "resize"});
    assert_eq!(
        sent(&ui, "Layout/PutRack"),
        vec![
            rack("rack-2", 13),
            rack("rack-2", 11),
            rack("rack-2", 40),
            rack("rack-1", 8)
        ]
    );

    // A rack of 1 U cannot step below it, one of 60 U not above it.
    let edge = json!({
        "rows": [{"id": "row-1"}],
        "racks": [
            {"id": "rack-1", "row": "row-1", "heightU": 1},
            {"id": "rack-2", "row": "row-1", "number": 1, "heightU": 60},
        ],
    });
    let mut ui = reads_of(&[edge], &hosts());
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"return { shrink: all(tree, "rack-shrink").map((n) => n.attrs.disabled), grow: all(tree, "rack-grow").map((n) => n.attrs.disabled) };"#,
    );
    assert_eq!(out, json!({"shrink": [true, false], "grow": [false, true]}));
}

/// Proves: a shrink that would cut a sled off the top of the rack is refused before any call, under
/// that rack, naming each sled cut, where it stands, what is on it and the least height the rack can
/// have (by exact words, one sled and two); a coordinator refusal of a resize is told under the rack
/// in words too; opening a form clears it.
/// Catches: a shrink sent that the coordinator would refuse with a bare token, or worse a message that
/// names no sled, and an error shown under the wrong rack.
#[test]
fn a_shrink_that_would_cut_a_sled_is_refused_naming_it_and_sends_nothing() {
    let mut ui = reads_of(&[layout()], &hosts());
    ui.refuse(
        &m("Layout/PutRack"),
        412,
        "FAILED_PRECONDITION",
        "out_of_rack",
    );
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
choose(tree, "rack-height", "7", 0);
await settle();
let now = last();
const racks = nodes(now).filter((n) => n.attrs.role === "group" && (n.attrs.class || "").split(" ").includes("rack"));
const under = racks.map((r) => all(r, "rack-size-error").map(words));
choose(now, "rack-height", "3", 0);
await settle();
const two = one(last(), "rack-size-error");
await click(last(), "rack-grow", 1);
await settle();
const coordinator = one(last(), "rack-size-error");
await click(last(), "add-sled-open", 0);
return { under, two, coordinator, cleared: one(last(), "rack-size-error"), reports: seen.reports, refreshes: seen.refreshes };"#,
    );
    assert_eq!(
        out,
        json!({
            "under": [["Row 0 / Rack 0 cannot be 7U: that would cut off the sled C0-R0-R0-S1 at U6-8 with mac-studio-1. Move or remove it first, or keep the rack at least 8U."], []],
            "two": "Row 0 / Rack 0 cannot be 3U: that would cut off the sleds C0-R0-R0-S0 at U1-4 with web-1; C0-R0-R0-S1 at U6-8 with mac-studio-1. Move or remove them first, or keep the rack at least 8U.",
            "coordinator": "That does not fit in the rack: it would reach past U 1 or the top U.",
            "cleared": null,
            "reports": [], "refreshes": 0,
        })
    );
    // Only the grow of rack 1 was sent (and refused); no shrink of rack 0.
    assert_eq!(
        sent(&ui, "Layout/PutRack"),
        vec![json!({"id": "rack-2", "row": "row-1", "heightU": 13, "reason": "resize"})]
    );
}

/// Proves: dragging the grip on a rack's bottom edge down makes the rack taller and up shorter, by
/// whole U of the drawing (16 px a U when the drawing is not measured), showing the height it would
/// set while it moves and marking it when that would cut a sled; on release one PutRack, or the
/// refusal naming the sled; the down and up arrow keys on the grip add or take one U, other keys
/// nothing.
/// Catches: a drag that resizes on every move, rounds a part of a U into a whole one wrongly, or
/// sends a shrink that cuts a sled.
#[test]
fn dragging_the_bottom_edge_resizes_the_rack_by_whole_u_and_the_arrow_keys_by_one() {
    let mut ui = start_with(&[layout()]);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const doc = mounted(tree);
const grips = doc.root.children.length === 0 ? [] : (function find(el, out) { if (el.nodeType === 1) { if (el.attributes["name"] === "rack-resize-grip") out.push(el); el.children.forEach((c) => find(c, out)); } return out; })(doc.root, []);
const label = (el) => el.querySelector(".grip-u").textContent;
const g1 = grips[1];
fire(g1, "pointerdown", { clientY: 100, currentTarget: g1 });
fire(g1, "pointermove", { clientY: 100 + 16 * 3 + 5, currentTarget: g1 });
const moving = { label: label(g1), dragging: g1.classList.classes.has("dragging"), cut: g1.classList.classes.has("cut") };
fire(g1, "pointerup", { clientY: 153, currentTarget: g1 });
await settle();
const afterUp = { dragging: g1.classList.classes.has("dragging") };
fire(g1, "pointermove", { clientY: 400, currentTarget: g1 });
const g0 = grips[0];
fire(g0, "pointerdown", { clientY: 100, currentTarget: g0 });
fire(g0, "pointermove", { clientY: 100 - 16 * 3, currentTarget: g0 });
const cutting = { label: label(g0), cut: g0.classList.classes.has("cut") };
fire(g0, "pointerup", { clientY: 52, currentTarget: g0 });
await settle();
const refused = one(last(), "rack-size-error");
const keys = [];
for (const key of ["ArrowDown", "ArrowUp", "Enter"]) {
  const e = { key, preventDefault: () => keys.push(key) };
  await named(tree, "rack-resize-grip")[1].on.keydown(e);
}
return { moving, afterUp, cutting, refused, keys, reports: seen.reports,
         aria: named(tree, "rack-resize-grip")[1].attrs["aria-label"] };"#,
    );
    assert_eq!(
        out,
        json!({
            "moving": {"label": "15U", "dragging": true, "cut": false},
            "afterUp": {"dragging": false},
            "cutting": {"label": "7U", "cut": true},
            "refused": "Row 0 / Rack 0 cannot be 7U: that would cut off the sled C0-R0-R0-S1 at U6-8 with mac-studio-1. Move or remove it first, or keep the rack at least 8U.",
            "keys": ["ArrowDown", "ArrowUp"],
            "reports": ["Row 0 / Rack 1 is now 15U.", "Row 0 / Rack 1 is now 13U.", "Row 0 / Rack 1 is now 11U."],
            "aria": "Resize Row 0 / Rack 1, now 12U: drag the bottom edge, or press the down arrow for one U more and the up arrow for one U less",
        })
    );
    let rack = |height: u32| json!({"id": "rack-2", "row": "row-1", "heightU": height, "reason": "resize"});
    assert_eq!(
        sent(&ui, "Layout/PutRack"),
        vec![rack(15), rack(13), rack(11)]
    );
}

/// Proves: the form stays open across the page's refreshes in the edit mode, and leaving the edit
/// mode closes it (coming back shows no form).
/// Catches: a form that lingers outside the edit mode, or comes back open on the next visit.
#[test]
fn leaving_the_edit_mode_closes_the_form() {
    let mut ui = start_with(&[layout(), layout(), layout(), layout()]);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-sled-open", 0);
const kept = one(await render(ctx), "rack-form-title");
ctx.params.edit = false;
const outside = one(await render(ctx), "rack-form-title");
ctx.params.edit = true;
const back = one(await render(ctx), "rack-form-title");
return { kept, outside, back };"#,
    );
    assert_eq!(
        out,
        json!({"kept": "Add a sled to Row 0 / Rack 0 at U5", "outside": null, "back": null})
    );
}

/// Proves: the model's rack helpers: the height a drag asks for (to the nearest U, never below 1 nor
/// above 60, no movement when the U cannot be measured), the U a sled takes in words, where the next
/// sled goes (at or above a U, else the lowest, else nowhere), what a height is refused for, and the
/// free slots of one rack.
/// Catches: off-by-one U in a span, a next position below the sled just added while there is room
/// above, and a shrink to the top U of a sled refused.
#[test]
fn the_lab_model_says_what_a_shrink_cuts_and_where_the_next_sled_goes() {
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ dragHeight, uSpan, nextSledStart, rackHeightProblem, freeSlotsIn, layoutOf }} from "./lab.js";
export async function main() {{
  const layout = layoutOf({{}});
  const lab = layoutOf({lab});
  return {{
    drag: [dragHeight(42, 33, 16), dragHeight(42, 23, 16), dragHeight(42, -1000, 16), dragHeight(42, 10000, 16), dragHeight(42, 50, 0)],
    span: [uSpan(5, 1), uSpan(5, 3)],
    next: [nextSledStart(lab, "rack-1", 1, 6), nextSledStart(lab, "rack-1", 1, 11), nextSledStart(lab, "rack-1", 3, 1),
           nextSledStart(lab, "rack-2", 2, 3), nextSledStart(layout, "rack-9", 1, 1)],
    problem: [rackHeightProblem(lab, "rack-2", 0), rackHeightProblem(lab, "rack-2", 61), rackHeightProblem(lab, "rack-2", 1.5),
              rackHeightProblem(lab, "rack-1", 8), rackHeightProblem(lab, "rack-2", 1)],
    slots: freeSlotsIn(lab, "rack-1").map((f) => `${{f.sled}} ${{f.slot}} ${{f.address}} U${{f.u}} ${{f.height}}U`),
    none: freeSlotsIn(lab, "rack-2"),
  }};
}}"#,
        lab = layout()
    ));
    assert_eq!(
        out,
        json!({
            "drag": [44, 43, 1, 60, 42],
            "span": ["U5", "U5-7"],
            "next": [9, 5, null, 3, null],
            "problem": ["A rack is 1 to 60U high.", "A rack is 1 to 60U high.", "A rack is 1 to 60U high.", null, null],
            "slots": ["sled-2 1 C0-R0-R0-S1-S1 U6 3U"],
            "none": [],
        })
    );
}

/// Proves: a form's inputs are kept from one rack to another but a position belongs to its rack: the
/// form of another rack starts at that rack's first free U, and the form of the first rack again at its
/// own first free U.
/// Catches: the next free U of one rack (just above a sled added there) carried to another rack, where
/// it is a valid but arbitrary U.
#[test]
fn a_position_belongs_to_its_rack_and_the_other_inputs_are_kept() {
    let mut ui = start_with(&[layout()]);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-sled-open", 1);
let now = last();
choose(now, "new-sled-slots", "4");
now = last();
choose(now, "new-sled-u", "9");
now = last();
const chosenThere = chosen(now, "new-sled-u");
await click(now, "add-sled-open", 0);
now = last();
const other = { title: one(now, "rack-form-title"), slots: chosen(now, "new-sled-slots") };
await click(now, "add-sled-open", 1);
now = last();
return { chosenThere, other, back: one(now, "rack-form-title"), slots: chosen(now, "new-sled-slots") };"#,
    );
    assert_eq!(
        out,
        json!({
            "chosenThere": "9",
            "other": {"title": "Add a sled to Row 0 / Rack 0 at U5", "slots": "4"},
            "back": "Add a sled to Row 0 / Rack 1 at U1",
            "slots": "4",
        })
    );
}

/// Proves: every draw makes new elements, so the control the operator used takes the focus back
/// once the page is drawn again: the stepper's button and the grip after a resize, a form's select
/// after a choice, Add sled after an add, and the + that opened the form after Close; no other
/// control of the page asks for it.
/// Catches: a keyboard user dropped to the top of the page after each U of a resize or each add.
#[test]
fn the_control_used_takes_the_focus_back_after_the_page_is_drawn_again() {
    let mut ui = start_with(&[layout(), layout(), layout(), layout()]);
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
const focusing = (t) => nodes(t).filter((n) => typeof n.on.mounted === "function" && n.tag !== "div" && n.tag !== "span")
  .map((n) => `${n.attrs.name}:${all(t, n.attrs.name).indexOf(n)}`);
await click(tree, "rack-grow", 1);
const grow = focusing(await render(ctx));
await named(tree, "rack-resize-grip")[0].on.keydown({ key: "ArrowDown", preventDefault: () => {} });
const grip = focusing(await render(ctx));
await click(tree, "add-sled-open", 1);
let now = last();
const opened = focusing(now);
choose(now, "new-sled-slots", "2");
now = last();
const choice = focusing(now);
await click(now, "add-sled");
now = await render(ctx);
const added = focusing(now);
await click(now, "close-rack-form");
const closed = focusing(last());
return { grow, grip, opened, choice, added, closed };"#,
    );
    assert_eq!(
        out,
        json!({
            "grow": ["rack-grow:1"],
            "grip": ["rack-resize-grip:0"],
            "opened": ["new-sled-height:0"],
            "choice": ["new-sled-slots:0"],
            "added": ["add-sled:0"],
            "closed": ["add-sled-open:1"],
        })
    );
}

/// Proves: a second click on Add sled while the first add is on its way sends nothing: one PutSled.
/// Catches: a double click that adds two sleds.
#[test]
fn a_second_click_while_an_add_is_on_its_way_sends_nothing() {
    let mut ui = reads_of(&[layout()], &hosts());
    ui.hold(&m("Layout/PutSled"), 200, "{}");
    let out = drive(
        &mut ui,
        r#"{ row: "row-1", edit: true }"#,
        r#"
await click(tree, "add-sled-open", 1);
const now = last();
const first = named(now, "add-sled")[0].on.click();
await settle();
await named(now, "add-sled")[0].on.click();
gatewayRelease("hostkeeper.operator.v1.Layout/PutSled");
await first;
return { refreshes: seen.refreshes, said: seen.reports };"#,
    );
    assert_eq!(
        out,
        json!({"refreshes": 1, "said": ["Added a 1U sled with 1 slot at Row 0 / Rack 1, U1."]})
    );
    assert_eq!(sent(&ui, "Layout/PutSled").len(), 1);
}
