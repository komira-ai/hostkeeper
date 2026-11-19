//! Why a host is not healthy (`ui/health.js`), on its page and on Home: each reason in plain words
//! with the time it began and the one action that fixes it (Clear halt, Retry update), a js-agent
//! taint's facts as hostkeeper holds them, a server's row saying why, and the fleet's degraded
//! hosts counted with their reasons grouped.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// An alert of Alerts.List.
fn alert(
    kind: &str,
    severity: &str,
    host: &str,
    service: &str,
    message: &str,
    since: &str,
) -> Value {
    json!({"kind": format!("ALERT_KIND_{kind}"), "severity": format!("ALERT_SEVERITY_{severity}"),
           "hostId": host, "service": service, "message": message, "firstObservedUnix": since})
}

/// A live host that heartbeated, with `services`.
fn live(node: &str, services: Value) -> Value {
    json!({"nodeId": node, "live": true, "lastHeartbeatUnix": "1800000000", "services": services})
}

const TAINT_SAID: &str = "the agent is tainted: daemonised_child";
const HALT_SAID: &str = "js-agent is halted at 0.1.250 by node-07, check tainted: nothing is ordered for it until an operator clears the halt";
const PREREQ_SAID: &str = "node-07 cannot make sure of its host prerequisites (xcode-command-line-tools failed 3 in a row, next try at unix 1800000600: softwareupdate exited 1); its daemon tries again by itself, and until it succeeds the host lacks what its host group needs";

/// js-agent's view on a host whose update hook halted for a taint.
fn tainted_agent() -> Value {
    json!({"service": "js-agent", "gateReason": "GATE_REASON_HALTED", "health": "HEALTH_UNHEALTHY",
           "lastUpdate": {"target": "0.1.250", "outcome": "OUTCOME_FAILED", "phase": "FAILURE_PHASE_HALT",
                          "reason": "tainted", "detail": TAINT_SAID, "startedUnix": "1799992000",
                          "logTail": ["prepare: tainted", "halt 76"]}})
}

/// tierstore's view on a host whose update failed to fetch the package.
fn fetch_failed() -> Value {
    json!({"service": "tierstore", "gateReason": "GATE_REASON_FAILED_HERE",
           "lastUpdate": {"target": "3.1.0", "outcome": "OUTCOME_FAILED", "phase": "FAILURE_PHASE_BEFORE_COMMIT",
                          "reason": "fetch_failed", "detail": "the package could not be fetched",
                          "startedUnix": "1799991000"}})
}

/// node-07: halted js-agent for a taint (and unhealthy), a failed prerequisite, a failed
/// tierstore update and a healthy kbf.
fn mixed() -> Value {
    live(
        "node-07",
        json!([tainted_agent(), fetch_failed(),
               {"service": "kbf", "health": "HEALTH_HEALTHY", "gateReason": "GATE_REASON_UP_TO_DATE"}]),
    )
}

/// The alerts of the fleet `mixed` stands in: two of node-07's, one of another host's and one of
/// the fleet's.
fn mixed_alerts() -> Value {
    json!({"alerts": [
        alert("HALT", "CRITICAL", "node-07", "js-agent", HALT_SAID, "1799992100"),
        alert("HALT", "CRITICAL", "node-01", "tierstore", "tierstore is halted at 3.1.0 by node-01, check draining", "1799990100"),
        alert("CERT_NEAR_EXPIRY", "CRITICAL", "", "", "the certificate the coordinator serves its hosts expires", "1799990000"),
        alert("PREREQUISITE_FAILED", "WARNING", "node-07", "", PREREQ_SAID, "1799993000"),
    ]})
}

/// Runs `body` (JavaScript with `health`, the module, in scope) and returns what it returns.
fn pure(body: &str) -> Value {
    let mut ui = Ui::start();
    ui.run(&format!(
        r#"
import * as health from "./health.js";
export async function main() {{
  {body}
}}"#
    ))
}

/// A reason as an array, in the order of its fields.
const FIELDS: &str = r#"const shape = (r) => [r.kind, r.group, r.title, r.said, r.since === undefined ? null : r.since,
  r.sinceIs, r.tone, r.service, r.fix, r.taint];"#;

#[test]
fn a_hosts_reasons_are_its_alerts_then_its_services_each_with_its_fix() {
    // Asserts: node-07's reasons, in order, are the halt alert naming it (with the taint of its
    // halt report), its prerequisite alert, then js-agent unhealthy and the failed tierstore
    // update; the halt alert of another host and the fleet's alert are not node-07's, and the
    // halt report is not counted twice (as a halt and as a failed update).
    // Catches: a halt shown twice, another host's halt shown here, a failed update offered Clear
    // halt or a halt offered Retry, a reason that loses its time.
    let out = pure(&format!(
        "{FIELDS}\nreturn health.reasonsOf({}, {}.alerts).map(shape);",
        mixed(),
        mixed_alerts()
    ));
    assert_eq!(
        out,
        json!([
            ["halt", "js-agent halted: tainted", "js-agent is halted by the check tainted", HALT_SAID,
             "1799992100", "first seen by the coordinator", "bad", "js-agent", "clear-halt",
             {"said": TAINT_SAID, "log": ["prepare: tainted", "halt 76"]}],
            ["prerequisite", "host prerequisite failed", "A host prerequisite failed", PREREQ_SAID,
             "1799993000", "first seen by the coordinator", "warn", null, null, null],
            ["unhealthy", "js-agent unhealthy", "js-agent reports itself unhealthy", null, null, null,
             "bad", "js-agent", null, null],
            ["failed", "tierstore update failed: fetch_failed",
             "The update of tierstore to 3.1.0 failed: fetch_failed", "the package could not be fetched",
             "1799991000", "started on the host", "bad", "tierstore", "retry", null],
        ])
    );
}

#[test]
fn a_halt_no_alert_names_is_told_by_the_gate_and_a_cleared_one_is_a_failed_update() {
    // Asserts: with no alert, a halt the host holds of its own (HALTED_HERE, no report) and a
    // halt report while the service is halted are halts with Clear halt; a halt report whose halt
    // was cleared (the gate says FAILED_HERE) is a failed update with Retry; a service halted
    // elsewhere whose own update succeeded is no reason; a failed update with no reason and no
    // target says so. An alert of another kind for the host has the coordinator's words.
    // Catches: Clear halt offered for a halt already cleared, a host-held halt missed because no
    // alert names it.
    let host = live(
        "node-08",
        json!([
            {"service": "a", "gateReason": "GATE_REASON_STALE_HALT_HERE"},
            {"service": "b", "gateReason": "GATE_REASON_HALTED",
             "lastUpdate": {"target": "2.0", "outcome": "OUTCOME_FAILED", "phase": "FAILURE_PHASE_HALT",
                            "reason": "draining", "detail": "store draining", "startedUnix": "1799990000"}},
            {"service": "c", "gateReason": "GATE_REASON_FAILED_HERE",
             "lastUpdate": {"target": "2.0", "outcome": "OUTCOME_FAILED", "phase": "FAILURE_PHASE_HALT",
                            "reason": "draining"}},
            {"service": "d", "gateReason": "GATE_REASON_HALTED",
             "lastUpdate": {"target": "2.0", "outcome": "OUTCOME_SUCCEEDED"}},
            {"service": "e", "lastUpdate": {"outcome": "OUTCOME_FAILED"}},
        ]),
    );
    let alerts = json!([alert(
        "SILENT_HOST",
        "WARNING",
        "node-08",
        "",
        "node-08 has not heartbeated for 300 s",
        "1799990000"
    )]);
    let out = pure(&format!(
        "{FIELDS}\nreturn health.reasonsOf({host}, {alerts}).map(shape);"
    ));
    assert_eq!(
        out,
        json!([
            [
                "alert",
                "silent host",
                "silent host",
                "node-08 has not heartbeated for 300 s",
                "1799990000",
                "first seen by the coordinator",
                "warn",
                null,
                null,
                null
            ],
            [
                "halt",
                "a halted",
                "a is halted on this host: the host holds a halt of its own",
                null,
                null,
                "started on the host",
                "bad",
                "a",
                "clear-halt",
                null
            ],
            [
                "halt",
                "b halted: draining",
                "b is halted by the check draining",
                "store draining",
                "1799990000",
                "started on the host",
                "bad",
                "b",
                "clear-halt",
                null
            ],
            [
                "failed",
                "c update failed: draining",
                "The update of c to 2.0 failed: draining",
                null,
                null,
                "started on the host",
                "bad",
                "c",
                "retry",
                null
            ],
            [
                "failed",
                "e update failed: no reason given",
                "The update of e failed",
                null,
                null,
                "started on the host",
                "bad",
                "e",
                "retry",
                null
            ],
        ])
    );
}

#[test]
fn only_a_live_host_with_a_reason_is_degraded() {
    // Asserts: degraded needs a heartbeat, live, and a reason; a host never heard from (no time,
    // or time 0) or silent is not degraded whatever its reasons, and a live host with none is not.
    // Catches: an offline host counted among the degraded, or a host with no heartbeat at all.
    let out = pure(
        r#"
const r = [{ kind: "alert" }];
const at = { live: true, lastHeartbeatUnix: "1800000000" };
return [
  health.isDegraded(at, r),
  health.isDegraded(at, []),
  health.isDegraded({ live: false, lastHeartbeatUnix: "1800000000" }, r),
  health.isDegraded({ live: true, lastHeartbeatUnix: "0" }, r),
  health.isDegraded({ live: true }, r),
];"#,
    );
    assert_eq!(out, json!([true, false, false, false, false]));
}

/// The fleet the brief describes: three Macs whose prerequisite failed, node-01 halted on
/// tierstore (and unhealthy), node-02 whose js-agent fetch failed, node-04 halted on a tainted
/// js-agent, node-05 healthy, and node-06 silent.
fn fleet() -> (Value, Value) {
    let draining = json!({"service": "tierstore", "gateReason": "GATE_REASON_HALTED", "health": "HEALTH_UNHEALTHY",
        "lastUpdate": {"target": "3.1.0", "outcome": "OUTCOME_FAILED", "phase": "FAILURE_PHASE_HALT",
                       "reason": "draining", "startedUnix": "1799990000"}});
    let fetch = json!({"service": "js-agent", "gateReason": "GATE_REASON_FAILED_HERE",
        "lastUpdate": {"target": "0.1.250", "outcome": "OUTCOME_FAILED", "reason": "fetch_failed"}});
    let mut tainted = tainted_agent();
    tainted["health"] = json!("HEALTH_HEALTHY");
    let hosts = json!([
        live("mac-1", json!([])), live("mac-2", json!([])), live("mac-3", json!([])),
        live("node-01", json!([draining])), live("node-02", json!([fetch])),
        live("node-04", json!([tainted])), live("node-05", json!([{"service": "kbf", "health": "HEALTH_HEALTHY"}])),
        {"nodeId": "node-06", "live": false, "lastHeartbeatUnix": "1799000000"},
    ]);
    let prereq = |mac: &str| {
        alert(
            "PREREQUISITE_FAILED",
            "WARNING",
            mac,
            "",
            "xcode-command-line-tools failed",
            "1799993000",
        )
    };
    let alerts = json!([
        alert(
            "HALT",
            "CRITICAL",
            "node-01",
            "tierstore",
            "tierstore is halted at 3.1.0 by node-01, check draining",
            "1799990100"
        ),
        alert(
            "HALT",
            "CRITICAL",
            "node-04",
            "js-agent",
            "js-agent is halted at 0.1.250 by node-04, check tainted",
            "1799992100"
        ),
        alert(
            "SILENT_HOST",
            "WARNING",
            "node-06",
            "",
            "node-06 has not heartbeated for 300 s",
            "1799990000"
        ),
        prereq("mac-1"),
        prereq("mac-2"),
        prereq("mac-3"),
    ]);
    (hosts, alerts)
}

#[test]
fn the_fleet_counts_each_degraded_host_once_and_groups_them_by_reason() {
    // Asserts: six degraded hosts (the healthy one and the silent one are not), the Macs' reason
    // first with its three hosts, then the one-host reasons by name; node-01, with two reasons,
    // is in two groups and counted once. An empty fleet has none.
    // Catches: a host counted once per reason, an offline host among the degraded, groups that
    // split one reason by host.
    let (hosts, alerts) = fleet();
    let out = pure(&format!(
        "return [health.fleetHealth({hosts}, {alerts}), health.fleetHealth([], [])];"
    ));
    assert_eq!(
        out,
        json!([
            {"degraded": ["mac-1", "mac-2", "mac-3", "node-01", "node-02", "node-04"],
             "groups": [
                 {"label": "host prerequisite failed", "hosts": ["mac-1", "mac-2", "mac-3"]},
                 {"label": "js-agent halted: tainted", "hosts": ["node-04"]},
                 {"label": "js-agent update failed: fetch_failed", "hosts": ["node-02"]},
                 {"label": "tierstore halted: draining", "hosts": ["node-01"]},
                 {"label": "tierstore unhealthy", "hosts": ["node-01"]},
             ]},
            {"degraded": [], "groups": []},
        ])
    );
}

/// A gateway that answers the Host page's reads of `host`, with `alerts` for Alerts.List (none
/// scripted: it fails `UNIMPLEMENTED`).
fn host_ui(host: &Value, alerts: Option<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    if let Some(alerts) = alerts {
        ui.reply(&m("Alerts/List"), alerts);
    }
    ui
}

/// The driver's view of the Host page's health section: its heading, each reason's title, when
/// it began and its buttons, the taint's facts, the notes and the header's state.
const SECTION: &str = r#"
const section = nodes(tree).find((n) => n.attrs["name"] === "health");
const bar = nodes(tree).find((n) => n.attrs["class"] === "hostbar");
const state = text(nodes(bar).find((n) => String(n.attrs["class"]).startsWith("pill")));
if (section === undefined) {
  return { state, section: null };
}
const items = nodes(section).filter((n) => n.attrs["name"] === "reason").map((li) => ({
  title: text(li.children[0]),
  since: text(nodes(li).find((n) => n.attrs["class"] === "since")),
  buttons: nodes(li).filter((n) => n.tag === "button").map((n) => n.attrs["name"]),
}));
const taint = nodes(section).find((n) => n.attrs["name"] === "taint");
return {
  state,
  heading: text(nodes(section).find((n) => n.tag === "h3")),
  items,
  taint: taint === undefined ? null : rows(taint),
  log: nodes(section).filter((n) => n.tag === "pre").map(text),
  notes: nodes(section).filter((n) => ["note", "warning"].includes(n.attrs["class"])).map(text),
  reasonField: nodes(section).filter((n) => n.attrs["name"] === "health-reason").length,
};"#;

#[test]
fn the_host_page_says_why_it_is_degraded_with_when_and_the_one_fix_of_each_reason() {
    // Asserts: node-07's page says degraded and lists each reason with its time and whose clock
    // it is ("Since: not reported." for unhealthy), one Clear halt for the halt, one Retry update
    // for the failed update, words instead of a button for the prerequisite and the unhealthy
    // service, one Reason field, the taint's facts (finding, check and run id not reported, the
    // log lines) and that Clear taint is not in hostkeeper; Alerts.List is the last read.
    // Catches: a degraded host whose page does not say why, a reason with no time, a taint shown
    // without what the agent said.
    let mut ui = host_ui(&mixed(), Some(mixed_alerts()));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-07" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(
        out["out"],
        json!({
            "state": "degraded",
            "heading": "Why node-07 is not healthy",
            "items": [
                {"title": "js-agent is halted by the check tainted",
                 "since": "Since 2027-01-15 05:48:20 UTC (first seen by the coordinator).",
                 "buttons": ["clear-halt-js-agent"]},
                {"title": "A host prerequisite failed",
                 "since": "Since 2027-01-15 06:03:20 UTC (first seen by the coordinator).", "buttons": []},
                {"title": "js-agent reports itself unhealthy", "since": "Since: not reported.", "buttons": []},
                {"title": "The update of tierstore to 3.1.0 failed: fetch_failed",
                 "since": "Since 2027-01-15 05:30:00 UTC (started on the host).", "buttons": ["retry-tierstore"]},
            ],
            "taint": [
                ["Taint", "the agent is tainted (its update hook reports the reason tainted)"],
                ["Finding", TAINT_SAID],
                ["Check", "not reported to hostkeeper"],
                ["Run id", "not reported to hostkeeper"],
            ],
            "log": ["prepare: tainted\n halt 76\n"],
            "notes": [
                "Clear taint: hostkeeper has no action that clears a taint yet. Clear it on the host, then press Clear halt here.",
                "Nothing to press here: the host's daemon tries again by itself, at the next try the message names.",
                "Nothing to press here: the service itself says it is unhealthy. Its Services tab has its last update and the actions on it.",
            ],
            "reasonField": 1,
        })
    );
    let methods = ui.methods();
    assert_eq!(methods.last(), Some(&m("Alerts/List")));
    assert!(super_text(&out).contains(HALT_SAID), "{}", super_text(&out));
}

/// The page's text.
fn super_text(out: &Value) -> &str {
    out["text"].as_str().expect("the page's text")
}

#[test]
fn clear_halt_needs_a_good_reason_asks_first_and_clears_the_services_halt() {
    // Asserts: Clear halt with no reason, or one with a control character, sends nothing and
    // says why; a no to the question sends nothing; a yes sends ClearHalt of js-agent with the
    // trimmed reason, refreshes and says so. The question says the halt is lifted on every host
    // and that the taint stays. A refusal is reported and does not refresh.
    // Catches: a clear sent without a reason or without asking, a question that hides that the
    // clear is fleet-wide.
    let ui = host_ui(&mixed(), Some(mixed_alerts()));
    ui.reply(
        &m("Releases/ClearHalt"),
        json!({"entry": {"seq": "9", "action": "clear_halt"}}),
    );
    ui.refuse(
        &m("Releases/ClearHalt"),
        403,
        "PERMISSION_DENIED",
        "forbidden",
    );
    let mut ui = ui;
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-07" }"#,
        "[false, true, true]",
        r#"
await click(tree, "clear-halt-js-agent");
type(tree, "health-reason", "a\tb");
await click(tree, "clear-halt-js-agent");
type(tree, "health-reason", "  taint cleared on the host  ");
await click(tree, "clear-halt-js-agent");
await click(tree, "clear-halt-js-agent");
await click(tree, "clear-halt-js-agent");"#,
    ));
    let question = "Clear the halt of js-agent? It lifts the halt on every host of js-agent, not only node-07, and they are ordered again. The agent on node-07 is still tainted: its update hook may halt again until the taint is cleared on the host.";
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "A reason has no control character (a tab or a line break, for example).",
            "Not cleared.",
            "Cleared the halt of js-agent.",
            "PERMISSION_DENIED forbidden",
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([question, question, question])
    );
    assert_eq!(out["seen"]["refreshes"], json!(1));
    let sent: Vec<_> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Releases/ClearHalt"))
        .collect();
    let body = json!({"service": "js-agent", "reason": "taint cleared on the host"});
    assert_eq!(
        sent,
        [
            (m("Releases/ClearHalt"), body.clone()),
            (m("Releases/ClearHalt"), body)
        ]
    );
}

#[test]
fn retry_asks_first_and_says_whether_the_failed_report_was_set_aside_now_or_before() {
    // Asserts: a no sends nothing; a yes sends Retry of node-07 and tierstore, refreshes and says
    // the failed update to 3.1.0 is set aside; a second Retry answered with no audit entry says it
    // was set aside already. The question names the host and the service.
    // Catches: Retry sent for another host or service, an answer that claims a retry the
    // coordinator did not write.
    let ui = host_ui(&mixed(), Some(mixed_alerts()));
    ui.reply(
        &m("Hosts/Retry"),
        json!({"setAside": {"target": "3.1.0", "orderTry": "2"},
                                       "entry": {"seq": "10", "action": "retry"}}),
    );
    ui.reply(
        &m("Hosts/Retry"),
        json!({"setAside": {"target": "3.1.0", "orderTry": "2"}}),
    );
    let mut ui = ui;
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-07" }"#,
        "[false, true, true]",
        r#"
await click(tree, "retry-tierstore");
await click(tree, "retry-tierstore");
await click(tree, "retry-tierstore");"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not retried.",
            "Set aside the failed update of tierstore to 3.1.0: node-07 is ordered it again at its next heartbeat.",
            "The failed update of tierstore to 3.1.0 was set aside already: node-07 is ordered it again at its next heartbeat.",
        ])
    );
    let question = "Retry the update of tierstore on node-07? Its failed report is set aside, and it is ordered the desired version again at its next heartbeat.";
    assert_eq!(
        out["seen"]["questions"],
        json!([question, question, question])
    );
    assert_eq!(out["seen"]["refreshes"], json!(2));
    let sent: Vec<_> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Hosts/Retry"))
        .collect();
    let body = json!({"nodeId": "node-07", "service": "tierstore"});
    assert_eq!(
        sent,
        [(m("Hosts/Retry"), body.clone()), (m("Hosts/Retry"), body)]
    );
}

#[test]
fn without_the_alerts_the_page_says_so_and_still_shows_the_hosts_own_reasons() {
    // Asserts: when Alerts.List fails, a degraded host's page says the alerts were not read and
    // still lists its failed update with Retry; a healthy host's page says only that; a healthy
    // host whose alerts were read and name only others has no section and says healthy.
    // Catches: a failed Alerts.List that hides every reason, or that marks a healthy host degraded.
    let note = "The alerts were not read (UNIMPLEMENTED unimplemented): only what the host itself reported is shown.";
    let mut ui = host_ui(&live("node-02", json!([fetch_failed()])), None);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-02" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(
        out["out"],
        json!({"state": "degraded", "heading": "Why node-02 is not healthy",
               "items": [{"title": "The update of tierstore to 3.1.0 failed: fetch_failed",
                          "since": "Since 2027-01-15 05:30:00 UTC (started on the host).",
                          "buttons": ["retry-tierstore"]}],
               "taint": null, "log": [], "notes": [note], "reasonField": 0})
    );

    let healthy = live(
        "node-05",
        json!([{"service": "kbf", "health": "HEALTH_HEALTHY"}]),
    );
    let mut ui = host_ui(&healthy, None);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-05" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(
        out["out"],
        json!({"state": "healthy", "heading": "Alerts", "items": [], "taint": null, "log": [],
               "notes": [note], "reasonField": 0})
    );

    let mut ui = host_ui(&healthy, Some(mixed_alerts()));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-05" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(out["out"], json!({"state": "healthy", "section": null}));
}

#[test]
fn an_alert_alone_makes_a_live_host_degraded_even_while_it_updates() {
    // Asserts: a live host updating a service (`updating` before) whose only fault is an alert
    // naming it says degraded, and its page lists the alert with no button; a silent host with the
    // same kind of alert stays offline and still shows why.
    // Catches: the Macs whose prerequisite failed shown as healthy or updating.
    let mut ui = host_ui(
        &live(
            "mac-1",
            json!([{"service": "hostkeeper", "holdsSlot": true}]),
        ),
        Some(
            json!({"alerts": [alert("PREREQUISITE_FAILED", "WARNING", "mac-1", "", "xcode-command-line-tools failed", "1799993000")]}),
        ),
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "mac-1" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(out["out"]["state"], "degraded");
    assert_eq!(
        out["out"]["items"],
        json!([{"title": "A host prerequisite failed",
                "since": "Since 2027-01-15 06:03:20 UTC (first seen by the coordinator).", "buttons": []}])
    );

    let mut ui = host_ui(
        &json!({"nodeId": "node-06", "live": false, "lastHeartbeatUnix": "1799000000"}),
        Some(
            json!({"alerts": [alert("SILENT_HOST", "WARNING", "node-06", "", "node-06 has not heartbeated for 300 s", "1799990000")]}),
        ),
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "node-06" }"#,
        "[]",
        SECTION,
    ));
    assert_eq!(out["out"]["state"], "offline");
    assert_eq!(out["out"]["heading"], "Why node-06 is not healthy");
    assert_eq!(
        out["out"]["notes"],
        json!(["Nothing to press here: the coordinator's words above say what to do."])
    );
}

/// A gateway that answers Home's reads with `hosts` and `alerts` (none scripted: Alerts.List
/// fails) and an empty lab.
fn home_ui(hosts: &Value, alerts: Option<&Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({ "hosts": hosts }));
    ui.reply(&m("Releases/Status"), json!({}));
    if let Some(alerts) = alerts {
        ui.reply(&m("Alerts/List"), json!({ "alerts": alerts }));
    }
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    ui
}

/// The driver's view of Home: the degraded hosts strip (its count and each group's line), the
/// list's rows, and where a click on the first host of the strip goes.
const HOME: &str = r#"
const strip = nodes(tree).find((n) => n.attrs["name"] === "degraded");
const groups = nodes(strip).filter((n) => n.attrs["name"] === "degraded-reason").map((li) => text(li));
const links = nodes(strip).filter((n) => n.attrs["name"] === "open-degraded");
if (links.length > 0) {
  await links[links.length - 1].on.click();
}
return { strip: groups.length === 0 ? text(strip).trim() : groups, rows: rows(tree), navigations: seen.navigations };"#;

#[test]
fn home_counts_the_degraded_hosts_groups_their_reasons_and_each_row_says_why() {
    // Asserts: Home's strip says "Degraded hosts (6)" over five reasons, most hosts first, each
    // host a link to its page; each server's row says why under Why (two reasons joined, the
    // silent host's alert on an offline row), a healthy one "-", and a degraded host shows failed.
    // Catches: a degraded host counted twice, a row that says failed without why, a host link that
    // opens another host.
    let (hosts, alerts) = fleet();
    let mut ui = home_ui(&hosts, Some(&alerts));
    let out = ui.run(&page_driver("home", "{ list: true }", "[]", HOME));
    assert!(
        super_text(&out).contains("Degraded hosts (6)"),
        "{}",
        super_text(&out)
    );
    assert_eq!(
        out["out"]["strip"],
        json!([
            "host prerequisite failed 3 hosts: mac-1 mac-2 mac-3",
            "js-agent halted: tainted 1 host: node-04",
            "js-agent update failed: fetch_failed 1 host: node-02",
            "tierstore halted: draining 1 host: node-01",
            "tierstore unhealthy 1 host: node-01",
        ])
    );
    let why: Vec<Value> = out["out"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| json!([row[0], row[1], row[2]]))
        .collect();
    assert_eq!(
        why,
        [
            json!(["Server", "State", "Why"]),
            json!(["mac-1", "failed", "host prerequisite failed"]),
            json!(["mac-2", "failed", "host prerequisite failed"]),
            json!(["mac-3", "failed", "host prerequisite failed"]),
            json!([
                "node-01",
                "failed",
                "tierstore halted: draining; tierstore unhealthy"
            ]),
            json!(["node-02", "failed", "js-agent update failed: fetch_failed"]),
            json!(["node-04", "failed", "js-agent halted: tainted"]),
            json!(["node-05", "healthy", "-"]),
            json!(["node-06", "offline", "silent host"]),
        ]
    );
    assert_eq!(
        out["out"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "node-01"}}])
    );
}

#[test]
fn home_says_no_host_is_degraded_and_without_the_alerts_counts_from_the_hosts_own_reports() {
    // Asserts: a fleet of healthy hosts reads "No host is degraded."; when Alerts.List fails, the
    // count still holds the hosts whose own report is bad (node-01, node-02, node-04) and not the
    // Macs, whose only reason is an alert.
    // Catches: a failed Alerts.List that empties the fleet view, or a healthy fleet shown red.
    let healthy = json!([live(
        "node-05",
        json!([{"service": "kbf", "health": "HEALTH_HEALTHY"}])
    )]);
    let mut ui = home_ui(&healthy, Some(&json!([])));
    let out = ui.run(&page_driver("home", "{ list: true }", "[]", HOME));
    assert_eq!(out["out"]["strip"], "No host is degraded.");

    let (hosts, _) = fleet();
    let mut ui = home_ui(&hosts, None);
    let out = ui.run(&page_driver("home", "{ list: true }", "[]", HOME));
    assert!(
        super_text(&out).contains("Degraded hosts (3)"),
        "{}",
        super_text(&out)
    );
    assert_eq!(
        out["out"]["strip"],
        json!([
            "js-agent halted: tainted 1 host: node-04",
            "js-agent update failed: fetch_failed 1 host: node-02",
            "tierstore halted: draining 1 host: node-01",
            "tierstore unhealthy 1 host: node-01",
        ])
    );
}
