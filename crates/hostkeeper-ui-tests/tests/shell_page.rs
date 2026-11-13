//! The frame around the pages (`ui/app.js`): the side menu and its groups, the wordmark that goes
//! Home, the menu collapsed to icons and as a drawer, the top bar with the page's place, the user
//! and the sign-out, the theme toggle, and the routes that moved. The whole app in a fake document
//! over the fake gateway.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

const CSRF: &str = "csrf_5b2e91c7d4a06f38";

/// Starts the app with a live session, a lab with one row, and a theme port that records what it
/// is told; the document is kept as `globalThis.doc`, the app as `globalThis.theApp`.
fn start() -> Ui {
    let mut ui = Ui::start();
    ui.session(CSRF);
    for _ in 0..8 {
        ui.reply(&m("Hosts/List"), json!({"hosts": []}));
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(
            &m("Layout/Get"),
            json!({"name": "Lab 1", "rows": [{"id": "row-1", "number": 1}]}),
        );
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
    }
    ui.run(
        r#"
import { fakeDocument, settle } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  globalThis.doc = doc;
  globalThis.applied = [];
  globalThis.theApp = startApp({
    doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    prefs: { apply: (theme) => globalThis.applied.push(theme) },
  });
  await settle();
}"#,
    );
    ui
}

/// Runs `steps` (JavaScript with `doc`, `fire`, `domNamed`, `elements`, `domTexts`, `settle`,
/// `text`, `names`, `click` and `frame` in scope) and returns what they return.
fn steps(ui: &mut Ui, steps: &str) -> Value {
    ui.run(&format!(
        r#"
import {{ domNamed, domTexts, elements, fire, settle }} from "../test/kit.js";
export async function main() {{
  const doc = globalThis.doc;
  const text = () => domTexts(doc.root).join(" ").replace(/\s+/g, " ");
  const names = () => elements(doc.root).map((e) => e.attributes["name"]).filter((n) => n !== undefined);
  const click = async (name) => {{ fire(domNamed(doc.root, name), "click", {{}}); await settle(); }};
  const frame = () => elements(doc.root).find((e) => (e.attributes["class"] || "").split(" ").includes("frame"));
  const classes = () => frame().attributes["class"];
  {steps}
}}"#
    ))
}

#[test]
fn the_menu_has_the_wordmark_and_the_groups_and_no_fleet_release_rollout_or_holds() {
    // Catches: a menu that still lists a page that moved (Fleet, Release, Rollout, Holds), one that
    // loses an Admin page, a wordmark that is not a button to Home, and a menu that does not say
    // which page it is on.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
const first = names();
const home = text();
await click("page-settings");
const tabs = names().filter((n) => n.startsWith("page-") && n !== "page-settings" && !["page-hostgroups", "page-packages", "page-inventory", "page-enrollment"].includes(n));
await click("page-audit");
const current = elements(doc.root)
  .filter((e) => e.attributes["aria-current"] === "page")
  .map((e) => e.attributes["name"]);
const crumb = elements(doc.root).find((e) => e.attributes["class"] === "crumb");
return { first, home, tabs, current, crumb: domTexts(crumb).join("") };"#,
    );
    let names: Vec<&str> = out["first"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let pages: Vec<&&str> = names.iter().filter(|n| n.starts_with("page-")).collect();
    assert_eq!(
        pages,
        [
            &"page-hostgroups",
            &"page-packages",
            &"page-inventory",
            &"page-enrollment",
            &"page-settings"
        ]
    );
    assert_eq!(names[0], "home", "the wordmark leads the menu");
    for settings in ["page-users", "page-tokens", "page-mdm", "page-audit"] {
        assert!(
            !names.contains(&settings),
            "{settings} is in the menu, not under Settings"
        );
    }
    assert_eq!(
        out["tabs"],
        json!(["page-users", "page-tokens", "page-mdm", "page-audit"]),
        "Settings opens with its tabs"
    );
    for foot in ["theme", "sign-out", "collapse"] {
        assert!(names.contains(&foot), "{foot} is at the bottom of the menu");
    }
    for gone in [
        "page-fleet",
        "page-release",
        "page-rollout",
        "page-holds",
        "page-host",
    ] {
        assert!(!names.contains(&gone), "{gone} is in the menu");
    }
    assert!(
        out["home"].as_str().unwrap().contains("Lab 1"),
        "Home is the first page"
    );
    assert_eq!(out["current"], json!(["page-settings", "page-audit"]));
    assert_eq!(out["crumb"], "Settings");
}

#[test]
fn the_wordmark_goes_home_and_the_old_fleet_route_lands_there() {
    // Catches: a wordmark that does nothing, `fleet` (a bookmark, a doc, a habit) that is no more,
    // and an old route that arrives with another page's parameters.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
await click("page-settings");
await click("page-audit");
const audit = text();
await click("home");
const home = text();
globalThis.theApp.navigate("audit", {});
await settle();
globalThis.theApp.navigate("fleet", { row: "row-1" });
await settle();
const fleet = text();
const crumbs = elements(doc.root).filter((e) => e.attributes["class"] === "crumb").length;
return { audit: audit.includes("Audit"), home: home.includes("Lab 1") && home.includes("Row 1"), fleet: fleet.includes("Lab 1") && fleet.includes("Row 1"), crumbs };"#,
    );
    assert_eq!(out["audit"], true);
    assert_eq!(out["home"], true);
    assert_eq!(out["fleet"], true);
    assert_eq!(
        out["crumbs"], 0,
        "Home has its own breadcrumbs, so the top bar has none"
    );
}

#[test]
fn the_old_release_pages_still_open_by_their_route_but_have_no_menu_entry() {
    // Catches: a route that is dropped from the app because its entry left the menu, which breaks
    // every link to it until the package page takes its place.
    let mut ui = start();
    ui.reply(&m("Releases/Status"), json!({}));
    let out = steps(
        &mut ui,
        r#"
const shown = [];
for (const route of ["release", "rollout", "holds", "host"]) {
  globalThis.theApp.navigate(route, {});
  await settle();
  const crumb = elements(doc.root).find((e) => e.attributes["class"] === "crumb");
  shown.push(crumb === undefined ? null : domTexts(crumb).join(""));
}
return shown;"#,
    );
    assert_eq!(
        out,
        json!(["Release", "Rollout", "Holds", null]),
        "the host page has the lab breadcrumbs and no top-bar crumb"
    );
}

#[test]
fn the_menu_collapses_to_icons_and_opens_as_a_drawer_and_keeps_the_page() {
    // Catches: a collapse that is not told to a screen reader, a drawer that stays open after a
    // choice, a scrim that does not close it, and a redraw that loses the page.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
const expanded = () => domNamed(doc.root, "collapse").attributes["aria-expanded"];
const start = { classes: classes(), expanded: expanded() };
await click("collapse");
const collapsed = { classes: classes(), expanded: expanded(), keeps: text().includes("Lab 1") };
await click("collapse");
const back = classes();
await click("menu");
const open = { classes: classes(), expanded: domNamed(doc.root, "menu").attributes["aria-expanded"] };
await click("scrim");
const closed = classes();
await click("menu");
await click("page-settings");
const chosen = classes();
return { start, collapsed, back, open, closed, chosen };"#,
    );
    assert_eq!(
        out["start"],
        json!({"classes": "frame", "expanded": "true"})
    );
    assert_eq!(
        out["collapsed"],
        json!({"classes": "frame collapsed", "expanded": "false", "keeps": true})
    );
    assert_eq!(out["back"], "frame");
    assert_eq!(
        out["open"],
        json!({"classes": "frame drawer-open", "expanded": "true"})
    );
    assert_eq!(out["closed"], "frame");
    assert_eq!(out["chosen"], "frame", "a choice closes the drawer");
}

#[test]
fn the_theme_toggle_cycles_system_light_dark_through_the_theme_port() {
    // Catches: a theme that is applied to the page but not kept across a redraw, or a cycle that
    // skips a theme; the app starts on the browser's own.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
const label = () => text().match(/Theme \w+/)[0];
const seen = [label()];
for (let i = 0; i < 4; i++) {
  await click("theme");
  seen.push(label());
}
return { seen, applied: globalThis.applied };"#,
    );
    assert_eq!(
        out["seen"],
        json!([
            "Theme system",
            "Theme light",
            "Theme dark",
            "Theme system",
            "Theme light"
        ])
    );
    assert_eq!(
        out["applied"],
        json!(["system", "light", "dark", "system", "light"])
    );
}

#[test]
fn the_top_bar_names_the_user_and_the_menu_signs_out() {
    // Catches: a top bar that does not say who is signed in, or a sign-out that leaves the frame.
    let mut ui = start();
    ui.reply_raw(support::LOGOUT, 200, "{}");
    let out = steps(
        &mut ui,
        r#"
const who = domTexts(domNamed(doc.root, "who")).join("");
await click("sign-out");
return { who, after: text().includes("Sign in with your user name and password.") };"#,
    );
    assert_eq!(out["who"], "operator");
    assert_eq!(out["after"], true);
}

#[test]
fn every_menu_entry_has_a_drawn_icon_in_the_svg_namespace_and_no_letters() {
    // Catches: an icon made as an html element (it would not draw), a glyph of two letters left in
    // a button, and an icon a screen reader would read out.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
const side = elements(doc.root).find((e) => e.tagName === "ASIDE");
const buttons = elements(side).filter((e) => e.tagName === "BUTTON");
const icons = buttons.map((b) => elements(b).filter((e) => e.tagName === "SVG"));
const first = icons[0][0];
return {
  buttons: buttons.length,
  oneEach: icons.every((i) => i.length === 1),
  namespace: first.namespace,
  hidden: first.attributes["aria-hidden"],
  strokes: elements(first).filter((e) => e.tagName !== "SVG").every((e) => e.namespace === first.namespace),
  glyphs: elements(side).filter((e) => e.attributes["class"] === "glyph").map((g) => domTexts(g).join("")),
};"#,
    );
    assert_eq!(out["oneEach"], true);
    assert!(out["buttons"].as_u64().unwrap() >= 9);
    assert_eq!(out["namespace"], "http://www.w3.org/2000/svg");
    assert_eq!(out["hidden"], "true");
    assert_eq!(out["strokes"], true);
    assert!(
        out["glyphs"].as_array().unwrap().iter().all(|g| g == ""),
        "no letters in a glyph: {}",
        out["glyphs"]
    );
}

#[test]
fn the_settings_tabs_open_their_pages_and_the_theme_button_names_its_setting() {
    // Catches: a tab that does not switch the page, a Settings entry that is not marked current
    // under a tab, and a theme control whose state is only in its icon.
    let mut ui = start();
    let out = steps(
        &mut ui,
        r#"
await click("page-settings");
const users = text().includes("Users");
await click("page-mdm");
const current = elements(doc.root).filter((e) => e.attributes["aria-current"] === "page").map((e) => e.attributes["name"]);
const theme = domNamed(doc.root, "theme").attributes["title"];
return { users, current, theme };"#,
    );
    assert_eq!(out["users"], true);
    assert_eq!(out["current"], json!(["page-settings", "page-mdm"]));
    assert_eq!(out["theme"], "Theme: system. Click for the next.");
}
