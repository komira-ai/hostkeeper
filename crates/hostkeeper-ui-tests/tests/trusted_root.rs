//! The Host page's trusted root: `rootState` and `rootAge`, the pure functions from what
//! Hosts.Get answers of the host's Sigstore trusted root to what the page says, at fixed inputs;
//! and the page's trusted-root section, with its warning line when the reason needs attention.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

/// `rootState(reason)` and `rootAge(secs)` for each input, in order.
fn said(reasons: &str, ages: &str) -> Value {
    let mut ui = Ui::start();
    ui.run(&format!(
        r#"
import {{ rootState, rootAge }} from "./pages/host.js";
export async function main() {{
  return {{ states: {reasons}.map(rootState), ages: {ages}.map(rootAge) }};
}}"#
    ))
}

/// Catches a reason shown as another, an unknown reason (absent, unspecified, or a value the page
/// does not know) shown as one that needs no attention, and an age that is absent or no number
/// shown as a number: each reason is its own state, `NONE` is `ok`, the rest are `unknown`; an
/// age is whole days and its seconds, as Hosts.Get answers it (a string) or as a number, and an
/// absent, empty, negative or non-numeric one is `not reported`.
#[test]
fn root_state_and_age_say_each_reason_and_unknown_for_anything_else() {
    let out = said(
        r#"["TRUSTED_ROOT_REASON_STALE", "TRUSTED_ROOT_REASON_MISSING",
            "TRUSTED_ROOT_REASON_UNPARSEABLE", "TRUSTED_ROOT_REASON_NONE",
            "TRUSTED_ROOT_REASON_UNSPECIFIED", undefined, "TRUSTED_ROOT_REASON_LATER", 2]"#,
        r#"["7862400", 86399, "0", undefined, null, "", "-1", "old"]"#,
    );
    assert_eq!(
        out["states"],
        json!([
            "stale",
            "missing",
            "unparseable",
            "ok",
            "unknown",
            "unknown",
            "unknown",
            "unknown"
        ])
    );
    assert_eq!(
        out["ages"],
        json!([
            "91 days (7862400 s)",
            "0 days (86399 s)",
            "0 days (0 s)",
            "not reported",
            "not reported",
            "not reported",
            "not reported",
            "not reported"
        ])
    );
}

/// Renders the Host page of `web-2`, whose Hosts.Get answers `facts` as part of the host; returns
/// the trusted-root section's warnings and rows.
fn host_page(facts: Value) -> Value {
    let mut ui = Ui::start();
    let mut host = json!({"nodeId": "web-2", "role": "web", "live": true});
    if let (Some(host), Some(facts)) = (host.as_object_mut(), facts.as_object()) {
        host.extend(facts.clone());
    }
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.run(
        r#"
import { api, recorder, text, nodes, rows } from "../test/kit.js";
import { render } from "./pages/host.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), { nodeId: "web-2" }, [], 1800000000);
  const tree = await render(ctx);
  const section = nodes(tree).find((node) => node.attrs["class"] === "trusted-root");
  return {
    warnings: nodes(section).filter((node) => node.attrs["role"] === "alert").map(text),
    rows: rows(section),
  };
}"#,
    )
}

fn rows(state: &str, age: &str) -> Value {
    json!([["", ""], ["State", state], ["Age", age]])
}

/// Catches a root that needs attention shown without its warning, a warning of another reason,
/// and a warning for a root that needs none or is unknown: a stale root warns that checks go on
/// and refuse packages signed after a rotation, a missing or unparseable one that every check
/// fails, each with its state and age; a root that needs no attention and a host whose heartbeat
/// said nothing of its root show no warning, the latter `not reported` twice.
#[test]
fn the_host_page_warns_while_the_trusted_root_needs_attention() {
    let stale = host_page(json!({
        "trustedRootAgeSecs": "7862400",
        "trustedRootReason": "TRUSTED_ROOT_REASON_STALE",
    }));
    assert_eq!(
        stale["warnings"],
        json!([
            "The Sigstore trusted root is stale: package checks go on with it, and refuse every \
             package signed after a key rotation it does not hold. Update hostkeeper on the \
             host, or copy a current root to it."
        ])
    );
    assert_eq!(stale["rows"], rows("stale", "91 days (7862400 s)"));

    let missing = host_page(json!({"trustedRootReason": "TRUSTED_ROOT_REASON_MISSING"}));
    assert_eq!(
        missing["warnings"],
        json!(["The host has no Sigstore trusted root: every package check on it fails."])
    );
    assert_eq!(missing["rows"], rows("missing", "not reported"));

    let unparseable = host_page(json!({
        "trustedRootAgeSecs": "60",
        "trustedRootReason": "TRUSTED_ROOT_REASON_UNPARSEABLE",
    }));
    assert_eq!(
        unparseable["warnings"],
        json!([
            "The host's Sigstore trusted root cannot be read: every package check on it fails."
        ])
    );
    assert_eq!(unparseable["rows"], rows("unparseable", "0 days (60 s)"));

    let fine = host_page(json!({
        "trustedRootAgeSecs": "3600",
        "trustedRootReason": "TRUSTED_ROOT_REASON_NONE",
    }));
    assert_eq!(fine["warnings"], json!([]));
    assert_eq!(fine["rows"], rows("needs no attention", "0 days (3600 s)"));

    let unknown = host_page(json!({}));
    assert_eq!(unknown["warnings"], json!([]));
    assert_eq!(unknown["rows"], rows("not reported", "not reported"));
}
