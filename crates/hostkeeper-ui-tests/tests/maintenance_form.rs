//! More of the Schedule form and the Update controls (`maintenance_schedule.rs` has the main
//! cases): a second Check, a kind that falls due while the form is open, the 32-host limit, the
//! converted times of a window kept up to date without drawing the page again, the cards and
//! pages that have no group, no report or nothing due, and the words of kinds and skips this page
//! does not know. Run in the JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

const MDM: &str = "mdm_conflict: a device-management setting hostkeeper must not update under";

/// A report of a Mac with `due` listed.
fn report(due: Vec<Value>) -> Value {
    json!({
        "host": {"osName": "macOS", "osVersion": "26.1", "osBuild": "25B78", "runningKernel": "25.1.0",
                 "bootId": "b-1"},
        "due": due,
        "lookedUnix": "1799999400",
    })
}

fn macos_due() -> Value {
    json!({"kind": "MAINTENANCE_KIND_MACOS", "from": "26.1", "to": "26.2", "needsReboot": true,
           "label": "macOS Tahoe 26.2-25C56"})
}

fn xcode_due() -> Value {
    json!({"kind": "MAINTENANCE_KIND_XCODE", "to": "27.1", "label": "Xcode 27.1"})
}

/// A host of `group` ("": no group) with `due` listed in its report.
fn host(node_id: &str, group: &str, due: Vec<Value>) -> Value {
    let mut out =
        json!({"nodeId": node_id, "maintenance": report(due), "maintenanceLookAgeSecs": "600"});
    if !group.is_empty() {
        out["hostGroup"] = json!(group);
    }
    out
}

/// The Maintenance page over `hosts`, with one read of the runs (none).
fn page_ui(hosts: Vec<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({ "hosts": hosts }));
    ui.reply(&m("Maintenance/List"), json!({}));
    ui
}

/// HostGroups.Get of `mac-build`, with an empty policy.
fn group_known(ui: &Ui) {
    ui.reply(
        &m("HostGroups/Get"),
        json!({"hostGroup": {"group": {"name": "mac-build", "packages": ["hostkeeper"], "policy": {}}}}),
    );
}

fn schedules(ui: &Ui) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Maintenance/Schedule"))
        .map(|(_, body)| body)
        .collect()
}

/// A turn of the answer to a Schedule of `mac-build`: waiting, or skipped for a device-management
/// conflict.
fn turn(host_id: &str, skipped: bool) -> Value {
    let mut out =
        json!({"hostId": host_id, "hostGroup": "mac-build", "kinds": ["MAINTENANCE_KIND_MACOS"]});
    if skipped {
        out["step"] = json!("MAINTENANCE_HOST_STEP_SKIPPED");
        out["skippedBecause"] = json!("MAINTENANCE_SKIP_MDM_CONFLICT");
        out["kinds"] = json!([]);
    } else {
        out["step"] = json!("MAINTENANCE_HOST_STEP_WAITING");
    }
    out
}

#[test]
fn a_second_check_asks_again_for_a_host_the_first_left_out() {
    // Asserts: with "the hosts ticked" chosen over m-1 and m-2, a Check names both; the answer
    // leaves m-2 out for a device-management conflict, which is shown as a line with no tick; a
    // second Check names both again (it is not told m-2 is out), and its answer, which takes
    // both, shows two ticks and no line of a host left out. Each Check says how many hosts it
    // would take and how many it skips.
    let mut ui = page_ui(vec![
        host("m-1", "mac-build", vec![macos_due()]),
        host("m-2", "mac-build", vec![macos_due()]),
    ]);
    group_known(&ui);
    let run = json!({"state": "MAINTENANCE_STATE_SCHEDULED", "hostGroup": "mac-build"});
    ui.reply(
        &m("Maintenance/Schedule"),
        json!({"run": run, "hosts": [turn("m-1", false), turn("m-2", true)]}),
    );
    ui.reply(
        &m("Maintenance/Schedule"),
        json!({"run": run, "hosts": [turn("m-1", false), turn("m-2", false)]}),
    );
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const picks = () => nodes(cur()).filter((n) => n.attrs["name"] === "pick").length;
const left = () => nodes(cur()).filter((n) => n.attrs["name"] === "skipped-host").map(text);
await click(tree, "open-schedule", 0);
choose(cur(), "where", "hosts", 0);
await click(cur(), "schedule-check");
const first = { picks: picks(), left: left() };
await click(cur(), "schedule-check");
return { first, second: { picks: picks(), left: left() } };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "first": {"picks": 1, "left": [format!("m-2: left out ({MDM})")]},
            "second": {"picks": 2, "left": []},
        })
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Checked: 1 hosts would be taken, 1 skipped.",
            "Checked: 2 hosts would be taken, 0 skipped.",
        ])
    );
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["m-1", "m-2"]}),
            json!({"hostIds": ["m-1", "m-2"]})
        ]
    );
}

#[test]
fn a_kind_that_falls_due_while_the_form_is_open_is_ticked_and_asked_for() {
    // Asserts: a form opened when only macOS is due shows one kind, ticked; when the hosts are
    // read again and an Xcode update is due too, the form shows two kinds, both ticked (the new
    // one is not left unticked), and a Check sends no `kinds` (every kind due), not macOS alone.
    let mut ui = page_ui(vec![host("m-1", "mac-build", vec![macos_due()])]);
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [host("m-1", "mac-build", vec![macos_due(), xcode_due()])]}),
    );
    group_known(&ui);
    ui.reply(&m("Maintenance/Schedule"), json!({}));
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const kinds = () => named(cur(), "kind").map((n) => n.attrs["checked"]);
await click(tree, "open-schedule", 0);
const before = kinds();
await seen.repeats[0].run();
const after = kinds();
await click(cur(), "schedule-check");
return { before, after };"#,
    ));
    assert_eq!(out["out"], json!({"before": [true], "after": [true, true]}));
    assert_eq!(schedules(&ui), [json!({"hostGroup": "mac-build"})]);
}

#[test]
fn more_than_32_hosts_ticked_is_told_and_sends_nothing_until_one_is_unticked() {
    // Asserts: with 33 hosts ticked, Check and Schedule say "Tick at most 32 hosts, or choose the
    // group." and call nothing (the API takes 1 to 32 host ids); with one unticked, a Check sends
    // the other 32.
    let hosts: Vec<Value> = (1..=33)
        .map(|n| host(&format!("m-{n:02}"), "mac-build", vec![macos_due()]))
        .collect();
    let mut ui = page_ui(hosts);
    group_known(&ui);
    ui.reply(&m("Maintenance/Schedule"), json!({}));
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
choose(cur(), "where", "hosts", 0);
type(cur(), "schedule-reason", "macOS 26.2");
await click(cur(), "schedule-check");
await click(cur(), "schedule-confirm");
tick(cur(), "pick", false, 0);
await click(cur(), "schedule-check");
return null;"#,
    ));
    let tell = "Tick at most 32 hosts, or choose the group.";
    assert_eq!(
        out["seen"]["reports"],
        json!([tell, tell, "Checked: 0 hosts would be taken, 0 skipped."])
    );
    assert_eq!(out["seen"]["questions"], json!([]));
    let sent = schedules(&ui);
    assert_eq!(sent.len(), 1);
    let ids: Vec<String> = (2..=33).map(|n| format!("m-{n:02}")).collect();
    assert_eq!(sent[0], json!({"hostIds": ids, "reason": "macOS 26.2"}));
}

#[test]
fn a_window_field_updates_the_converted_times_in_place_and_does_not_draw_the_page_again() {
    // Asserts: committing a window field (its `change`) puts the converted times, "Start
    // 2027-01-16 03:00:00 UTC; end 2027-01-16 07:30:00 UTC" for 22:00 to 02:30 in the fake
    // zone UTC-05:00, into the tree and into the element of the page found from the field's
    // nearest fieldset by `[name=window-utc]`, and draws the page no further time (a drawn page
    // would take the focus from the field the operator moves to).
    let mut ui = page_ui(vec![host("m-1", "mac-build", vec![macos_due()])]);
    group_known(&ui);
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
choose(cur(), "when", "window", 0);
const drawn = seen.shown.length;
type(cur(), "window-start", "2027-01-15T22:00");
type(cur(), "window-end", "2027-01-16T02:30");
const live = { textContent: "" };
const asked = [];
const target = {
  closest: (selector) => {
    asked.push(selector);
    return { querySelector: (found) => { asked.push(found); return live; } };
  },
};
named(cur(), "window-end")[0].on.change({ target });
return { redrawn: seen.shown.length - drawn, live: live.textContent, tree: text(named(cur(), "window-utc")[0]), asked };"#,
    ));
    let said = "Start 2027-01-16 03:00:00 UTC; end 2027-01-16 07:30:00 UTC";
    assert_eq!(
        out["out"],
        json!({"redrawn": 0, "live": said, "tree": said, "asked": ["fieldset", "[name=window-utc]"]})
    );
}

#[test]
fn a_host_with_no_report_has_no_update_controls_on_its_updates_tab() {
    // Asserts: the host page of a host whose daemon sends no maintenance report has no Update
    // button, no Reason field for it and no Schedule form (there is nothing to say is due).
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "h-1", "hostGroup": "g"}}),
    );
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        r#"
return ["update", "update-reason", "schedule-form"].map((name) => nodes(tree).filter((n) => n.attrs["name"] === name).length);"#,
    ));
    assert_eq!(out["out"], json!([0, 0, 0]));
}

#[test]
fn a_form_over_hosts_with_nothing_due_says_so_and_asks_for_no_kind() {
    // Asserts: the form of a group whose hosts report nothing due has no kind checkbox and says
    // "Nothing is reported due." in a note of its own (`schedule-nothing-due`, not the Updates
    // panel's `nothing-due`); a Check then sends the group with no `kinds`.
    let mut ui = page_ui(vec![host("m-1", "mac-build", vec![])]);
    group_known(&ui);
    ui.reply(&m("Maintenance/Schedule"), json!({}));
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
await click(cur(), "schedule-check");
return {
  note: named(cur(), "schedule-nothing-due").map(text),
  kinds: nodes(cur()).filter((n) => n.attrs["name"] === "kind").length,
  clash: nodes(cur()).filter((n) => n.attrs["name"] === "nothing-due").length,
};"#,
    ));
    assert_eq!(
        out["out"],
        json!({"note": ["Nothing is reported due."], "kinds": 0, "clash": 0})
    );
    assert_eq!(schedules(&ui), [json!({"hostGroup": "mac-build"})]);
}

#[test]
fn the_card_of_the_hosts_in_no_group_has_no_group_button_and_a_form_over_the_hosts_ticked() {
    // Asserts: the Unassigned card has the Update button of its due host and no "Update the
    // group" button; opening its form reads no host group (no HostGroups.Get), chooses "the hosts
    // ticked" and has no "every host of" choice, ticks the due host, and labels the batch and
    // the drain deadline "the group's default".
    let mut ui = page_ui(vec![host("u-1", "", vec![macos_due()])]);
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const buttons = (t) => nodes(t).filter((n) => n.tag === "button").map(text);
const before = buttons(tree);
await click(tree, "open-schedule", 0);
const labels = nodes(cur()).filter((n) => n.tag === "label").map((n) => text(n).trim());
return {
  groupButton: before.filter((words) => words === "Update the group").length,
  rowButtons: named(tree, "update").length,
  where: chosen(cur(), "where", 0),
  options: choices(cur(), "where", 0),
  picks: named(cur(), "pick").map((n) => n.attrs["checked"]),
  batch: labels.filter((words) => words.startsWith("Batch")),
  deadline: labels.filter((words) => words.startsWith("Drain deadline")),
};"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "groupButton": 0,
            "rowButtons": 1,
            "where": "hosts",
            "options": ["the hosts ticked"],
            "picks": [true],
            "batch": ["Batch (the group's default)"],
            "deadline": ["Drain deadline, seconds (the group's default)"],
        })
    );
    assert_eq!(ui.methods(), [m("Hosts/List"), m("Maintenance/List")]);
}

#[test]
fn a_group_whose_policy_cannot_be_read_still_opens_its_form_and_the_form_closes_again() {
    // Asserts: when HostGroups.Get is refused the form opens, its batch and drain deadline are
    // labelled "the group's default" and "every host of mac-build" is the default place; the
    // button then reads "Close the form" and a second click removes the form and reads
    // "Schedule..." again.
    let mut ui = page_ui(vec![host("m-1", "mac-build", vec![macos_due()])]);
    ui.refuse(&m("HostGroups/Get"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const state = (t) => ({ form: nodes(t).filter((n) => n.attrs["name"] === "schedule-form").length, button: text(named(t, "open-schedule")[0]) });
const closed = state(tree);
await click(tree, "open-schedule", 0);
const labels = nodes(cur()).filter((n) => n.tag === "label").map((n) => text(n).trim());
const opened = {
  ...state(cur()),
  where: chosen(cur(), "where", 0),
  batch: labels.filter((words) => words.startsWith("Batch")),
};
await click(cur(), "open-schedule", 0);
return { closed, opened, again: state(cur()) };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "closed": {"form": 0, "button": "Schedule..."},
            "opened": {"form": 1, "button": "Close the form", "where": "group",
                       "batch": ["Batch (the group's default)"]},
            "again": {"form": 0, "button": "Schedule..."},
        })
    );
}

#[test]
fn a_check_with_a_reason_the_api_would_refuse_says_so_and_calls_nothing() {
    // Asserts: a Check whose reason has a tab says "A reason has no control character (a tab or a
    // line break, for example)." and sends nothing.
    let mut ui = page_ui(vec![host("m-1", "mac-build", vec![macos_due()])]);
    group_known(&ui);
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
type(cur(), "schedule-reason", "a\tb");
await click(cur(), "schedule-check");
return null;"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!(["A reason has no control character (a tab or a line break, for example)."])
    );
    assert_eq!(schedules(&ui), Vec::<Value>::new());
}

#[test]
fn kinds_and_skips_this_page_does_not_know_are_said_by_their_lower_cased_names() {
    // Asserts: an unknown kind is its lower-cased name and follows the known kinds in the order
    // first seen; an unknown skip is its lower-cased name; an absent skip or the unspecified one
    // is the empty words.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { dueKinds, kindWords, skipWords } from "./schedule.js";
export async function main() {
  const due = (...kinds) => ({ maintenance: { due: kinds.map((kind) => ({ kind })) } });
  return {
    kind: kindWords("MAINTENANCE_KIND_FIRMWARE"),
    skip: skipWords("MAINTENANCE_SKIP_DISK_FULL"),
    none: [skipWords(undefined), skipWords(""), skipWords("MAINTENANCE_SKIP_UNSPECIFIED")],
    order: dueKinds([due("MAINTENANCE_KIND_FIRMWARE", "MAINTENANCE_KIND_XCODE"), due("MAINTENANCE_KIND_MACOS")]),
  };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "kind": "firmware",
            "skip": "disk_full",
            "none": ["", "", ""],
            "order": ["MAINTENANCE_KIND_MACOS", "MAINTENANCE_KIND_XCODE", "MAINTENANCE_KIND_FIRMWARE"],
        })
    );
}
