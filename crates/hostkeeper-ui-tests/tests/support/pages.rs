//! What the page tests share: the page driver, the scripted replies they start from, and
//! queries over what the fake gateway recorded.

use serde_json::{Value, json};

use super::Ui;

pub const OP: &str = "hostkeeper.operator.v1";

pub fn m(method: &str) -> String {
    format!("{OP}.{method}")
}

/// A driver that renders `page` (a module under `ui/pages/`) with `params`, then runs `then`
/// (JavaScript with `tree`, `ctx`, `seen` and the kit's helpers in scope) and returns
/// `{text, seen, out}`: the page's text, what it did, and what `then` returned.
pub fn page_driver(page: &str, params: &str, answers: &str, then: &str) -> String {
    format!(
        r#"
import {{ api, recorder, text, named, type, tick, choose, click, nodes, rows, cell }} from "../test/kit.js";
import {{ render }} from "./pages/{page}.js";
export async function main() {{
  const {{ ctx, seen }} = recorder(api("operator-token"), {params}, {answers});
  const tree = await render(ctx);
  const shown = text(tree);
  const out = await (async () => {{ {then} }})();
  return {{ text: shown, seen, out: out === undefined ? null : out }};
}}"#
    )
}

pub fn render(ui: &mut Ui, page: &str, params: &str) -> Value {
    ui.run(&page_driver(page, params, "[]", ""))
}

pub fn shown(out: &Value) -> &str {
    out["text"].as_str().expect("the page's text")
}

pub fn assert_shows(out: &Value, expected: &[&str]) {
    let text = shown(out);
    for piece in expected {
        assert!(text.contains(piece), "{piece:?} is not shown in: {text}");
    }
}

pub fn bodies(ui: &Ui) -> Vec<(String, Value)> {
    ui.requests()
        .iter()
        .map(|r| (r.method().to_owned(), r.body()))
        .collect()
}

pub fn hosts_reply() -> Value {
    json!({"hosts": [
        {"nodeId": "web-1", "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
         "services": [{"service": "api", "up": true, "installedVersion": "1.4.0",
                       "health": "HEALTH_HEALTHY", "gateReason": "GATE_REASON_UP_TO_DATE",
                       "membership": "MEMBERSHIP_PRESENT", "holdsSlot": true}]},
        {"nodeId": "web-2", "role": "web", "live": true,
         "services": [{"service": "api", "installedVersion": "1.3.0", "health": "HEALTH_UNHEALTHY",
                       "gateReason": "GATE_REASON_HELD",
                       "holds": [{"scope": {"host": "web-2"}, "service": "api", "reason": "investigating",
                                  "setBy": "ops", "setUnix": "1800000000"}]}]},
        {"nodeId": "new-1", "role": "web",
         "services": [{"service": "api", "gateReason": "GATE_REASON_WAITING_FOR_OPERATOR"}]},
        {"nodeId": "clone-1", "role": "db",
         "services": [{"service": "api", "installedVersion": "1.4.0", "gateReason": "GATE_REASON_DUPLICATE",
                       "membership": "MEMBERSHIP_EXCLUDED", "excludedReason": "cloned disk"}]},
    ]})
}

/// A reply of `Alerts.List` with one alert of each of five kinds, a halt first, as the
/// coordinator orders them.
pub fn alerts_reply() -> Value {
    let alert = |kind: &str, severity: &str, host: &str, message: &str| {
        json!({"kind": format!("ALERT_KIND_{kind}"), "severity": format!("ALERT_SEVERITY_{severity}"),
               "hostId": host, "service": "api", "message": message,
               "firstObservedUnix": "1800000000"})
    };
    json!({"alerts": [
        alert("HALT", "CRITICAL", "web-2", "api is halted at 1.4.0 by web-2, check disk_full"),
        alert("WAITING_FOR_OPERATOR", "WARNING", "", "request ab waits for an operator's approval"),
        alert("FAILED_CANARY", "CRITICAL", "web-4", "web-4, a canary of api, failed 1.3.0"),
        alert("HELD_ON_BAD_VERSION", "WARNING", "web-2", "an operator holds web-2 on api 1.3.0"),
        alert("SILENT_HOST", "WARNING", "new-1", "new-1 has not heartbeated for 300 s"),
    ]})
}

/// The update plan of `api` in `status_reply`: three stages over eight eligible hosts, the second
/// entered by an operator, the first done and the second waiting for its Advance.
pub fn api_plan() -> Value {
    json!({
        "version": "1.4.0", "current": 1, "eligible": 8, "waiting": "PLAN_WAIT_MANUAL_ADVANCE",
        "stages": [
            {"number": 1, "percent": 1, "hosts": 1, "advance": "STAGE_ADVANCE_AUTO",
             "members": [{"nodeId": "web-1", "role": "web", "state": "PLAN_HOST_STATE_DONE"}]},
            {"number": 2, "percent": 50, "hosts": 4, "advance": "STAGE_ADVANCE_MANUAL",
             "members": [{"nodeId": "web-4", "role": "web", "state": "PLAN_HOST_STATE_WAITING"},
                         {"nodeId": "web-5", "role": "web", "state": "PLAN_HOST_STATE_WAITING",
                          "deferring": true}]},
            {"number": 3, "percent": 100, "hosts": 8, "advance": "STAGE_ADVANCE_AUTO"},
        ],
    })
}

/// The update plan of `web` in `status_reply`: one stage, paused by an operator.
pub fn paused_plan() -> Value {
    json!({
        "version": "0.9.0", "current": 1, "eligible": 1, "waiting": "PLAN_WAIT_PAUSED",
        "paused": true, "pausedBy": "ops", "pauseReason": "freeze for audit",
        "stages": [{"number": 1, "percent": 100, "hosts": 1, "advance": "STAGE_ADVANCE_AUTO",
                    "members": [{"nodeId": "web-9", "role": "web", "state": "PLAN_HOST_STATE_BAKING"}]}],
    })
}

pub fn status_reply() -> Value {
    json!({
        "services": [
            {"service": "api",
             "desired": {"version": "1.4.0"},
             "convergence": {"desired": "1.4.0", "state": "CONVERGENCE_STATE_HALTED",
                             "roles": [{"role": "web", "atDesired": 3, "updating": 1},
                                       {"role": "db", "atDesired": 1, "failed": 2, "down": 1}],
                             "force": {"version": "1.4.0", "generation": "2", "reason": "hotfix"}},
             "hosts": [
                 {"nodeId": "web-1", "role": "web", "live": true,
                  "view": {"service": "api", "holdsSlot": true, "gateReason": "GATE_REASON_UPDATING"}},
                 {"nodeId": "web-4", "role": "web", "live": true,
                  "view": {"service": "api", "gateReason": "GATE_REASON_ADVANCE_WAIT", "stage": 2}},
                 {"nodeId": "web-5", "role": "web", "live": true,
                  "view": {"service": "api", "gateReason": "GATE_REASON_BAKING"}}],
             "up": 4,
             "plan": api_plan(),
             "marks": {"halt": {"nodeId": "web-2", "check": "disk_full", "target": "1.4.0"},
                       "lastGood": "1.2.9", "bad": ["1.3.0"], "badByForce": ["1.2.0"]}},
            {"service": "web", "convergence": {"state": "CONVERGENCE_STATE_CONVERGED"},
             "plan": paused_plan()},
        ],
        "audit": [
            {"seq": "6", "by": "ACTOR_OPERATOR", "action": "hold", "service": "api", "nodeId": "web-2"},
            {"seq": "7", "by": "ACTOR_OPERATOR", "action": "exclude_abandon", "service": "api",
             "nodeId": "db-9", "reason": "host gone"},
        ],
    })
}
