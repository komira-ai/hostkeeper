//! The Host page's serial console baseline: `baselineRows` and `baselineNotice`, the pure
//! functions from a host's last heartbeat to the Overview row and the warning, at fixed inputs;
//! and the Overview as rendered, with its row and its reboot-pending notice.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

/// Proves: the row says each state in its own words (port, then state, with the step in
/// parentheses for failed and unsupported), says the raw state for an unknown one, and is absent
/// when the heartbeat carries no report or no heartbeat is held.
#[test]
fn baseline_rows_say_each_state_and_nothing_without_a_report() {
    let mut ui = Ui::start();
    let out: Value = ui.run(
        r#"
import { baselineRows } from "./serialbaseline.js";
const report = (state, detail) => ({ heartbeat: { serialConsole: { port: "ttyS1", state, detail } } });
export async function main() {
  return [
    baselineRows(report("applied", "")),
    baselineRows(report("reboot_pending", "")),
    baselineRows(report("failed", "update_grub")),
    baselineRows(report("failed", "")),
    baselineRows(report("unsupported", "service_manager")),
    baselineRows(report("later", "")),
    baselineRows({ heartbeat: {} }),
    baselineRows({}),
  ];
}"#,
    );
    assert_eq!(
        out,
        json!([
            [["Serial console", "ttyS1, applied"]],
            [["Serial console", "ttyS1, reboot pending"]],
            [["Serial console", "ttyS1, failed (update_grub)"]],
            [["Serial console", "ttyS1, failed"]],
            [["Serial console", "ttyS1, unsupported (service_manager)"]],
            [["Serial console", "ttyS1, later"]],
            [],
            []
        ])
    );
}

/// Proves: the notice is one alert line for reboot_pending and for failed (with the step, or an
/// unknown step when none is named), and nothing for applied, unsupported, an unknown state or no
/// report.
#[test]
fn baseline_notice_warns_only_for_reboot_pending_and_failed() {
    let mut ui = Ui::start();
    let out: Value = ui.run(
        r#"
import { text } from "../test/kit.js";
import { baselineNotice } from "./serialbaseline.js";
const report = (state, detail) => ({ heartbeat: { serialConsole: { port: "ttyS1", state, detail } } });
const lines = (host) => baselineNotice(host).map((node) => [node.attrs["role"], text(node)]);
export async function main() {
  return [
    lines(report("reboot_pending", "")),
    lines(report("failed", "getty")),
    lines(report("failed", "")),
    lines(report("applied", "")),
    lines(report("unsupported", "service_manager")),
    lines(report("later", "")),
    lines({}),
  ];
}"#,
    );
    let reboot = "The serial console is set up on ttyS1; the kernel's console moves to it at the \
                  host's next reboot. hostkeeper does not reboot the host.";
    assert_eq!(
        out,
        json!([
            [["alert", reboot]],
            [[
                "alert",
                "The serial console baseline on ttyS1 failed at getty."
            ]],
            [[
                "alert",
                "The serial console baseline on ttyS1 failed at an unknown step."
            ]],
            [],
            [],
            [],
            []
        ])
    );
}

/// Renders the Host page of a host whose heartbeat is `heartbeat`; returns the page's "Serial
/// console" rows and the alert lines that speak of the serial console.
fn host_page(heartbeat: Value) -> Value {
    let mut ui = Ui::start();
    let host = json!({"nodeId": "lin-1", "role": "build", "live": true, "heartbeat": heartbeat});
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.run(
        r#"
import { api, recorder, text, nodes, rows } from "../test/kit.js";
import { render } from "./pages/host.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), { nodeId: "lin-1" }, [], 1800000000);
  const tree = await render(ctx);
  const warnings = nodes(tree)
    .filter((node) => node.attrs["role"] === "alert" && /serial console/i.test(text(node)))
    .map(text);
  return { rows: rows(tree).filter(([name]) => name === "Serial console"), warnings };
}"#,
    )
}

/// Proves: the Overview shows the "Serial console" row for a reported baseline, with the
/// reboot-pending notice only for reboot_pending, and shows neither for a heartbeat without a
/// report.
#[test]
fn the_host_page_shows_the_serial_console_row_and_the_reboot_pending_notice() {
    let applied = host_page(json!({"serialConsole": {"port": "ttyS1", "state": "applied"}}));
    assert_eq!(
        applied["rows"],
        json!([["Serial console", "ttyS1, applied"]])
    );
    assert_eq!(applied["warnings"], json!([]));

    let pending = host_page(json!({"serialConsole": {"port": "ttyS1", "state": "reboot_pending"}}));
    assert_eq!(
        pending["rows"],
        json!([["Serial console", "ttyS1, reboot pending"]])
    );
    assert_eq!(
        pending["warnings"],
        json!([
            "The serial console is set up on ttyS1; the kernel's console moves to it at the host's \
             next reboot. hostkeeper does not reboot the host."
        ])
    );

    let silent = host_page(json!({"platform": "PLATFORM_LINUX_X86_64"}));
    assert_eq!(silent["rows"], json!([]));
    assert_eq!(silent["warnings"], json!([]));
}
