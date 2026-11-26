//! The Add Xcode panel of the Xcode page (`ui/xcodeadd.js`) in the JavaScript engine against the
//! fake gateway of `support`: the command and the settings line, Copy through `ctx.copy` and its
//! refusal, the upload's progress and the stored state, and the frame's `ctx.origin` and
//! `ctx.copy`.

mod support;

use serde_json::json;
use support::Ui;
use support::pages::{bodies, m};
use support::xcode::*;

/// A driver that starts the whole app with `extra` (more members of the options of `startApp`)
/// and opens the Xcode page, then runs `steps`.
fn app(extra: &str, steps: &str) -> String {
    format!(
        r#"
import {{ fakeDocument, domNamed, domTexts, elements, fire, settle }} from "../test/kit.js";
import {{ startApp }} from "./app.js";
export async function main() {{
  const doc = fakeDocument();
  startApp({{ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000, {extra} }});
  await settle();
  const app = elements(doc.root);
  const menu = elements(doc.root).some((e) => e.attributes["name"] === "page-xcode");
  fire(domNamed(doc.root, "page-xcode"), "click", {{}});
  await settle();
  const text = () => domTexts(doc.root).join(" ");
  const click = async (name) => {{ fire(domNamed(doc.root, name), "click", {{}}); await settle(); }};
  {steps}
}}"#
    )
}

fn app_ui() -> Ui {
    let ui = Ui::start();
    ui.session("tok_a");
    // Home, where the app starts, reads the hosts first.
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(
        &m("Xcode/List"),
        store(archives(false), Some(waiting()), vec![]),
    );
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("Maintenance/List"), json!({}));
    ui
}

#[test]
fn the_panel_shows_the_command_for_the_waiting_release_and_the_settings_line_with_the_origin() {
    // Asserts: Add Xcode opens a panel headed "Add Xcode 27.3 (27C21)" whose command is exactly
    // `hk xcode add 27.3` and whose first-time line is exactly `hk settings init --endpoint
    // <ctx.origin> --ca-file <ca.pem> --token-file <token-file>` with the origin the context
    // gives; the panel reads Xcode.List every 10 seconds, and Close takes it away and stops that
    // read.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        r#"
ctx.origin = "https://console.example:8443";
await click(tree, "add-xcode");
const panel = named(cur(), "add-panel")[0];
const opened = {
  head: t(nodes(panel).find((n) => n.tag === "h3")),
  command: t(named(panel, "add-line")[0]),
  settings: t(named(panel, "settings-line")[0]),
  note: t(named(panel, "add-note")[0]),
  repeats: seen.repeats.map((r) => r.seconds),
  addButton: has(cur(), "add-xcode"),
};
await click(cur(), "close-add");
return { opened, closed: { panel: has(cur(), "add-panel"), stops: seen.repeats[1].stops, button: has(cur(), "add-xcode") } };"#,
    );
    assert_eq!(
        out["out"]["opened"],
        json!({
            "head": "Add Xcode 27.3 (27C21)",
            "command": "hk xcode add 27.3",
            "settings": "hk settings init --endpoint https://console.example:8443 --ca-file <ca.pem> --token-file <token-file>",
            "note": "It downloads 27.3 with xcodes (or takes ~/Downloads/Xcode_27.3.xip), checks Apple's signature and the build, and uploads it here. This page shows the upload as it arrives.",
            "repeats": [60, 10],
            "addButton": false,
        })
    );
    assert_eq!(
        out["out"]["closed"],
        json!({"panel": false, "stops": 1, "button": true})
    );
}

#[test]
fn a_page_with_no_origin_names_the_console_address_in_words() {
    // Asserts: with no `ctx.origin` the settings line names "<this console's address>" and no
    // address of its own.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        r#"
await click(tree, "add-xcode");
return t(named(cur(), "settings-line")[0]);"#,
    );
    assert_eq!(
        out["out"],
        "hk settings init --endpoint <this console's address> --ca-file <ca.pem> --token-file <token-file>"
    );
}

#[test]
fn with_nothing_waiting_the_panel_shows_the_bare_command_and_names_what_is_stored() {
    // Asserts: with no release waiting the panel is headed "Add Xcode", its command is exactly
    // `hk xcode add`, and its note says that the store holds the newest release and that the
    // newest archive in ~/Downloads is taken; an archive stored while it is open is named in the
    // stored line, since no release was named when it opened ("Stored. Xcode 27.4 (27D5). Due on
    // 1 Mac of g.").
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![mac(
            "m-1",
            "g",
            &[
                ("27.2", "27B52", true, false),
                ("27.1", "27A10", false, false),
            ],
            &[],
        )],
        vec![group("g", 2, None)],
        vec![],
    );
    ui.reply(
        &m("Xcode/List"),
        store(
            vec![
                archive(D, "27.4", "27D5"),
                archive(A, "27.2", "27B52"),
                archive(B, "27.1", "27A10"),
            ],
            None,
            vec![],
        ),
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
await click(tree, "add-xcode");
const panel = named(cur(), "add-panel")[0];
const opened = {
  head: t(nodes(panel).find((n) => n.tag === "h3")),
  command: t(named(panel, "add-line")[0]),
  note: t(named(panel, "add-note")[0]),
};
await seen.repeats[1].run();
return { opened, stored: t(named(cur(), "upload-stored")[0]) };"#,
    );
    assert_eq!(
        out["out"]["opened"],
        json!({
            "head": "Add Xcode",
            "command": "hk xcode add",
            "note": "No release waits: the store holds the newest release. `hk xcode add` takes the newest archive in ~/Downloads. This page shows the upload as it arrives.",
        })
    );
    assert_eq!(
        out["out"]["stored"],
        "Stored. Xcode 27.4 (27D5). Due on 1 Mac of g. Open the Maintenance page"
    );
}

#[test]
fn the_commands_are_made_from_the_release_and_the_origin() {
    // Asserts: the command is `hk xcode add` for no release and `hk xcode add 27.3` for one; the
    // settings line names the origin it is given, and the console's address in words for none.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { addCommand, settingsLine } from "./xcodeadd.js";
export async function main() {
  return { bare: addCommand(null), named: addCommand({ version: "27.3", build: "27C21" }),
           none: settingsLine(""), some: settingsLine("https://c.example") };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "bare": "hk xcode add",
            "named": "hk xcode add 27.3",
            "none": "hk settings init --endpoint <this console's address> --ca-file <ca.pem> --token-file <token-file>",
            "some": "hk settings init --endpoint https://c.example --ca-file <ca.pem> --token-file <token-file>",
        })
    );
}

#[test]
fn copy_gives_the_clipboard_exactly_the_line_and_says_so() {
    // Asserts: Copy of the command calls `ctx.copy` with exactly `hk xcode add 27.3`, and Copy of
    // the first-time line with exactly the settings line made from `ctx.origin`; each says
    // "Copied."; nothing says the clipboard refused.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        r#"
ctx.origin = "https://console.example:8443";
await click(tree, "add-xcode");
await click(cur(), "copy-add");
await click(cur(), "copy-settings");
return { refused: has(cur(), "add-refused") || has(cur(), "settings-refused") };"#,
    );
    assert_eq!(out["out"], json!({"refused": false}));
    assert_eq!(
        out["seen"]["copies"],
        json!([
            "hk xcode add 27.3",
            "hk settings init --endpoint https://console.example:8443 --ca-file <ca.pem> --token-file <token-file>"
        ])
    );
    assert_eq!(out["seen"]["reports"], json!(["Copied.", "Copied."]));
}

#[test]
fn a_refused_copy_selects_the_line_and_says_to_copy_it_by_hand() {
    // Asserts: when `ctx.copy` rejects, the command's line is selected when the page is shown
    // again (its `mounted` hook selects the text of the element it is given, through the document's
    // selection), the panel says "This browser did not allow copying: the line above is selected.
    // Copy it by hand.", "Copied." is not said, and the first-time line, not refused, is neither
    // marked nor selected.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        r#"
ctx.copyRefused = true;
await click(tree, "add-xcode");
await click(cur(), "copy-add");
const view = cur();
const selected = [];
const element = {
  ownerDocument: {
    createRange: () => ({ selectNodeContents: (node) => selected.push(["range", node]) }),
    defaultView: { getSelection: () => ({
      removeAllRanges: () => selected.push(["clear"]),
      addRange: (range) => selected.push(["add"]),
    }) },
  },
};
named(view, "add-line")[0].on.mounted({ currentTarget: element });
return {
  said: t(named(view, "add-refused")[0]),
  settingsMarked: has(view, "settings-refused"),
  settingsSelects: Object.keys(named(view, "settings-line")[0].on).includes("mounted"),
  steps: selected.map(([what, node]) => (node === element ? "element" : what)),
};"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "said": "This browser did not allow copying: the line above is selected. Copy it by hand.",
            "settingsMarked": false,
            "settingsSelects": false,
            "steps": ["element", "clear", "add"],
        })
    );
    assert_eq!(out["seen"]["copies"], json!(["hk xcode add 27.3"]));
    assert_eq!(out["seen"]["reports"], json!([]));
}

#[test]
fn a_copy_that_works_after_a_refusal_takes_the_refusal_away() {
    // Asserts: after a refused copy, a copy the clipboard takes says "Copied." and the refusal
    // text is gone.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        r#"
ctx.copyRefused = true;
await click(tree, "add-xcode");
await click(cur(), "copy-add");
const refused = has(cur(), "add-refused");
ctx.copyRefused = false;
await click(cur(), "copy-add");
return { refused, after: has(cur(), "add-refused") };"#,
    );
    assert_eq!(out["out"], json!({"refused": true, "after": false}));
    assert_eq!(out["seen"]["reports"], json!(["Copied."]));
}

#[test]
fn the_panel_shows_the_upload_as_it_arrives_then_the_stored_state_with_the_macs_it_is_due_on() {
    // Asserts: with the panel open, each read of Xcode.List that lists the upload of 27.3 says
    // "Uploading: 7.31 of 11.92 GB, last write 2 minutes ago" (by the page's clock), and the next
    // read that lists a new archive says "Stored. Due on 2 Macs of mac-build." (the Mac of
    // `mac-off`, which keeps no Xcode, is not counted) with a link that opens the Maintenance
    // page, and no longer says "Uploading".
    let mut ui = plain();
    ui.reply(
        &m("Xcode/List"),
        store(
            archives(false),
            Some(waiting()),
            vec![upload("7310000000", "ana")],
        ),
    );
    ui.reply(
        &m("Xcode/List"),
        store(
            vec![
                archive(D, "27.3", "27C21"),
                archive(A, "27.2", "27B52"),
                archive(B, "27.1", "27A10"),
            ],
            None,
            vec![],
        ),
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
await click(tree, "add-xcode");
const before = has(cur(), "upload-progress");
await seen.repeats[1].run();
const during = t(named(cur(), "upload-progress")[0]);
const storedDuring = has(cur(), "upload-stored");
await seen.repeats[1].run();
const stored = t(named(cur(), "upload-stored")[0]);
const after = has(cur(), "upload-progress");
await click(cur(), "open-maintenance");
return { before, during, storedDuring, stored, after, navigations: seen.navigations };"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "before": false,
            "during": "Uploading: 7.31 of 11.92 GB, last write 2 minutes ago",
            "storedDuring": false,
            "stored": "Stored. Due on 2 Macs of mac-build. Open the Maintenance page",
            "after": false,
            "navigations": [{"name": "maintenance", "params": {}}],
        })
    );
    let reads = bodies(&ui)
        .iter()
        .filter(|(method, _)| *method == m("Xcode/List"))
        .count();
    assert_eq!(reads, 3, "the open and two reads of the panel");
    let others: Vec<String> = bodies(&ui)
        .into_iter()
        .map(|(method, _)| method)
        .filter(|method| *method != m("Xcode/List"))
        .collect();
    assert_eq!(
        others,
        [m("Hosts/List"), m("HostGroups/List"), m("Maintenance/List")],
        "the panel's reads call Xcode.List alone"
    );
}

#[test]
fn a_stored_release_due_on_no_mac_says_so() {
    // Asserts: when the archive stored while the panel is open is due on no Mac (no Mac of a
    // group that keeps it), the panel says "Stored. Due on no Mac."
    let mut ui = fleet(
        store(archives(false), Some(waiting()), vec![]),
        vec![mac("m-3", "mac-off", &[], &[])],
        vec![group("mac-off", 0, None)],
        vec![],
    );
    ui.reply(
        &m("Xcode/List"),
        store(vec![archive(D, "27.3", "27C21")], None, vec![]),
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
await click(tree, "add-xcode");
await seen.repeats[1].run();
return t(named(cur(), "upload-stored")[0]);"#,
    );
    assert!(
        out["out"]
            .as_str()
            .expect("text")
            .starts_with("Stored. Due on no Mac."),
        "{}",
        out["out"]
    );
}

#[test]
fn the_frame_lists_the_page_in_the_menu_and_gives_it_the_origin_and_the_clipboard() {
    // Asserts: the Manage menu has the Xcode page; the frame gives the page the origin it was
    // started with (in the settings line) and a `ctx.copy` that is the function it was started
    // with, called with exactly the command's line.
    let mut ui = app_ui();
    let out = ui.run(&app(
        r#"origin: "https://console.example:8443", copy: async (text) => { globalThis.copied = text; }"#,
        r#"
await click("add-xcode");
await click("copy-add");
return { menu, copied: globalThis.copied, line: domTexts(domNamed(doc.root, "settings-line")).join(""), said: text().includes("Copied.") };"#,
    ));
    assert_eq!(
        out,
        json!({
            "menu": true,
            "copied": "hk xcode add 27.3",
            "line": "hk settings init --endpoint https://console.example:8443 --ca-file <ca.pem> --token-file <token-file>",
            "said": true,
        })
    );
}

#[test]
fn a_frame_started_with_no_clipboard_or_origin_refuses_the_copy_and_names_no_address() {
    // Asserts: with no `copy` and no `origin` given to the frame, Copy ends in the refusal text
    // (the page does not throw) and the settings line names the console's address in words.
    let mut ui = app_ui();
    let out = ui.run(&app(
        "",
        r#"
await click("add-xcode");
await click("copy-add");
return { refused: text().includes("This browser did not allow copying"),
         line: domTexts(domNamed(doc.root, "settings-line")).join("") };"#,
    ));
    assert_eq!(
        out,
        json!({
            "refused": true,
            "line": "hk settings init --endpoint <this console's address> --ca-file <ca.pem> --token-file <token-file>",
        })
    );
}
