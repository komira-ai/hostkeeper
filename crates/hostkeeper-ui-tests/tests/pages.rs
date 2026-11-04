//! The six pages, run in the JavaScript engine against the fake gateway of `support`: what each
//! calls, what it shows, and what each of its buttons sends.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{
    assert_shows, bodies, hosts_reply, m, page_driver, render, shown, status_reply,
};

/// Markup in text the API returns: a claim, a reason, a log line.
const MARKUP: &str = "<img src=x onerror=alert(1)>";

#[test]
fn fleet_page_lists_hosts_and_alerts() {
    // Catches: a Fleet page that calls another method, leaves out a host, its role, a service's
    // version, health or marks, or an alert the API's answers carry.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Status"), status_reply());
    let out = ui.run(
        r#"
import { api, recorder, text } from "../test/kit.js";
import { render, fleetAlerts } from "./pages/fleet.js";
export async function main() {
  const { ctx, seen } = recorder(api("operator-token"), {}, []);
  const tree = await render(ctx);
  return { text: text(tree), alerts: fleetAlerts(HOSTS.hosts, STATUS), seen };
}"#
        .replace("HOSTS", &hosts_reply().to_string())
        .replace("STATUS", &status_reply().to_string())
        .as_str(),
    );
    assert_eq!(ui.methods(), [m("Hosts/List"), m("Releases/Status")]);
    assert_eq!(
        out["alerts"],
        json!([
            {"kind": "halt", "text": "api is halted by web-2, check disk_full"},
            {"kind": "held_on_bad_version", "text": "web-2 is held on api 1.3.0, which is marked bad"},
            {"kind": "waiting_for_operator", "text": "new-1 is waiting for an operator"},
            {"kind": "duplicate", "text": "clone-1 shares its identity with another host"},
            {"kind": "abandoned_while_updating", "text": "db-9 was abandoned while updating api: host gone"},
        ])
    );
    assert_shows(
        &out,
        &[
            "Alerts (5)",
            "halt",
            "waiting for operator",
            "duplicate",
            "held on bad version",
            "abandoned while updating",
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
    // Catches: an alert raised by a healthy fleet, or a halt read from a converged service.
    let mut ui = Ui::start();
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
    assert_eq!(bodies(&ui), [(m("Hosts/Get"), json!({"nodeId": "web-2"}))]);
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
    // confirmed, and labels that are not pairs.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_with_api());
    for method in [
        "Hosts/Exclude",
        "Hosts/Exclude",
        "Hosts/Include",
        "Hosts/Abort",
        "Services/Hold",
        "Services/Unhold",
        "Hosts/SetLabels",
    ] {
        ui.reply(&m(method), json!({}));
    }
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[false, true]",
        r#"
for (const name of ["exclude", "abort", "hold", "unhold"]) { await click(tree, name); }
type(tree, "reason", "  disk swap  ");
await click(tree, "exclude");
tick(tree, "abandon", true);
await click(tree, "exclude");
await click(tree, "exclude");
await click(tree, "include");
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
            "Excluded web-2 from api.",
            "Not excluded.",
            "Excluded web-2 from api.",
            "Included web-2 in api.",
            "Abort of api queued for web-2's next heartbeat.",
            "Held api on web-2.",
            "Released the hold of api on web-2.",
            "Labels are key=value pairs.",
            "Labels of web-2 set.",
        ])
    );
    assert_eq!(seen["refreshes"], 7);
    // The tree a status line mounts again shows what the state holds.
    assert_eq!(
        out["out"],
        json!({"abandon": true, "reason": "  disk swap  ", "labels": "rack=r2, zone=z1"})
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
    // state, a role's counts, the slot holder, the canary and bake, the halt or a bad version.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    let out = render(&mut ui, "rollout", "{}");
    assert_eq!(ui.methods(), [m("Releases/Status")]);
    assert_shows(
        &out,
        &[
            "api",
            "Desired 1.4.0",
            "halted",
            "Up 4 hosts",
            "held by web-1",
            "Waiting for the canary web-4",
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
            "CI sets the desired version",
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
         "claims": {"machineId": "m-0001", "dmiSerial": "SN-42", "addresses": ["192.0.2.10"]}},
        {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32), "renewalOf": "web-3",
         "duplicateClaim": true, "macNoncePresented": true, "claims": {"macSerial": "C02-X"}},
     ],
     "nextPageToken": "page-2"})
}

#[test]
fn enrollment_approve_sends_the_listed_key_hash_and_replace_is_false_by_default() {
    // Catches: a key hash recomputed or taken from elsewhere, replace on by default, a replace
    // sent without the operator's yes, and an approval without a host id or role.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), pending_reply());
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
await click(tree, "approve", 0);
type(tree, "host-id", "web-7", 0);
type(tree, "role", "web", 0);
type(tree, "services", "api, metrics", 0);
await click(tree, "approve", 0);
type(tree, "role", "web", 1);
tick(tree, "replace", true, 1);
await click(tree, "approve", 1);
type(tree, "reason", "re-imaged", 1);
await click(tree, "approve", 1);
await click(tree, "next-page");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (
                m("Enrollment/Approve"),
                json!({"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32),
                "hostId": "web-7", "role": "web", "services": ["api", "metrics"], "replace": false, "reason": ""})
            ),
            (
                m("Enrollment/Approve"),
                json!({"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32),
                "hostId": "web-3", "role": "web", "services": [], "replace": true, "reason": "re-imaged"})
            ),
        ]
    );
    let seen = &out["seen"];
    let question = format!(
        "Approve request {} as web-3 and revoke the certificate of any host that holds web-3?",
        "2e".repeat(32)
    );
    assert_eq!(seen["questions"], json!([question, question]));
    assert_eq!(
        seen["reports"],
        json!([
            "Name the host id and the role.",
            "Approved web-7: the host gets its certificate at its next poll.",
            "Not approved.",
            "Approved web-3: the host gets its certificate at its next poll.",
        ])
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
fn audit_page_passes_its_filters_to_the_call() {
    // Catches: a filter dropped from the request, an empty filter sent, and paging that does not
    // go on from the last entry.
    let mut ui = Ui::start();
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [{"seq": "6", "by": "ACTOR_OPERATOR", "action": "hold", "service": "api",
                            "nodeId": "web-1", "reason": "kernel patch"}],
               "nextSeq": "6"}),
    );
    ui.reply(&m("Audit/List"), json!({}));
    let out = ui.run(&page_driver(
        "audit",
        r#"{ actor: "operator", action: "hold", service: "api", nodeId: "web-1", sinceSeq: "5" }"#,
        "[]",
        r#"
await click(tree, "next-page");
await click(tree, "first-page");
type(tree, "service", " ");
type(tree, "action", "unhold");
choose(tree, "actor", "ci");
await click(tree, "show");
const { ctx: other } = recorder(ctx.api, {}, []);
return text(await render(other));
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Audit/List"),
                json!({"sinceSeq": "5", "limit": 100, "actor": "operator", "action": "hold",
                                      "service": "api", "nodeId": "web-1"})
            ),
            (m("Audit/List"), json!({"sinceSeq": "0", "limit": 100})),
        ]
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "audit", "params": {"actor": "operator", "action": "hold", "service": "api", "nodeId": "web-1", "sinceSeq": "6"}},
            {"name": "audit", "params": {"actor": "operator", "action": "hold", "service": "api", "nodeId": "web-1"}},
            {"name": "audit", "params": {"actor": "ci", "action": "unhold", "nodeId": "web-1"}},
        ])
    );
    assert_shows(
        &out,
        &["6", "operator", "hold", "api", "web-1", "kernel patch"],
    );
    assert!(
        out["out"]
            .as_str()
            .expect("the second page")
            .contains("No entry.")
    );
}

#[test]
fn every_page_shows_the_refusal_when_its_call_is_unimplemented() {
    // Catches: a page that shows nothing, or a blank, when the gateway does not serve its method
    // yet (its handler has not merged).
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
        assert_eq!(ui.requests().len(), 1, "{page}");
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
