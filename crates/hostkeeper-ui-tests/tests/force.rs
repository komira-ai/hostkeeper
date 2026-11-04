//! The Rollout page's force: the dialog that names the hosts it reaches, the typed count it
//! needs, and what happens when the hosts cannot be counted, run in the JavaScript engine against
//! the fake gateway of `support`.

mod support;

use serde_json::json;
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
await Promise.all([click(latest(), "force-send"), click(latest(), "force-send")]);
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
