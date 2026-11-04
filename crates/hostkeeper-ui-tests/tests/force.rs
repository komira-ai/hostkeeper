//! The Rollout page's force: the dialog that names the hosts it reaches, the typed count it
//! needs, what happens when the hosts cannot be counted or the force is refused or unanswered,
//! and the orders in which counts, cancels and the force's answer may arrive, run in the
//! JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, hosts_reply, m, page_driver, status_reply};

fn force_driver(steps: &str) -> String {
    page_driver(
        "rollout",
        "{}",
        "[]",
        &format!(
            r#"
let page = tree;
const latest = () => (seen.shown.length > 0 ? seen.shown[seen.shown.length - 1] : page);
const steps = [];
{steps}
return steps;"#
        ),
    )
}

#[test]
fn the_rollout_force_dialog_names_the_count_service_version_and_target() {
    // Catches: a force sent from the page without a fresh Hosts.List, without the typed count,
    // twice for a double click, or with a dialog that does not name the hosts, the service, the
    // version and the target.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Force"), json!({"targeted": 2, "held": ["web-2"], "skipped": [{"nodeId": "web-9", "reason": "daemon_too_old"}]}));
    let out = ui.run(&force_driver(
        r#"
await click(page, "force-prepare");
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
choose(page, "force-target", "role");
await click(page, "force-prepare");
type(page, "force-role", "web");
await click(page, "force-prepare");
steps.push(text(latest()));
type(latest(), "force-count", "3");
await click(latest(), "force-send");
type(latest(), "force-count", "1");
const send = named(latest(), "force-send")[0];
await Promise.all([send.on.click(), send.on.click()]);
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Releases/Status"), json!({})),
            (m("Hosts/List"), json!({})),
            (
                m("Releases/Force"),
                json!({"service": "api", "version": "1.3.0", "reason": "roll back", "includeHeld": false, "role": "web"})
            ),
        ]
    );
    let dialog = out["out"][0].as_str().expect("the dialog's text");
    assert!(
        dialog.contains("Force api to 1.3.0 on 1 hosts (role web)"),
        "{dialog}"
    );
    assert!(dialog.contains("Type 1 to confirm"), "{dialog}");
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Name the service, the version and a reason.",
            "Name the role.",
            "Not sent: type 1, the number of hosts, to confirm.",
            "Force sent: 2 hosts ordered; held, not ordered: web-2; skipped: web-9 (daemon_too_old).",
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn the_force_button_stays_disabled_when_the_hosts_cannot_be_counted() {
    // Catches: a Force button that stays usable after Hosts.List failed.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.refuse(&m("Hosts/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&force_driver(
        r#"
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
choose(page, "force-target", "hosts");
type(page, "force-hosts", "web-1, web-2");
tick(page, "force-include-held", true);
await click(page, "force-prepare");
const button = named(latest(), "force-send")[0];
steps.push(text(latest()), button.attrs.disabled === true, Object.keys(button.on).length);
"#,
    ));
    assert_eq!(ui.methods(), [m("Releases/Status"), m("Hosts/List")]);
    let dialog = out["out"][0].as_str().expect("the dialog's text");
    assert!(
        dialog.contains("could not be counted (UNAVAILABLE store_failed)"),
        "{dialog}"
    );
    assert_eq!(out["out"][1], true);
    assert_eq!(out["out"][2], 0);
}

#[test]
fn a_fleet_force_confirms_the_service_and_cancel_sends_nothing() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Hosts/List"), hosts_reply());
    let out = ui.run(&force_driver(
        r#"
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
tick(page, "force-include-held", true);
await click(page, "force-prepare");
steps.push(text(latest()));
await click(latest(), "force-cancel");
steps.push(nodes(latest()).some((n) => n.attrs["name"] === "force-send"));
"#,
    ));
    let dialog = out["out"][0].as_str().expect("the dialog's text");
    assert!(
        dialog.contains("on 3 hosts (the whole fleet, held hosts included)"),
        "{dialog}"
    );
    assert_eq!(out["out"][1], false);
    assert_eq!(ui.methods(), [m("Releases/Status"), m("Hosts/List")]);
}

fn force_bodies(ui: &Ui) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Releases/Force"))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn listed_host_zero_host_and_fleet_forces_are_sent_as_confirmed() {
    // Catches: a listed-host or fleet force sent with another body, a count that counts unknown
    // or uninstalled hosts wrongly, and a force to no host that cannot be confirmed with 0.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    for _ in 0..3 {
        ui.reply(&m("Hosts/List"), hosts_reply());
        ui.reply(&m("Releases/Force"), json!({"targeted": 1}));
    }
    let out = ui.run(&force_driver(
        r#"
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
choose(page, "force-target", "hosts");
type(page, "force-hosts", "web-1, new-1 ghost");
await click(page, "force-prepare");
steps.push(text(latest()));
type(latest(), "force-count", "2");
await click(latest(), "force-send");
choose(page, "force-target", "role");
type(page, "force-role", "cache");
await click(page, "force-prepare");
steps.push(text(latest()));
type(latest(), "force-count", "0");
await click(latest(), "force-send");
choose(page, "force-target", "fleet");
tick(page, "force-include-held", true);
await click(page, "force-prepare");
steps.push(text(latest()));
type(latest(), "force-count", "3");
await click(latest(), "force-send");
"#,
    ));
    let dialogs: Vec<&str> = out["out"]
        .as_array()
        .expect("the dialogs")
        .iter()
        .map(|d| d.as_str().expect("a dialog"))
        .collect();
    assert!(
        dialogs[0].contains("on 2 hosts (hosts web-1, new-1, ghost)"),
        "{}",
        dialogs[0]
    );
    assert!(
        dialogs[1].contains("on 0 hosts (role cache)"),
        "{}",
        dialogs[1]
    );
    assert!(dialogs[1].contains("Type 0 to confirm"), "{}", dialogs[1]);
    assert!(
        dialogs[2].contains("on 3 hosts (the whole fleet, held hosts included)"),
        "{}",
        dialogs[2]
    );
    let base = json!({"service": "api", "version": "1.3.0", "reason": "roll back"});
    let with = |extra: Value| {
        let mut body = base.clone();
        body.as_object_mut()
            .expect("an object")
            .extend(extra.as_object().expect("an object").clone());
        body
    };
    assert_eq!(
        force_bodies(&ui),
        [
            with(json!({"includeHeld": false, "hosts": {"nodeIds": ["web-1", "new-1", "ghost"]}})),
            with(json!({"includeHeld": false, "role": "cache"})),
            with(json!({"includeHeld": true, "fleet": {"confirmService": "api"}})),
        ]
    );
    assert_eq!(out["seen"]["refreshes"], 3);
}

#[test]
fn a_refused_force_keeps_the_dialog_and_needs_the_number_typed_again() {
    // Catches: a refused force that loses its dialog, or one retried without the number typed
    // again.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.refuse(&m("Releases/Force"), 412, "FAILED_PRECONDITION", "held_bad");
    ui.reply(&m("Releases/Force"), json!({"targeted": 1}));
    let out = ui.run(&force_driver(
        r#"
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
choose(page, "force-target", "role");
type(page, "force-role", "web");
await click(page, "force-prepare");
type(latest(), "force-count", "1");
await click(latest(), "force-send");
steps.push(named(latest(), "force-count")[0].attrs.value);
await click(latest(), "force-send");
type(latest(), "force-count", "1");
await click(latest(), "force-send");
"#,
    ));
    assert_eq!(out["out"], json!([""]));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not forced: FAILED_PRECONDITION held_bad. Type 1 again to retry.",
            "Not sent: type 1, the number of hosts, to confirm.",
            "Force sent: 1 hosts ordered.",
        ])
    );
    let body = json!({"service": "api", "version": "1.3.0", "reason": "roll back", "includeHeld": false, "role": "web"});
    assert_eq!(force_bodies(&ui), [body.clone(), body]);
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn a_force_with_no_answer_may_have_been_sent_and_needs_a_new_count() {
    // Catches: a force whose answer was lost offered for a second send as if nothing happened.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.drop_request(&m("Releases/Force"));
    let out = ui.run(&force_driver(
        r#"
choose(page, "force-service", "api");
type(page, "force-version", "1.3.0");
type(page, "force-reason", "roll back");
await click(page, "force-prepare");
type(latest(), "force-count", "2");
await click(latest(), "force-send");
steps.push(nodes(latest()).some((n) => n.attrs["name"] === "force-send"));
"#,
    ));
    assert_eq!(out["out"], json!([false]));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "The force may have been sent (NETWORK). Look at this page and the audit log before forcing again; \
             a new force needs a new count."
        ])
    );
    assert_eq!(force_bodies(&ui).len(), 1);
    assert_eq!(out["seen"]["refreshes"], 0);
}

/// A driver that runs `steps` against the Rollout page an earlier run kept in `globalThis.t`
/// (the page, what it did, and its latest tree), with the kit's helpers in scope.
fn later(steps: &str) -> String {
    format!(
        r#"
import {{ named, nodes, text, type, click }} from "../test/kit.js";
export async function main() {{
  const t = globalThis.t;
  const has = (name) => nodes(t.latest()).some((n) => n.attrs["name"] === name);
  {steps}
}}"#
    )
}

#[test]
fn cancel_and_recount_while_a_count_is_pending_and_no_cancel_once_the_force_is_sent() {
    // Catches: a count that finishes after Cancel, or after a newer count, and brings back its
    // dialog; a Cancel offered, or claiming success, after the force left; and a second force
    // sent from a control drawn while the first is in flight.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    for _ in 0..3 {
        ui.hold(&m("Hosts/List"), 200, &hosts_reply().to_string());
    }
    ui.hold(
        &m("Releases/Force"),
        200,
        &json!({"targeted": 1}).to_string(),
    );
    let counting = ui.run(
        r#"
import { api, recorder, text, type, choose, click } from "../test/kit.js";
import { render } from "./pages/rollout.js";
export async function main() {
  const { ctx, seen } = recorder(api("operator-token"), {}, []);
  const page = await render(ctx);
  globalThis.t = { page, seen, latest: () => (seen.shown.length > 0 ? seen.shown[seen.shown.length - 1] : page) };
  choose(page, "force-service", "api");
  type(page, "force-version", "1.3.0");
  type(page, "force-reason", "roll back");
  choose(page, "force-target", "role");
  type(page, "force-role", "web");
  click(page, "force-prepare");
  return text(globalThis.t.latest());
}"#,
    );
    assert!(
        counting
            .as_str()
            .expect("text")
            .contains("Counting the hosts..."),
        "{counting}"
    );
    let cancelled = ui.run(&later(
        r#"
await click(t.latest(), "force-cancel");
const released = gatewayRelease("hostkeeper.operator.v1.Hosts/List");
return { released, counting: text(t.latest()).includes("Counting"), dialog: has("force-send"), reports: t.seen.reports };"#,
    ));
    let after_release = ui.run(&later(
        r#"return { dialog: has("force-send"), shown: t.seen.shown.length };"#,
    ));
    assert_eq!(
        cancelled,
        json!({"released": true, "counting": false, "dialog": false, "reports": ["Force cancelled: nothing was sent."]})
    );
    assert_eq!(
        after_release["dialog"], false,
        "a cancelled count brings no dialog back"
    );
    let recount = ui.run(&later(
        r#"
const shown = t.seen.shown.length;
click(t.latest(), "force-prepare");
click(t.latest(), "force-prepare");
gatewayRelease("hostkeeper.operator.v1.Hosts/List");
return shown;"#,
    ));
    let stale = ui.run(&later(
        r#"return { counting: text(t.latest()).includes("Counting"), dialog: has("force-send") };"#,
    ));
    assert_eq!(
        stale,
        json!({"counting": true, "dialog": false}),
        "the older count is ignored"
    );
    assert!(recount.as_u64().is_some());
    let counted = ui.run(&later(
        r#"return gatewayRelease("hostkeeper.operator.v1.Hosts/List");"#,
    ));
    assert_eq!(counted, true);
    let sending = ui.run(&later(
        r#"
type(t.latest(), "force-count", "1");
const send = named(t.latest(), "force-send")[0];
send.on.click();
const drawn = named(t.latest(), "force-send")[0];
await click(t.latest(), "force-prepare");
return {
  text: text(t.latest()),
  cancel: has("force-cancel"),
  disabled: drawn.attrs.disabled === true,
  handlers: Object.keys(drawn.on).length,
  reports: t.seen.reports,
};"#,
    ));
    assert_eq!(sending["cancel"], false);
    assert_eq!(sending["disabled"], true);
    assert_eq!(sending["handlers"], 0);
    let text = sending["text"].as_str().expect("text");
    assert!(
        text.contains("Sending the force of api to 1.3.0 on 1 hosts. It cannot be cancelled."),
        "{text}"
    );
    assert_eq!(
        sending["reports"],
        json!([
            "Force cancelled: nothing was sent.",
            "A force is being sent: wait for its answer."
        ])
    );
    let answered = ui.run(&later(
        r#"
gatewayRelease("hostkeeper.operator.v1.Releases/Force");
return null;"#,
    ));
    assert_eq!(answered, Value::Null);
    let done = ui.run(&later(
        r#"return { reports: t.seen.reports, dialog: has("force-send") };"#,
    ));
    assert_eq!(
        done,
        json!({"reports": ["Force cancelled: nothing was sent.", "A force is being sent: wait for its answer.",
                           "Force sent: 1 hosts ordered."], "dialog": false})
    );
    assert_eq!(force_bodies(&ui).len(), 1);
    assert_eq!(
        ui.methods()
            .iter()
            .filter(|x| **x == m("Hosts/List"))
            .count(),
        3
    );
}
