//! The six pages, run in the JavaScript engine against the fake gateway of `support`: what each
//! calls, what it shows, and what each of its buttons sends. The Audit page's own tests are in
//! `audit_page.rs`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{
    alerts_reply, assert_shows, bodies, hosts_reply, m, page_driver, render, shown, status_reply,
};

/// Markup in text the API returns: a claim, a reason, a log line.
const MARKUP: &str = "<img src=x onerror=alert(1)>";

#[test]
fn fleet_page_lists_hosts_and_alerts() {
    // Catches: a Fleet page that calls another method, leaves out a host, its role, a service's
    // version, health or marks, or an alert `Alerts.List` answers (its kind, whether it is
    // critical, its message and when it was first observed), a failed canary among them.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Alerts/List"), alerts_reply());
    let out = ui.run(
        r#"
import { api, recorder, text } from "../test/kit.js";
import { render } from "./pages/fleet.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), {}, []);
  const tree = await render(ctx);
  const box = tree.children.find((child) => child.attrs && child.attrs.class === "alerts");
  // The kit joins text nodes with a space; the page's DOM does not.
  const one = (li) => text(li).split(/\s+/).join(" ").replace(" :", ":");
  return { text: text(tree), alerts: box.children[1].children.map(one) };
}"#,
    );
    assert_eq!(
        ui.methods(),
        [m("Hosts/List"), m("Releases/Status"), m("Alerts/List")]
    );
    assert!(
        bodies(&ui)
            .iter()
            .any(|(method, body)| *method == m("Alerts/List") && *body == json!({}))
    );
    assert_eq!(
        out["alerts"],
        json!([
            "halt (critical): api is halted at 1.4.0 by web-2, check disk_full (since 2027-01-15 08:00:00 UTC)",
            "waiting for operator: request ab waits for an operator's approval (since 2027-01-15 08:00:00 UTC)",
            "failed canary (critical): web-4, a canary of api, failed 1.3.0 (since 2027-01-15 08:00:00 UTC)",
            "held on bad version: an operator holds web-2 on api 1.3.0 (since 2027-01-15 08:00:00 UTC)",
            "silent host: new-1 has not heartbeated for 300 s (since 2027-01-15 08:00:00 UTC)",
        ])
    );
    assert_shows(
        &out,
        &[
            "Alerts (5)",
            "failed canary",
            "Hosts (4)",
            "web-1",
            "web-2",
            "new-1",
            "clone-1",
            "db",
            "live",
            "silent",
            "1.4.0",
            "healthy",
            "unhealthy",
            "not installed",
            "held",
            "holds the update slot",
            "excluded",
            "2027-01-15 08:00:00 UTC",
        ],
    );
}

#[test]
fn fleet_alerts_show_nothing_when_nothing_needs_a_person() {
    // Catches: an alert the page makes up when `Alerts.List` answers none (a halt read from the
    // status, a gate reason of a host).
    let mut ui = Ui::start();
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [{"nodeId": "web-1", "role": "web", "live": true}]}),
    );
    ui.reply(
        &m("Releases/Status"),
        json!({"quiet": true, "services": [{"service": "api", "convergence": {"state": "CONVERGENCE_STATE_CONVERGED"}}]}),
    );
    let out = render(&mut ui, "fleet", "{}");
    assert_shows(
        &out,
        &[
            "Alerts (0)",
            "Nothing needs a person.",
            "quiet start",
            "Hosts (1)",
        ],
    );
}

#[test]
fn host_page_shows_services_marks_and_last_update() {
    // Catches: a Host page that calls another method, or leaves out the role, a service's
    // installed version, health, hold and excluded marks, or a field of the last update.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {
            "nodeId": "web-2", "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
            "heartbeat": {"agent": {"name": "hostkeeper", "version": "0.4.1"}},
            "labels": {"rack": "r1"},
            "exclusions": [{"service": "metrics", "reason": "noisy"}],
            "services": [
                {"service": "api", "up": false, "holdsSlot": true, "gateReason": "GATE_REASON_HELD",
                 "membership": "MEMBERSHIP_PRESENT", "installedVersion": "1.3.0", "health": "HEALTH_UNHEALTHY",
                 "holds": [{"scope": {"role": "web"}, "service": "api", "reason": "freeze for audit",
                            "setBy": "ops", "setUnix": "1800000000"}],
                 "lastUpdate": {"target": "1.4.0", "outcome": "OUTCOME_FAILED",
                                "phase": "FAILURE_PHASE_AFTER_COMMIT", "class": "FAILURE_CLASS_RELEASE",
                                "reason": "health_check", "orderTry": "2", "orderId": "order-17",
                                "detail": "probe returned 503", "startedUnix": "1800000000",
                                "logTail": ["starting api", "probe failed"]}},
                {"service": "metrics", "membership": "MEMBERSHIP_EXCLUDED", "excludedReason": "noisy",
                 "installedVersion": "2.0.0", "health": "HEALTH_HEALTHY"},
            ],
        }}),
    );
    let out = render(&mut ui, "host", r#"{ nodeId: "web-2" }"#);
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-2"})),
            (m("Services/List"), json!({})),
        ]
    );
    assert_shows(
        &out,
        &[
            "Host web-2",
            "web",
            "live",
            "hostkeeper 0.4.1",
            "rack=r1",
            "metrics (noisy)",
            "1.3.0",
            "unhealthy",
            "holds the update slot",
            "held",
            "present",
            "Held (role web) by ops",
            "freeze for audit",
            "1.4.0",
            "failed",
            "after_commit",
            "release",
            "health_check",
            "probe returned 503",
            "2",
            "order-17",
            "starting api",
            "probe failed",
            "yes: noisy",
            "excluded",
            "healthy",
            "2.0.0",
        ],
    );
}

#[test]
fn host_page_without_a_host_asks_for_one_and_calls_nothing() {
    let mut ui = Ui::start();
    let out = ui.run(&page_driver(
        "host",
        "{}",
        "[]",
        r#"await click(tree, "open"); type(tree, "node-id", " web-3 "); await click(tree, "open");"#,
    ));
    assert_shows(&out, &["Choose a host on the Fleet page"]);
    assert_eq!(out["seen"]["reports"], json!(["Name a host first."]));
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "web-3"}}])
    );
    assert!(ui.requests().is_empty());
}

fn host_with_api() -> Value {
    json!({"host": {"nodeId": "web-2", "role": "web",
                    "services": [{"service": "api", "installedVersion": "1.3.0"}]}})
}

#[test]
fn host_page_buttons_each_send_one_call() {
    // Catches: a button that sends another method, more than one call, a body without the host,
    // the service or the reason, an action needing a reason sent without one, an abandon not
    // confirmed, a reason over 256 bytes or with a control character, an abort reported as
    // queued when the coordinator queued none, and labels that are not pairs.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_with_api());
    for method in [
        "Hosts/Exclude",
        "Hosts/Exclude",
        "Hosts/Include",
        "Services/Hold",
        "Services/Unhold",
        "Hosts/SetLabels",
    ] {
        ui.reply(&m(method), json!({}));
    }
    ui.reply(&m("Hosts/Abort"), json!({"queued": true}));
    ui.reply(&m("Hosts/Abort"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[false, true]",
        r#"
for (const name of ["exclude", "abort", "hold", "unhold"]) { await click(tree, name); }
type(tree, "reason", "bad\treason");
await click(tree, "exclude");
type(tree, "reason", "\u00e9".repeat(129));
await click(tree, "hold");
type(tree, "reason", "  disk swap  ");
await click(tree, "exclude");
tick(tree, "abandon", true);
await click(tree, "exclude");
await click(tree, "exclude");
await click(tree, "include");
await click(tree, "abort");
await click(tree, "abort");
await click(tree, "hold");
await click(tree, "unhold");
type(tree, "labels", "rack");
await click(tree, "set-labels");
type(tree, "labels", "rack=r2, zone=z1");
await click(tree, "set-labels");
return {
  abandon: named(tree, "abandon")[0].attrs.checked,
  reason: named(tree, "reason")[0].attrs.value,
  labels: named(tree, "labels")[0].attrs.value,
};
"#,
    ));
    let reason = "disk swap";
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-2"})),
            (m("Services/List"), json!({})),
            (
                m("Hosts/Exclude"),
                json!({"service": "api", "nodeId": "web-2", "reason": reason, "abandon": false})
            ),
            (
                m("Hosts/Exclude"),
                json!({"service": "api", "nodeId": "web-2", "reason": reason, "abandon": true})
            ),
            (
                m("Hosts/Include"),
                json!({"service": "api", "nodeId": "web-2"})
            ),
            (
                m("Hosts/Abort"),
                json!({"nodeId": "web-2", "service": "api", "reason": reason})
            ),
            (
                m("Hosts/Abort"),
                json!({"nodeId": "web-2", "service": "api", "reason": reason})
            ),
            (
                m("Services/Hold"),
                json!({"scope": {"host": "web-2"}, "service": "api", "reason": reason})
            ),
            (
                m("Services/Unhold"),
                json!({"scope": {"host": "web-2"}, "service": "api", "reason": reason})
            ),
            (
                m("Hosts/SetLabels"),
                json!({"nodeId": "web-2", "labels": {"rack": "r2", "zone": "z1"}})
            ),
        ]
    );
    let seen = &out["seen"];
    assert_eq!(
        seen["questions"],
        json!([
            "Exclude web-2 from api and free the update slot it holds (abandon)?",
            "Exclude web-2 from api and free the update slot it holds (abandon)?",
        ])
    );
    assert_eq!(
        seen["reports"],
        json!([
            "Give a reason first.",
            "Give a reason first.",
            "Give a reason first.",
            "Give a reason first.",
            "A reason has no control character (a tab or a line break, for example).",
            "A reason is at most 256 bytes; this one is 258.",
            "Excluded web-2 from api.",
            "Not excluded.",
            "Excluded web-2 from api.",
            "Included web-2 in api.",
            "Abort of api queued for web-2's next heartbeat.",
            "Nothing to abort: web-2 runs nothing of api, so no abort was queued.",
            "Held api on web-2.",
            "Released the hold of api on web-2.",
            "Labels are key=value pairs, each key and value 1 to 32 of a-z, 0-9 and -: not rack.",
            "Labels of web-2 set.",
        ])
    );
    assert_eq!(seen["refreshes"], 8);
    // The tree a status line mounts again shows what the state holds.
    assert_eq!(
        out["out"],
        json!({"abandon": true, "reason": "  disk swap  ", "labels": "rack=r2, zone=z1"})
    );
}

#[test]
fn set_labels_checks_every_pair_and_asks_before_removing_all() {
    // Catches: a key that reaches the object's prototype (`__proto__`) and is dropped, so that
    // the request silently removes the labels; a key given twice; keys and values off their
    // grammar or over 32 bytes; more than 8 labels; and an empty set sent without the operator's
    // yes. The limits themselves (8 labels of 32-byte keys and values) are sent.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-3", "role": "web", "labels": {"rack": "r1"}}}),
    );
    ui.reply(&m("Hosts/SetLabels"), json!({}));
    ui.reply(&m("Hosts/SetLabels"), json!({}));
    let long = "a".repeat(32);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-3" }"#,
        "[false, true]",
        r#"
const shown = named(tree, "labels")[0].attrs.value;
const tries = [
  "__proto__=x", "rack=r1 rack=r2", "Rack=r1", "a".repeat(33) + "=v", "=v", "k=", "rack",
  Array.from({ length: 9 }, (_, i) => `a${i}=v`).join(", "), "", "",
  Array.from({ length: 8 }, (_, i) => `${"a".repeat(31)}${i}=${"b".repeat(32)}`).join(" "),
];
for (const labels of tries) {
  type(tree, "labels", labels);
  await click(tree, "set-labels");
}
return shown;
"#,
    ));
    assert_eq!(out["out"], "rack=r1");
    let pairs = |pair: &str| {
        format!(
            "Labels are key=value pairs, each key and value 1 to 32 of a-z, 0-9 and -: not {pair}."
        )
    };
    assert_eq!(
        out["seen"]["reports"],
        json!([
            pairs("__proto__=x"),
            "The label rack is given twice.",
            pairs("Rack=r1"),
            pairs(&format!("{long}a=v")),
            pairs("=v"),
            pairs("k="),
            pairs("rack"),
            "A host has at most 8 labels, not 9.",
            "Labels not changed.",
            "Labels of web-3 set.",
            "Labels of web-3 set.",
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Remove every label of web-3?",
            "Remove every label of web-3?"
        ])
    );
    let eight: serde_json::Map<String, Value> = (0..8)
        .map(|i| (format!("{}{i}", "a".repeat(31)), json!("b".repeat(32))))
        .collect();
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-3"})),
            (m("Services/List"), json!({})),
            (
                m("Hosts/SetLabels"),
                json!({"nodeId": "web-3", "labels": {}})
            ),
            (
                m("Hosts/SetLabels"),
                json!({"nodeId": "web-3", "labels": eight})
            ),
        ]
    );
}

#[test]
fn a_refused_action_reports_the_code_and_reason() {
    // Catches: a refusal of an action lost, or shown as success.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_with_api());
    ui.refuse(&m("Hosts/Include"), 404, "NOT_FOUND", "unknown_service");
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"await click(tree, "include");"#,
    ));
    assert_eq!(out["seen"]["reports"], json!(["NOT_FOUND unknown_service"]));
    assert_eq!(out["seen"]["refreshes"], 0);
}

#[test]
fn rollout_page_shows_desired_against_converged() {
    // Catches: a Rollout page that calls another method, or leaves out the desired version, the
    // state, a role's counts, the slot holders, the plan and why it waits, the bake, the halt or a
    // bad version.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    let out = render(&mut ui, "rollout", "{}");
    assert_eq!(ui.methods(), [m("Releases/Status"), m("Services/List")]);
    assert_shows(
        &out,
        &[
            "api",
            "Desired 1.4.0",
            "halted",
            "Up 4 hosts",
            "Update slots held by web-1",
            "Plan 1 (1%), 4 (50%, manual), 8 (100%) hosts of 8 eligible",
            "Stage 1 of 3",
            "Waiting manual_advance, stage 2",
            "Waiting paused by ops: freeze for audit",
            "Baking web-5",
            "by web-2, check disk_full, target 1.4.0",
            "Last good 1.2.9",
            "Bad versions 1.3.0",
            "1.2.0",
            "1.4.0 (hotfix)",
            "web 3 1 0 0",
            "db 1 0 2 1",
            "Unmark 1.3.0",
            "Clear the halt",
            "converged",
            "none set",
            "Halt",
            "CI or an operator sets the desired version",
        ],
    );
}

#[test]
fn rollout_page_buttons_each_send_one_call() {
    // Catches: a mark, unmark, halt, clear or converge that sends another method or body, one
    // sent without a reason, and a halt sent without the operator's yes.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    for method in [
        "Releases/MarkBad",
        "Releases/UnmarkBad",
        "Releases/ClearHalt",
        "Releases/Halt",
    ] {
        ui.reply(&m(method), json!({}));
    }
    ui.reply(
        &m("Releases/Converge"),
        json!({"convergence": {"desired": "1.4.0", "state": "CONVERGENCE_STATE_IN_PROGRESS"}}),
    );
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[false, true]",
        r#"
await click(tree, "mark-bad");
type(tree, "reason", "crash loop");
await click(tree, "mark-bad");
type(tree, "bad-version", "1.4.0");
await click(tree, "mark-bad");
await click(tree, "unmark-bad");
await click(tree, "clear-halt");
type(tree, "reason", "maintenance", 1);
await click(tree, "halt");
await click(tree, "halt");
await click(tree, "converge");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Releases/Status"), json!({})),
            (m("Services/List"), json!({})),
            (
                m("Releases/MarkBad"),
                json!({"service": "api", "version": "1.4.0", "reason": "crash loop"})
            ),
            (
                m("Releases/UnmarkBad"),
                json!({"service": "api", "version": "1.3.0", "reason": "crash loop"})
            ),
            (
                m("Releases/ClearHalt"),
                json!({"service": "api", "reason": "crash loop"})
            ),
            (
                m("Releases/Halt"),
                json!({"service": "web", "reason": "maintenance"})
            ),
            (m("Releases/Converge"), json!({"service": "api"})),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Name the version to mark bad.",
            "Marked api 1.4.0 bad.",
            "Unmarked api 1.3.0.",
            "Cleared the halt of api.",
            "Not halted.",
            "Halted web.",
            "api: in_progress, desired 1.4.0.",
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Halt web? No host of it is ordered until the halt is cleared.",
            "Halt web? No host of it is ordered until the halt is cleared.",
        ])
    );
}

#[test]
fn holds_page_requires_a_reason() {
    // Catches: a hold or a release sent with an empty or blank reason, a scope sent wrong, and a
    // hold listed without its reason, scope, setter or time.
    let mut ui = Ui::start();
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "api", "hosts": ["web-1"]}, {"service": "web"}],
               "holds": [{"scope": {"role": "web"}, "service": "api", "reason": "freeze for audit",
                          "setBy": "ops", "setUnix": "1800000000"},
                         {"scope": {"fleet": {}}, "service": "web", "reason": "incident"}]}),
    );
    for method in [
        "Services/Hold",
        "Services/Hold",
        "Services/Hold",
        "Services/Unhold",
    ] {
        ui.reply(&m(method), json!({}));
    }
    let out = ui.run(&page_driver(
        "holds",
        "{}",
        "[]",
        r#"
await click(tree, "hold");
choose(tree, "hold-service", "api");
await click(tree, "hold");
type(tree, "hold-reason", "   ");
await click(tree, "hold");
type(tree, "hold-reason", "kernel patch");
await click(tree, "hold");
type(tree, "hold-value", "web-1");
await click(tree, "hold");
choose(tree, "hold-scope", "role");
type(tree, "hold-value", " ");
await click(tree, "hold");
type(tree, "hold-value", "db");
await click(tree, "hold");
choose(tree, "hold-scope", "fleet");
const scopes = named(tree, "hold-scope")[0].children.map((o) => o.attrs.selected);
await click(tree, "hold");
await click(tree, "unhold");
type(tree, "unhold-reason", "audit done");
await click(tree, "unhold");
return scopes;
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Services/List"), json!({})),
            (
                m("Services/Hold"),
                json!({"scope": {"host": "web-1"}, "service": "api", "reason": "kernel patch"})
            ),
            (
                m("Services/Hold"),
                json!({"scope": {"role": "db"}, "service": "api", "reason": "kernel patch"})
            ),
            (
                m("Services/Hold"),
                json!({"scope": {"fleet": {}}, "service": "api", "reason": "kernel patch"})
            ),
            (
                m("Services/Unhold"),
                json!({"scope": {"role": "web"}, "service": "api", "reason": "audit done"})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the service.",
            "A hold needs a reason.",
            "A hold needs a reason.",
            "Name the host.",
            "Held api (host web-1).",
            "Name the role.",
            "Held api (role db).",
            "Held api (fleet).",
            "Give a reason for the release first.",
            "Released the hold of api (role web).",
        ])
    );
    assert_eq!(out["out"], json!([false, false, true]));
    assert_shows(
        &out,
        &[
            "Holds that stand (2)",
            "api",
            "role web",
            "freeze for audit",
            "ops",
            "2027-01-15 08:00:00 UTC",
            "fleet",
            "incident",
        ],
    );
}

fn pending_reply() -> Value {
    json!({"requests": [
        {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "image": "debian-hk",
         "firstSeenUnix": "1800000000",
         "claims": {"machineId": "m-0001", "dmiSerial": "SN-42", "productUuid": "4c4c4544-0042",
                    "addresses": ["192.0.2.10", "192.0.2.11"]}},
        {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32), "renewalOf": "web-3",
         "duplicateClaim": true, "macNoncePresented": true,
         "claims": {"macSerial": "C02-X", "platformUuid": "a1b2c3d4-0042"}},
     ],
     "nextPageToken": "page-2"})
}

/// The host groups the approval's dropdown lists, but the built-in `all`, which every host is in.
fn two_groups() -> Value {
    json!({"hostGroups": [{"group": {"name": "all"}}, {"group": {"name": "storage"}}, {"group": {"name": "web"}}]})
}

#[test]
fn enrollment_approve_sends_the_listed_key_hash_the_chosen_group_and_replace_false_by_default() {
    // Catches: a key hash recomputed or taken from elsewhere, replace on by default, a replace
    // sent without the operator's yes, an approval sent with a host id off its grammar or none, a
    // group other than the one chosen (or a group sent when none is chosen), a Role, Services or
    // Reason field on the page, a dropdown that lists anything but the groups and a blank, and a
    // refusal taken as success or losing what the operator typed and chose.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), pending_reply());
    ui.reply(&m("HostGroups/List"), two_groups());
    for (status, code, reason) in [
        (412, "FAILED_PRECONDITION", "key_mismatch"),
        (412, "FAILED_PRECONDITION", "not_pending"),
        (409, "ALREADY_EXISTS", "host_id_taken"),
        (412, "FAILED_PRECONDITION", "duplicate_claim"),
        (412, "FAILED_PRECONDITION", "denied"),
    ] {
        ui.refuse(&m("Enrollment/Approve"), status, code, reason);
    }
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"hostId": "web-7", "certificateIssuedAtPoll": true}),
    );
    ui.reply(
        &m("Enrollment/Approve"),
        json!({"hostId": "web-3", "certificateIssuedAtPoll": true}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[false, true]",
        r#"
const absent = ["role", "services", "reason"].map((n) => nodes(tree).filter((x) => (x.attrs || {}).name === n).length);
const offered = choices(tree, "host-group", 0);
for (const hostId of ["", "-web-7"]) {
  type(tree, "host-id", hostId, 0);
  await click(tree, "approve", 0);
}
type(tree, "host-id", " web-7 ", 0);
choose(tree, "host-group", "web", 0);
for (let i = 0; i < 6; i++) {
  await click(tree, "approve", 0);
}
const kept = [named(tree, "host-id")[0].attrs.value, chosen(tree, "host-group", 0)];
tick(tree, "replace", true, 1);
await click(tree, "approve", 1);
await click(tree, "approve", 1);
await click(tree, "next-page");
return { absent, offered, kept, second: chosen(tree, "host-group", 1) };
"#,
    ));
    let first = json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "hostId": "web-7",
        "hostGroup": "web", "replace": false});
    let second = json!({"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32), "hostId": "web-3",
        "hostGroup": "", "replace": true});
    let mut expected = vec![
        (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
        (m("HostGroups/List"), json!({})),
    ];
    expected.extend((0..6).map(|_| (m("Enrollment/Approve"), first.clone())));
    expected.push((m("Enrollment/Approve"), second));
    assert_eq!(bodies(&ui), expected);
    let seen = &out["seen"];
    let question = format!(
        "Approve request {} as web-3 and revoke the certificate of any host that holds web-3?",
        "2e".repeat(32)
    );
    assert_eq!(seen["questions"], json!([question, question]));
    assert_eq!(
        seen["reports"],
        json!([
            "Name the host id.",
            "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.",
            "FAILED_PRECONDITION key_mismatch",
            "FAILED_PRECONDITION not_pending",
            "ALREADY_EXISTS host_id_taken",
            "FAILED_PRECONDITION duplicate_claim",
            "FAILED_PRECONDITION denied",
            "Approved web-7 into host group web: the host gets its certificate at its next poll.",
            "Not approved.",
            "Approved web-3 with no host group (it runs the packages of all until it is put in one): the host gets its certificate at its next poll.",
        ])
    );
    assert_eq!(seen["refreshes"], 2, "a refused approval refreshes nothing");
    assert_eq!(
        out["out"],
        json!({"absent": [0, 0, 0], "offered": ["", "storage", "web"],
               "kept": [" web-7 ", "web"], "second": ""}),
        "a refusal keeps what the operator typed and chose"
    );
    assert_eq!(
        seen["navigations"],
        json!([{"name": "enrollment", "params": {"pageToken": "page-2"}}])
    );
    let key = "ab".repeat(32);
    assert_shows(
        &out,
        &[
            key.as_str(),
            "m-0001",
            "SN-42",
            "192.0.2.10",
            "debian-hk",
            "Duplicate claim",
            "Renewal of web-3",
            "C02-X",
            "Mac nonce presented yes",
            "Leave it off unless you mean that.",
            "Host group",
        ],
    );
}

#[test]
fn enrollment_page_passes_its_page_token() {
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), json!({}));
    let out = ui.run(&page_driver(
        "enrollment",
        r#"{ pageToken: "page-2" }"#,
        "[]",
        r#"await click(tree, "first-page");"#,
    ));
    assert_eq!(
        bodies(&ui),
        [(
            m("Enrollment/List"),
            json!({"limit": 50, "pageToken": "page-2"})
        )]
    );
    assert_shows(&out, &["No host is waiting."]);
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "enrollment", "params": {}}])
    );
}

#[test]
fn every_page_shows_the_refusal_when_its_call_is_unimplemented() {
    // Catches: a page that shows nothing, or a blank, when the gateway does not serve its method
    // yet (its handler has not merged). A page asks once, except the host page, which also asks
    // Services.List for its assigned services after Hosts.Get fails and shows both refusals.
    // The rollout page stops at its first call: without Releases.Status it has no services to
    // show, so it never asks Services.List.
    for (page, params) in [
        ("fleet", "{}"),
        ("host", r#"{ nodeId: "web-1" }"#),
        ("rollout", "{}"),
        ("holds", "{}"),
        ("enrollment", "{}"),
        ("audit", "{}"),
    ] {
        let mut ui = Ui::start();
        let out = render(&mut ui, page, params);
        assert!(
            shown(&out).contains("UNIMPLEMENTED unimplemented"),
            "{page}: {}",
            shown(&out)
        );
        let expected = if page == "host" { 2 } else { 1 };
        assert_eq!(ui.requests().len(), expected, "{page}");
        if page == "host" {
            assert_eq!(
                shown(&out).matches("UNIMPLEMENTED unimplemented").count(),
                2
            );
        }
    }
}

#[test]
fn untrusted_text_is_rendered_as_text() {
    // Catches: a page that builds markup from a claim, a reason or a log line: each must become a
    // text node, with no element made from it.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [{"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "image": MARKUP,
                             "claims": {"machineId": MARKUP, "addresses": [MARKUP]}}]}),
    );
    ui.reply(
        &m("Services/List"),
        json!({"holds": [{"scope": {"host": MARKUP}, "service": "api", "reason": MARKUP, "setBy": MARKUP}]}),
    );
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-1", "role": MARKUP, "services": [{"service": "api",
            "lastUpdate": {"reason": MARKUP, "detail": MARKUP, "logTail": [MARKUP, "plain"]},
            "holds": [{"scope": {"host": "web-1"}, "service": "api", "reason": MARKUP}]}]}}),
    );
    let out = ui.run(
        r#"
import { api, recorder, mounted, domTexts } from "../test/kit.js";
import * as enrollment from "./pages/enrollment.js";
import * as holds from "./pages/holds.js";
import * as host from "./pages/host.js";
export async function main() {
  const client = api("operator-token");
  const out = [];
  for (const [page, params] of [[enrollment, {}], [holds, {}], [host, { nodeId: "web-1" }]]) {
    const { ctx } = recorder(client, params, []);
    const doc = mounted(await page.render(ctx));
    out.push({
      tags: Array.from(new Set(doc.created.map((e) => e.tagName))),
      attributes: doc.created.flatMap((e) => Object.keys(e.attributes)),
      attributeValues: doc.created.flatMap((e) => Object.values(e.attributes)),
      texts: domTexts(doc.root),
    });
  }
  return out;
}"#,
    );
    for (index, (page, expected)) in [("enrollment", 3), ("holds", 3), ("host", 5)]
        .into_iter()
        .enumerate()
    {
        let found = &out[index];
        let tags: Vec<&str> = found["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!tags.contains(&"IMG"), "{page}: {tags:?}");
        assert!(
            found["attributes"]
                .as_array()
                .expect("names")
                .iter()
                .all(|a| !a.as_str().unwrap_or("").starts_with("on")),
            "{page}"
        );
        assert!(
            !found["attributeValues"].to_string().contains("onerror"),
            "{page}"
        );
        let carrying = found["texts"]
            .as_array()
            .expect("texts")
            .iter()
            .filter(|t| t.as_str().unwrap_or("").contains(MARKUP))
            .count();
        assert!(
            carrying >= expected,
            "{page}: {carrying} text nodes carry the markup"
        );
    }
}
