//! What an operator starts from the maintenance pages: the Update button (the host page's Updates
//! tab, a due host's row and a group's line on the Maintenance page, a group's line on Home) and
//! the Schedule form (its window in the operator's time zone, the kinds, the hosts, the batch and
//! the drain deadline, the Check's preview), run in the JavaScript engine against the fake gateway
//! of `support`. The page context answers the fake zone UTC-05:00 (`TEST_ZONE_OFFSET` of the kit)
//! unless a driver gives another.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// The second sentence of every question the buttons ask.
const TAIL: &str = "It drains, updates, restarts if needed, puts back.";

/// What a stored run of one waiting host is reported as.
const WAITING_ONE: &str = "Scheduled run r-7: 1 host waiting";

/// What a Check that answers no host is reported as.
const CHECKED_NONE: &str = "Checked: 0 hosts would be taken, 0 skipped.";

const MDM: &str = "mdm_conflict: a device-management setting hostkeeper must not update under";
const OLD_DAEMON: &str = "older daemon: it does not announce maintenance-v1";

/// A Linux kernel update: the new kernel package, and the reboot the newer kernel needs.
fn kernel_due() -> Vec<Value> {
    vec![
        json!({"kind": "MAINTENANCE_KIND_LINUX", "from": "6.8.0-45-generic", "to": "6.8.0-47-generic",
               "needsReboot": true, "label": "linux-image-generic", "security": true}),
        json!({"kind": "MAINTENANCE_KIND_REBOOT", "needsReboot": true, "label": "kernel"}),
    ]
}

/// A macOS update that needs a restart.
fn macos_due() -> Vec<Value> {
    vec![
        json!({"kind": "MAINTENANCE_KIND_MACOS", "from": "26.1", "to": "26.2", "needsReboot": true,
                "label": "macOS Tahoe 26.2-25C56"}),
    ]
}

fn linux_report(due: Vec<Value>) -> Value {
    json!({
        "host": {"osName": "ubuntu", "osVersion": "24.04", "runningKernel": "6.8.0-45-generic",
                 "newestKernel": "6.8.0-45-generic", "bootId": "b-1"},
        "due": due,
        "lookedUnix": "1799999400",
    })
}

/// A Mac's report with a macOS update due and `selected` the Xcode `xcode-select` names ("": none
/// is installed).
fn mac_report(selected: &str) -> Value {
    let xcodes = if selected.is_empty() {
        json!([])
    } else {
        json!([{"version": selected, "build": "27A100",
                "path": format!("/Applications/Xcode-{selected}.app"), "selected": true}])
    };
    json!({
        "host": {"osName": "macOS", "osVersion": "26.1", "osBuild": "25B78", "runningKernel": "25.1.0",
                 "bootId": "b-1"},
        "xcodes": xcodes,
        "due": macos_due(),
        "lookedUnix": "1799999400",
    })
}

fn host(node_id: &str, group: &str, report: Value) -> Value {
    json!({"nodeId": node_id, "hostGroup": group, "maintenance": report, "maintenanceLookAgeSecs": "600"})
}

fn mac_host(node_id: &str, selected: &str) -> Value {
    host(node_id, "mac-build", mac_report(selected))
}

/// The host page's gateway: `h-1` of group `g` with a kernel update due; `groups` answers
/// HostGroups.List when given.
fn host_ui(groups: Option<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": host("h-1", "g", linux_report(kernel_due()))}),
    );
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    if let Some(groups) = groups {
        ui.reply(&m("HostGroups/List"), groups);
    }
    ui
}

/// HostGroups.List of the one group `g`, whose maintenance policy has a batch of 2 and a drain
/// deadline of 600 seconds.
fn groups_with_policy() -> Value {
    json!({"hostGroups": [{"group": {"name": "g", "packages": ["hostkeeper"],
        "policy": {"maintenance": {"batch": 2, "drainDeadlineSecs": "600"}}}}]})
}

/// The host page with `replies` answers to Maintenance.Schedule, the group's policy known.
fn form_ui(replies: Vec<Value>) -> Ui {
    let ui = host_ui(Some(groups_with_policy()));
    for reply in replies {
        ui.reply(&m("Maintenance/Schedule"), reply);
    }
    ui
}

/// The Maintenance page over `hosts`, with two reads of the runs (none) and, when given, the
/// maintenance `policy` HostGroups.Get answers for `mac-build`.
fn mac_ui(policy: Option<Value>, hosts: Vec<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({ "hosts": hosts }));
    ui.reply(&m("Maintenance/List"), json!({}));
    ui.reply(&m("Maintenance/List"), json!({}));
    if let Some(policy) = policy {
        ui.reply(
            &m("HostGroups/Get"),
            json!({"hostGroup": {"group": {"name": "mac-build", "packages": ["hostkeeper"], "policy": policy}}}),
        );
    }
    ui
}

/// Home over `alerts`.
fn home_ui() -> Ui {
    let due = |group: &str| {
        json!({"kind": "ALERT_KIND_UPDATES_DUE", "severity": "ALERT_SEVERITY_WARNING", "hostGroup": group,
               "message": format!("Updates due on 2 hosts of {group}"), "firstObservedUnix": "1800000000"})
    };
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(
        &m("Alerts/List"),
        json!({"alerts": [due("linux-build"), due("mac-build")]}),
    );
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    ui
}

/// The Maintenance page over a Linux group with one due host and one without, and a Mac group with
/// one due host, and two reads of the runs.
fn maintenance_ui() -> Ui {
    mac_ui(
        None,
        vec![
            host("k-1", "linux-build", linux_report(kernel_due())),
            host("a-1", "linux-build", linux_report(vec![])),
            mac_host("m-1", ""),
        ],
    )
}

/// A place the Update button is: the gateway that serves its page, the page, the index among the
/// page's Reason fields and Update buttons, the one request a click sends, the question it asks
/// and how often the page is told to refresh.
struct Surface {
    name: &'static str,
    ui: fn() -> Ui,
    page: &'static str,
    params: &'static str,
    reason_at: usize,
    button_at: usize,
    body: Value,
    question: String,
    refreshes: u64,
}

fn surfaces() -> Vec<Surface> {
    let ask = |what: &str| format!("Update {what}? {TAIL}");
    vec![
        Surface {
            name: "the host page's Updates tab",
            ui: || host_ui(None),
            page: "host",
            params: r#"{ nodeId: "h-1" }"#,
            reason_at: 0,
            button_at: 0,
            body: json!({"hostIds": ["h-1"]}),
            question: ask("the host h-1"),
            refreshes: 1,
        },
        Surface {
            name: "a due host's row on the Maintenance page",
            ui: maintenance_ui,
            page: "maintenance",
            params: "{}",
            reason_at: 0,
            button_at: 0,
            body: json!({"hostIds": ["k-1"]}),
            question: ask("the host k-1"),
            refreshes: 0,
        },
        Surface {
            name: "the first group's button on the Maintenance page",
            ui: maintenance_ui,
            page: "maintenance",
            params: "{}",
            reason_at: 0,
            button_at: 1,
            body: json!({"hostGroup": "linux-build"}),
            question: ask("the due hosts of linux-build"),
            refreshes: 0,
        },
        Surface {
            name: "the second group's button on the Maintenance page",
            ui: maintenance_ui,
            page: "maintenance",
            params: "{}",
            reason_at: 1,
            button_at: 3,
            body: json!({"hostGroup": "mac-build"}),
            question: ask("the due hosts of mac-build"),
            refreshes: 0,
        },
        Surface {
            name: "the first line of Home's notice",
            ui: home_ui,
            page: "home",
            params: "{}",
            reason_at: 0,
            button_at: 0,
            body: json!({"hostGroup": "linux-build"}),
            question: ask("the due hosts of linux-build"),
            refreshes: 1,
        },
        Surface {
            name: "the second line of Home's notice, which shares the first's Reason field",
            ui: home_ui,
            page: "home",
            params: "{}",
            reason_at: 0,
            button_at: 1,
            body: json!({"hostGroup": "mac-build"}),
            question: ask("the due hosts of mac-build"),
            refreshes: 1,
        },
    ]
}

/// What a stored run of one waiting host answers.
fn stored() -> Value {
    json!({"run": {"id": "r-7", "state": "MAINTENANCE_STATE_RUNNING"},
           "hosts": [{"hostId": "h-1", "step": "MAINTENANCE_HOST_STEP_WAITING"}]})
}

/// Types each of `reasons` in the surface's Reason field and clicks its Update button after each.
fn press(surface: &Surface, ui: &mut Ui, reasons: &[&str], answers: &str) -> Value {
    let typing: String = reasons
        .iter()
        .map(|reason| {
            format!(
                "type(tree, \"update-reason\", {}, {});\nawait click(tree, \"update\", {});\n",
                serde_json::to_string(reason).expect("a reason is JSON"),
                surface.reason_at,
                surface.button_at
            )
        })
        .collect();
    ui.run(&page_driver(
        surface.page,
        surface.params,
        answers,
        &format!("{typing}return null;"),
    ))
}

/// The bodies sent to Maintenance.Schedule.
fn schedules(ui: &Ui) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Maintenance/Schedule"))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn each_update_button_sends_one_schedule_request_after_one_question() {
    // Asserts: on the host page, a due host's row and a group's button of the Maintenance page and
    // each line of Home's notice, a click with a reason typed (with spaces around it) asks the
    // operator once, in words naming what is updated, and on yes sends exactly one
    // Maintenance.Schedule body: the host ids or the host group, the trimmed reason and
    // `confirm: true`, with no kinds, no window, no batch, no drain deadline and no
    // `takeLastHost`; reports the run and the hosts waiting; and reads the page again (a refresh
    // of the host page and Home).
    for surface in surfaces() {
        let mut ui = (surface.ui)();
        ui.reply(&m("Maintenance/Schedule"), stored());
        let out = press(&surface, &mut ui, &["  monthly patch  "], "[true]");
        let mut body = surface.body.clone();
        body["reason"] = json!("monthly patch");
        body["confirm"] = json!(true);
        assert_eq!(schedules(&ui), [body], "{}", surface.name);
        assert_eq!(
            out["seen"]["questions"],
            json!([surface.question]),
            "{}",
            surface.name
        );
        assert_eq!(
            out["seen"]["reports"],
            json!([WAITING_ONE]),
            "{}",
            surface.name
        );
        assert_eq!(
            out["seen"]["refreshes"],
            json!(surface.refreshes),
            "{}",
            surface.name
        );
    }
}

#[test]
fn an_update_the_operator_declines_calls_nothing() {
    // Asserts: on every place, a click answered no sends no Maintenance.Schedule, reads the page
    // again no more than the page's own start did, asks exactly the one question and reports
    // "Not updated.".
    for surface in surfaces() {
        let mut ui = (surface.ui)();
        let out = press(&surface, &mut ui, &["monthly patch"], "[false]");
        assert_eq!(schedules(&ui), Vec::<Value>::new(), "{}", surface.name);
        assert_eq!(
            out["seen"]["questions"],
            json!([surface.question]),
            "{}",
            surface.name
        );
        assert_eq!(
            out["seen"]["reports"],
            json!(["Not updated."]),
            "{}",
            surface.name
        );
        assert_eq!(out["seen"]["refreshes"], json!(0), "{}", surface.name);
    }
}

#[test]
fn an_update_without_a_usable_reason_asks_nothing_and_calls_nothing() {
    // Asserts: on every place, a reason that is empty, only spaces, or has a control character
    // (a tab) is told in words ("Give a reason first." / the rule of a reason), with no question
    // and no Maintenance.Schedule.
    for surface in surfaces() {
        let mut ui = (surface.ui)();
        let out = press(&surface, &mut ui, &["", "   ", "a\tb"], "[true]");
        assert_eq!(schedules(&ui), Vec::<Value>::new(), "{}", surface.name);
        assert_eq!(out["seen"]["questions"], json!([]), "{}", surface.name);
        assert_eq!(
            out["seen"]["reports"],
            json!([
                "Give a reason first.",
                "Give a reason first.",
                "A reason has no control character (a tab or a line break, for example)."
            ]),
            "{}",
            surface.name
        );
    }
}

#[test]
fn a_refused_update_is_reported_with_its_token_and_reads_nothing_again() {
    // Asserts: when the coordinator refuses the one request (FAILED_PRECONDITION last_host), the
    // report is the code and the token as the coordinator said them; exactly one request was
    // sent; and the page is not told to refresh.
    for surface in surfaces() {
        let mut ui = (surface.ui)();
        ui.refuse(
            &m("Maintenance/Schedule"),
            412,
            "FAILED_PRECONDITION",
            "last_host",
        );
        let out = press(&surface, &mut ui, &["monthly patch"], "[true]");
        assert_eq!(schedules(&ui).len(), 1, "{}", surface.name);
        assert_eq!(
            out["seen"]["reports"],
            json!(["FAILED_PRECONDITION last_host"]),
            "{}",
            surface.name
        );
        assert_eq!(out["seen"]["refreshes"], json!(0), "{}", surface.name);
    }
}

#[test]
fn times_convert_between_a_zone_and_unix_seconds_and_a_bad_time_is_null() {
    // Asserts: `localToUnix` gives the unix seconds of a wall-clock time in a zone at UTC-05:00:
    // 2027-01-15T03:00 is 08:00 UTC, which is 1800000000 (2027-01-15 00:00 UTC is 1799971200;
    // 03:00 later is 1799982000; the zone is 18000 s behind UTC, so add 18000); seconds count
    // (:30 is 30 more); a leap day of 2028 is taken (1835395200 + 18000). A zone that changes its
    // offset at 1805007600 (2027-03-14 07:00 UTC) is looked up at the converted instant: 01:30
    // that day is 06:30 UTC (1805005800), and 03:30, after the clock moved forward, is 07:30 UTC
    // (1805009400), not 08:30. A February 30 or 29 of a year that is not a leap year, a month
    // 13, an hour 24, a minute 60, a second 60, a space for the T, no time, and words give null.
    // `unixToLocal` is the inverse in both zones.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { localToUnix, unixToLocal } from "./schedule.js";
export async function main() {
  const fixed = () => -18000;
  const s = (value) => (value === null ? null : String(value));
  const moved = (unix) => (unix >= 1805007600 ? -14400 : -18000);
  return {
    fixed: ["2027-01-15T03:00", "2027-01-15T03:00:30", "2028-02-29T00:00"].map((t) => s(localToUnix(t, fixed))),
    bad: ["2027-02-30T10:00", "2027-13-01T10:00", "2027-02-29T10:00", "2027-01-15T24:00", "2027-01-15T10:60",
      "2027-01-15T10:00:60", "2027-01-15 10:00", "2027-01-15", "", "soon"].map((t) => s(localToUnix(t, fixed))),
    moved: ["2027-03-14T01:30", "2027-03-14T03:30"].map((t) => s(localToUnix(t, moved))),
    back: [unixToLocal(1800000000, fixed), unixToLocal(1805009400, moved)],
  };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "fixed": ["1800000000", "1800000030", "1835413200"],
            "bad": [null, null, null, null, null, null, null, null, null, null],
            "moved": ["1805005800", "1805009400"],
            "back": ["2027-01-15T03:00", "2027-03-14T03:30"],
        })
    );
}

#[test]
fn a_window_typed_in_the_operators_zone_is_sent_as_unix_seconds_and_shown_as_utc() {
    // Asserts: in the fake zone UTC-05:00, a window typed 2027-01-15T22:00 to 2027-01-16T02:30
    // shows as UTC "2027-01-16 03:00:00" to "2027-01-16 07:30:00" and the confirmed request
    // sends windowStartUnix "1800068400" and windowEndUnix "1800084600" as strings. Arithmetic:
    // 2027-01-15 00:00 UTC is 1799971200; 22:00 local is 1799971200 + 79200 = 1800050400 as a
    // wall time, and the zone is 18000 s behind UTC, so 1800050400 + 18000 = 1800068400; 02:30 of
    // the next day is 1799971200 + 86400 + 9000 = 1800066600, + 18000 = 1800084600 (4.5 hours
    // later). The question names the host, the reason is trimmed, and the host's tab says the run.
    let mut ui = form_ui(vec![stored()]);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[true]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
choose(tree, "when", "window");
type(cur(), "window-start", "2027-01-15T22:00");
named(cur(), "window-start")[0].on.change({});
type(cur(), "window-end", "2027-01-16T02:30");
named(cur(), "window-end")[0].on.change({});
const utc = text(named(cur(), "window-utc")[0]);
type(cur(), "schedule-reason", "  overnight window ");
await click(cur(), "schedule-confirm");
return { utc };"#,
    ));
    assert_eq!(
        out["out"]["utc"],
        "Start 2027-01-16 03:00:00 UTC; end 2027-01-16 07:30:00 UTC"
    );
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["h-1"], "windowStartUnix": "1800068400", "windowEndUnix": "1800084600",
                "reason": "overnight window", "confirm": true})
        ]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([format!("Schedule maintenance of the host h-1? {TAIL}")])
    );
    assert_eq!(out["seen"]["reports"], json!([WAITING_ONE]));
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn a_window_across_a_clock_change_is_converted_at_each_end_s_own_offset() {
    // Asserts: in a zone that is UTC-05:00 until 1805007600 (2027-03-14 07:00 UTC) and UTC-04:00
    // after, a window from 2027-03-14T01:30 to 2027-03-14T03:30 local is sent as windowStartUnix
    // "1805005800" (06:30 UTC: the offset before the change) and windowEndUnix "1805009400"
    // (07:30 UTC: the offset after it; a single lookup at the wall time would send 1805013000,
    // 08:30 UTC), and the converted times shown are those UTC times. Arithmetic: 2027-03-14 00:00
    // UTC is 1799971200 + 58 days * 86400 = 1804982400; 01:30 local is + 5400 = 1804987800 and
    // + 18000 = 1805005800; 03:30 local is + 12600 = 1804995000 and + 14400 (EDT) = 1805009400.
    let mut ui = form_ui(vec![stored()]);
    let out = ui.run(
        r#"
import { api, recorder, text, named, type, choose, click } from "../test/kit.js";
import { render } from "./pages/host.js";
export async function main() {
  const zone = (unix) => (unix >= 1805007600 ? -14400 : -18000);
  const { ctx, seen } = recorder(api("operator-token"), { nodeId: "h-1" }, [true], undefined, zone);
  const tree = await render(ctx);
  const cur = () => seen.shown[seen.shown.length - 1];
  choose(tree, "when", "window");
  type(cur(), "window-start", "2027-03-14T01:30");
  named(cur(), "window-start")[0].on.change({});
  type(cur(), "window-end", "2027-03-14T03:30");
  named(cur(), "window-end")[0].on.change({});
  const utc = text(named(cur(), "window-utc")[0]);
  type(cur(), "schedule-reason", "clock change");
  await click(cur(), "schedule-confirm");
  return { utc, reports: seen.reports };
}"#,
    );
    assert_eq!(
        out["utc"],
        "Start 2027-03-14 06:30:00 UTC; end 2027-03-14 07:30:00 UTC"
    );
    assert_eq!(out["reports"], json!([WAITING_ONE]));
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["h-1"], "windowStartUnix": "1805005800", "windowEndUnix": "1805009400",
                "reason": "clock change", "confirm": true})
        ]
    );
}

#[test]
fn a_window_that_is_not_a_time_or_does_not_run_forward_is_told_and_sends_nothing() {
    // Asserts: with "in a window" chosen, both Check and Schedule say, with no question and no
    // call: a start of February 30 or an empty start "The window's start is not a date and
    // time."; an end that is not a time "The window's end is not a date and time."; an end equal
    // to the start, or a minute before it, "The window ends before it starts."; and a window one
    // minute long (2027-01-15T10:00 to 10:01 local, 1800025200 to 1800025260 as unix seconds:
    // 1799971200 + 36000 + 18000) is taken and sent by Check, with the reason.
    let mut ui = form_ui(vec![json!({})]);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[true]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
choose(tree, "when", "window");
type(cur(), "schedule-reason", "r");
const tries = [
  ["2027-02-30T10:00", "2027-02-30T11:00"],
  ["", "2027-01-15T10:00"],
  ["2027-01-15T10:00", "soon"],
  ["2027-01-15T10:00", "2027-01-15T10:00"],
  ["2027-01-15T11:00", "2027-01-15T10:59"],
];
for (const [start, end] of tries) {
  type(cur(), "window-start", start);
  type(cur(), "window-end", end);
  await click(cur(), "schedule-check");
  await click(cur(), "schedule-confirm");
}
const refused = seen.reports.slice();
type(cur(), "window-start", "2027-01-15T10:00");
type(cur(), "window-end", "2027-01-15T10:01");
await click(cur(), "schedule-check");
return { refused, last: seen.reports[seen.reports.length - 1] };"#,
    ));
    let start = "The window's start is not a date and time.";
    let end = "The window's end is not a date and time.";
    let order = "The window ends before it starts.";
    assert_eq!(
        out["out"]["refused"],
        json!([
            start, start, start, start, end, end, order, order, order, order
        ])
    );
    assert_eq!(out["out"]["last"], CHECKED_NONE);
    assert_eq!(out["seen"]["questions"], json!([]));
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["h-1"], "windowStartUnix": "1800025200", "windowEndUnix": "1800025260",
                "reason": "r"})
        ]
    );
}

#[test]
fn every_kind_ticked_sends_no_kinds_and_unticking_sends_the_rest() {
    // Asserts: the form offers one tick for each kind due on the host, in the order Linux
    // updates, restart; with both ticked, Check sends no `kinds` (everything due); with restart
    // unticked it sends kinds ["MAINTENANCE_KIND_LINUX"]; with neither ticked it says "Tick at
    // least one kind." and sends nothing; with only restart ticked it sends
    // ["MAINTENANCE_KIND_REBOOT"]. A Check sends no `confirm`, and asks nothing.
    let mut ui = form_ui(vec![json!({}), json!({}), json!({})]);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        r#"
const labels = nodes(tree).filter((n) => n.tag === "label" && nodes(n).some((x) => x.attrs["name"] === "kind")).map((n) => text(n).trim());
const said = async () => { const before = seen.reports.length; await click(tree, "schedule-check"); return seen.reports.slice(before); };
const all = await said();
tick(tree, "kind", false, 1);
const noRestart = await said();
tick(tree, "kind", false, 0);
const none = await said();
tick(tree, "kind", true, 1);
const onlyRestart = await said();
return { labels, all, noRestart, none, onlyRestart };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "labels": ["Linux updates", "restart"],
            "all": [CHECKED_NONE],
            "noRestart": [CHECKED_NONE],
            "none": ["Tick at least one kind."],
            "onlyRestart": [CHECKED_NONE],
        })
    );
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["h-1"]}),
            json!({"hostIds": ["h-1"], "kinds": ["MAINTENANCE_KIND_LINUX"]}),
            json!({"hostIds": ["h-1"], "kinds": ["MAINTENANCE_KIND_REBOOT"]}),
        ]
    );
    assert_eq!(out["seen"]["questions"], json!([]));
}

#[test]
fn batch_and_drain_deadline_are_sent_only_when_typed_and_take_last_host_only_when_ticked() {
    // Asserts: the labels name the group's batch (2) and drain deadline (600 s) from its policy;
    // blank fields (spaces) send neither; a batch of 0, 10001, "2.5" and a drain deadline of -1
    // or "1.5" are refused in words with no call; a batch of 10000 with a drain deadline of 0 is
    // sent as `batch: 10000` and `drainDeadlineSecs: "0"` (a string, the group's deadline
    // overridden by none); a batch of 3, a deadline of 900 and the ticked box send `batch: 3`,
    // `drainDeadlineSecs: "900"` and `takeLastHost: true`.
    let mut ui = form_ui(vec![json!({}), json!({}), json!({})]);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        r#"
const labelOf = (name) => text(nodes(tree).find((n) => n.tag === "label" && nodes(n).some((x) => x.attrs["name"] === name))).trim();
const labels = [labelOf("batch"), labelOf("drain-deadline")];
const said = async (batch, deadline) => {
  type(tree, "batch", batch);
  type(tree, "drain-deadline", deadline);
  const before = seen.reports.length;
  await click(tree, "schedule-check");
  return seen.reports.slice(before);
};
const results = [];
results.push(await said("  ", "  "));
results.push(await said("0", ""));
results.push(await said("10001", ""));
results.push(await said("2.5", ""));
results.push(await said("1", "-1"));
results.push(await said("1", "1.5"));
results.push(await said("10000", "0"));
tick(tree, "take-last-host", true);
results.push(await said("3", "900"));
return { labels, results };"#,
    ));
    let batch = json!(["The batch is a whole number from 1 to 10000."]);
    let deadline = json!(["The drain deadline is a whole number of seconds, 0 or more."]);
    assert_eq!(
        out["out"],
        json!({
            "labels": ["Batch (the group's: 2)", "Drain deadline, seconds (the group's: 600)"],
            "results": [[CHECKED_NONE], batch, batch, batch, deadline, deadline, [CHECKED_NONE], [CHECKED_NONE]],
        })
    );
    assert_eq!(
        schedules(&ui),
        [
            json!({"hostIds": ["h-1"]}),
            json!({"hostIds": ["h-1"], "batch": 10000, "drainDeadlineSecs": "0"}),
            json!({"hostIds": ["h-1"], "batch": 3, "drainDeadlineSecs": "900", "takeLastHost": true}),
        ]
    );
}

#[test]
fn the_form_sends_the_host_the_group_or_the_hosts_ticked_as_the_operator_chose() {
    // Asserts: on the host page the form offers this host, every host of its group and the hosts
    // ticked, with this host chosen at first; choosing the group sends `hostGroup: "g"`;
    // choosing the hosts ticked shows one tick for the host (ticked, as it has something due) and
    // sends `hostIds: ["h-1"]`; with that tick cleared it says "Tick at least one host." and
    // sends nothing.
    let mut ui = form_ui(vec![json!({}), json!({})]);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const said = async (t) => { const before = seen.reports.length; await click(t, "schedule-check"); return seen.reports.slice(before); };
const options = choices(tree, "where", 0);
const start = chosen(tree, "where", 0);
choose(tree, "where", "group", 0);
const group = chosen(cur(), "where", 0);
const asGroup = await said(cur());
choose(cur(), "where", "hosts", 0);
const hosts = chosen(cur(), "where", 0);
const picks = nodes(cur()).filter((n) => n.attrs["name"] === "pick").length;
const asHosts = await said(cur());
tick(cur(), "pick", false, 0);
const none = await said(cur());
return { options, start, group, asGroup, hosts, picks, asHosts, none };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "options": ["this host: h-1", "every host of g", "the hosts ticked"],
            "start": "host",
            "group": "group",
            "asGroup": [CHECKED_NONE],
            "hosts": "hosts",
            "picks": 1,
            "asHosts": [CHECKED_NONE],
            "none": ["Tick at least one host."],
        })
    );
    assert_eq!(
        schedules(&ui),
        [json!({"hostGroup": "g"}), json!({"hostIds": ["h-1"]})]
    );
}

#[test]
fn the_schedule_button_needs_a_reason_and_one_yes_and_reports_a_refusal() {
    // Asserts: Schedule with no reason says "Give a reason first." with no question; with a
    // reason and an answer of no it asks once ("Schedule maintenance of the host h-1? ...") and
    // says "Not scheduled." with no call; answered yes it sends one body with the reason and
    // `confirm: true`, and a refusal of it (FAILED_PRECONDITION nothing_due) is reported as the
    // coordinator said it, with the page not told to refresh.
    let mut ui = host_ui(None);
    ui.refuse(
        &m("Maintenance/Schedule"),
        412,
        "FAILED_PRECONDITION",
        "nothing_due",
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[false, true]",
        r#"
await click(tree, "schedule-confirm");
type(tree, "schedule-reason", "patch");
await click(tree, "schedule-confirm");
await click(tree, "schedule-confirm");
return null;"#,
    ));
    let question = format!("Schedule maintenance of the host h-1? {TAIL}");
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Not scheduled.",
            "FAILED_PRECONDITION nothing_due"
        ])
    );
    assert_eq!(out["seen"]["questions"], json!([question, question]));
    assert_eq!(
        schedules(&ui),
        [json!({"hostIds": ["h-1"], "reason": "patch", "confirm": true})]
    );
    assert_eq!(out["seen"]["refreshes"], 0);
}

/// A turn of the answer to a Schedule of `mac-build`: waiting with `kinds`, or skipped for
/// `skipped`.
fn turn(host_id: &str, kinds: &[&str], skipped: Option<&str>) -> Value {
    let mut out = json!({"hostId": host_id, "hostGroup": "mac-build",
                         "kinds": kinds.iter().map(|k| format!("MAINTENANCE_KIND_{k}")).collect::<Vec<_>>()});
    match skipped {
        Some(why) => {
            out["step"] = json!("MAINTENANCE_HOST_STEP_SKIPPED");
            out["skippedBecause"] = json!(format!("MAINTENANCE_SKIP_{why}"));
        }
        None => out["step"] = json!("MAINTENANCE_HOST_STEP_WAITING"),
    }
    out
}

#[test]
fn a_host_the_check_leaves_out_is_shown_with_why_cannot_be_ticked_and_is_not_sent() {
    // Asserts: on a Mac group's form with "the hosts ticked" chosen, the three hosts have a tick;
    // Check sends all three (no `confirm`), and its answer, where m-2 is skipped for a
    // device-management conflict and m-3 for a daemon that does not announce maintenance-v1,
    // shows a preview table of the three with the words of why for each left out, shows m-2 and
    // m-3 as lines "left out (why)" with no tick (only m-1 keeps one) and reports "Checked: 1
    // hosts would be taken, 2 skipped."; with m-1 unticked, Schedule says "Tick at least one
    // host." and calls nothing; with it ticked the confirmed request names m-1 alone, and the
    // report of the stored run lists m-2 and m-3 as skipped with their words. Nothing else is
    // called but the page's reads and HostGroups.Get when the form is opened.
    let mut ui = mac_ui(
        Some(json!({})),
        vec![
            mac_host("m-1", ""),
            mac_host("m-2", ""),
            mac_host("m-3", ""),
        ],
    );
    let hosts = [
        turn("m-1", &["MACOS"], None),
        turn("m-2", &[], Some("MDM_CONFLICT")),
        turn("m-3", &[], Some("NO_MAINTENANCE_V1")),
    ];
    ui.reply(
        &m("Maintenance/Schedule"),
        json!({"run": {"state": "MAINTENANCE_STATE_SCHEDULED", "hostGroup": "mac-build"}, "hosts": hosts}),
    );
    ui.reply(
        &m("Maintenance/Schedule"),
        json!({"run": {"id": "r-9", "state": "MAINTENANCE_STATE_RUNNING", "hostGroup": "mac-build"},
               "hosts": hosts}),
    );
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[true]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const picks = () => nodes(cur()).filter((n) => n.attrs["name"] === "pick").length;
await click(tree, "open-schedule", 0);
choose(cur(), "where", "hosts", 0);
const before = picks();
type(cur(), "schedule-reason", "macOS 26.2");
await click(cur(), "schedule-check");
const checked = {
  picks: picks(),
  skipped: nodes(cur()).filter((n) => n.attrs["name"] === "skipped-host").map(text),
  preview: rows(named(cur(), "schedule-preview")[0]),
};
tick(cur(), "pick", false, 0);
await click(cur(), "schedule-confirm");
tick(cur(), "pick", true, 0);
await click(cur(), "schedule-confirm");
return { before, checked };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "before": 3,
            "checked": {
                "picks": 1,
                "skipped": [format!("m-2: left out ({MDM})"), format!("m-3: left out ({OLD_DAEMON})")],
                "preview": [
                    ["Host", "Group", "Kinds", "Left out"],
                    ["m-1", "mac-build", "macOS", "-"],
                    ["m-2", "mac-build", "-", MDM],
                    ["m-3", "mac-build", "-", OLD_DAEMON],
                ],
            },
        })
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Checked: 1 hosts would be taken, 2 skipped.",
            "Tick at least one host.",
            format!("Scheduled run r-9: 1 host waiting; skipped: m-2 ({MDM}), m-3 ({OLD_DAEMON})"),
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([format!("Schedule maintenance of the host m-1? {TAIL}")])
    );
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/List"), json!({})),
            (m("Maintenance/List"), json!({"limit": 10})),
            (
                m("HostGroups/Get"),
                json!({"hostGroup": "mac-build", "limit": 1})
            ),
            (
                m("Maintenance/Schedule"),
                json!({"hostIds": ["m-1", "m-2", "m-3"], "reason": "macOS 26.2"})
            ),
            (
                m("Maintenance/Schedule"),
                json!({"hostIds": ["m-1"], "reason": "macOS 26.2", "confirm": true})
            ),
            (m("Maintenance/List"), json!({"limit": 10})),
        ]
    );
}

/// The lines "moves the default Xcode" a Check of the Mac group shows, when its hosts m-1 (Xcode
/// 27.1 selected), m-2 (26.4), m-3 (none installed) and m-4 (27.1) are all in the answer, the first
/// three with an Xcode turn when `xcode_turns` and m-4 with a macOS turn only; the group's
/// maintenance policy is `policy` (none: HostGroups.Get is not answered, so the policy is not
/// known).
fn moves_default(policy: Option<Value>, xcode_turns: bool) -> Value {
    let mut ui = mac_ui(
        policy,
        vec![
            mac_host("m-1", "27.1"),
            mac_host("m-2", "26.4"),
            mac_host("m-3", ""),
            mac_host("m-4", "27.1"),
        ],
    );
    let kinds: &[&str] = if xcode_turns {
        &["MACOS", "XCODE"]
    } else {
        &["MACOS"]
    };
    ui.reply(
        &m("Maintenance/Schedule"),
        json!({"run": {"state": "MAINTENANCE_STATE_SCHEDULED", "hostGroup": "mac-build"},
               "hosts": [turn("m-1", kinds, None), turn("m-2", kinds, None), turn("m-3", kinds, None),
                         turn("m-4", &["MACOS"], None)]}),
    );
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
await click(cur(), "schedule-check");
return nodes(cur()).filter((n) => n.attrs["name"] === "moves-default").map(text);"#,
    ));
    assert_eq!(schedules(&ui), [json!({"hostGroup": "mac-build"})]);
    out["out"].clone()
}

#[test]
fn a_check_says_which_hosts_would_move_the_default_xcode() {
    // Asserts: with the group's policy naming Xcode 26.4 as the default, the Check's preview
    // says "moves the default Xcode from 27.1 to 26.4" for the host whose selected Xcode is 27.1
    // and "from none to 26.4" for the host with none, and says nothing of the host already on
    // 26.4 or of a host whose turn has no Xcode; with no default in the policy it names the
    // latest kept for every host whose turn has an Xcode ("to the latest kept"); with no Xcode
    // turn in the answer, or when the group's policy could not be read, there is no such line.
    assert_eq!(
        moves_default(
            Some(json!({"maintenance": {"xcodeKeep": 2, "xcodeDefault": "26.4"}})),
            true
        ),
        json!([
            "m-1: moves the default Xcode from 27.1 to 26.4",
            "m-3: moves the default Xcode from none to 26.4",
        ])
    );
    assert_eq!(
        moves_default(Some(json!({"maintenance": {"xcodeKeep": 1}})), true),
        json!([
            "m-1: moves the default Xcode from 27.1 to the latest kept",
            "m-2: moves the default Xcode from 26.4 to the latest kept",
            "m-3: moves the default Xcode from none to the latest kept",
        ])
    );
    assert_eq!(
        moves_default(
            Some(json!({"maintenance": {"xcodeKeep": 2, "xcodeDefault": "26.4"}})),
            false
        ),
        json!([])
    );
    assert_eq!(moves_default(None, true), json!([]));
}
