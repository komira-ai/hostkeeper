//! The Host page's Full Disk Access row: `accessState`, the pure function from what Hosts.Get
//! answers of a Mac's grant to what the page says, at fixed inputs; and the Overview as rendered,
//! with its row and its warning line while the grant is not in force.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

/// Proves: each state of the grant is its own words, and anything the page does not know (absent,
/// unspecified, a value a later coordinator sends) reads as not reported.
///
/// Catches: a state shown as another, and an unknown value shown as granted.
#[test]
fn access_state_says_each_state_and_not_reported_for_anything_else() {
    let mut ui = Ui::start();
    let out: Value = ui.run(
        r#"
import { accessState } from "./fulldisk.js";
export async function main() {
  return ["FULL_DISK_ACCESS_GRANTED", "FULL_DISK_ACCESS_NOT_GRANTED",
    "FULL_DISK_ACCESS_UNDETERMINED", "FULL_DISK_ACCESS_NOT_APPLICABLE",
    "FULL_DISK_ACCESS_UNSPECIFIED", undefined, "FULL_DISK_ACCESS_LATER", 2].map(accessState);
}"#,
    );
    assert_eq!(
        out,
        json!([
            "granted",
            "not granted",
            "not determined",
            "not a Mac",
            "not reported",
            "not reported",
            "not reported",
            "not reported"
        ])
    );
}

/// Renders the Host page of `node`, whose Hosts.Get answers `facts` as part of the host; returns
/// the page's Full Disk Access rows and the alert lines that speak of it.
fn host_page(facts: Value) -> Value {
    let mut ui = Ui::start();
    let mut host = json!({"nodeId": "mac-1", "role": "build", "live": true});
    if let (Some(host), Some(facts)) = (host.as_object_mut(), facts.as_object()) {
        host.extend(facts.clone());
    }
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.run(
        r#"
import { api, recorder, text, nodes, rows } from "../test/kit.js";
import { render } from "./pages/host.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), { nodeId: "mac-1" }, [], 1800000000);
  const tree = await render(ctx);
  const warnings = nodes(tree)
    .filter((node) => node.attrs["role"] === "alert" && /Full Disk Access/.test(text(node)))
    .map(text);
  return { rows: rows(tree).filter(([name]) => name === "Full Disk Access"), warnings };
}"#,
    )
}

fn heartbeat(platform: &str) -> Value {
    json!({"platform": platform})
}

/// Proves: a Mac that reports a state shows it in a "Full Disk Access" row, with a warning line
/// naming the privacy profile's document only while the grant is not granted; a Mac that reports
/// none shows "not reported"; a host that is not a Mac, and a Linux host that reports none, show
/// no row and no warning.
///
/// Catches: a grant that is not in force shown without its warning, a warning for a grant that is
/// in force or undetermined, a Linux host shown a row about a grant it cannot have, and a Mac of
/// an older daemon whose silence is not shown.
#[test]
fn the_host_page_shows_the_grant_and_warns_only_while_it_is_not_in_force() {
    let mac = heartbeat("PLATFORM_DARWIN_ARM64");
    let row = |state: &str| json!([["Full Disk Access", state]]);

    let granted =
        host_page(json!({"fullDiskAccess": "FULL_DISK_ACCESS_GRANTED", "heartbeat": mac}));
    assert_eq!(granted["rows"], row("granted"));
    assert_eq!(granted["warnings"], json!([]));

    let denied =
        host_page(json!({"fullDiskAccess": "FULL_DISK_ACCESS_NOT_GRANTED", "heartbeat": mac}));
    assert_eq!(denied["rows"], row("not granted"));
    assert_eq!(
        denied["warnings"],
        json!(["Full Disk Access is not granted: the privacy profile of \
             docs/operations/mac-full-disk-access.md is not in force on this Mac. Anything that \
             needs the grant fails until it is."])
    );

    let unsure =
        host_page(json!({"fullDiskAccess": "FULL_DISK_ACCESS_UNDETERMINED", "heartbeat": mac}));
    assert_eq!(unsure["rows"], row("not determined"));
    assert_eq!(unsure["warnings"], json!([]));

    let silent = host_page(json!({"heartbeat": mac}));
    assert_eq!(silent["rows"], row("not reported"));
    assert_eq!(silent["warnings"], json!([]));

    let linux = host_page(json!({
        "fullDiskAccess": "FULL_DISK_ACCESS_NOT_APPLICABLE",
        "heartbeat": heartbeat("PLATFORM_LINUX_X86_64"),
    }));
    assert_eq!(linux["rows"], json!([]));
    assert_eq!(linux["warnings"], json!([]));

    let old_linux = host_page(json!({"heartbeat": heartbeat("PLATFORM_LINUX_X86_64")}));
    assert_eq!(old_linux["rows"], json!([]));
    assert_eq!(host_page(json!({}))["rows"], json!([]));
}
