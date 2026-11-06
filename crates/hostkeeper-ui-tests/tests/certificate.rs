//! The Host page's certificate: `certState`, the pure function from a certificate's end and the
//! time to its state, at fixed inputs; and what the page shows of the end, the last renewal error
//! and the count of heartbeats with the clock off, at the time its context reads.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

/// The time the drivers of this file give the page, in unix seconds.
const NOW: u64 = 1_800_000_000;

const HOUR: u64 = 3600;

/// `certState(notAfter, now)` for each pair, in order.
fn states(pairs: &str) -> Value {
    let mut ui = Ui::start();
    ui.run(&format!(
        r#"
import {{ certState, CERT_WARN_SECS }} from "./pages/host.js";
export async function main() {{
  return {{ warn: CERT_WARN_SECS, states: {pairs}.map(([end, now]) => certState(end, now)) }};
}}"#
    ))
}

/// Catches a warning at the wrong side of 48 hours, an expired certificate shown as near, a
/// certificate that ends this very second shown as expired, and an end that is absent or no
/// number shown as a state: 47 hours left is `soon`, 49 hours and exactly 48 are `ok`, one second
/// past the end is `expired`, the end itself is `soon`, and an absent, empty, zero or non-numeric
/// end is `unknown`. Ends come as Hosts.Get answers them, strings, and as numbers.
#[test]
fn cert_state_is_soon_under_48_hours_ok_past_them_and_expired_after_the_end() {
    let pairs = format!(
        r#"[["{h47}", {NOW}], ["{h49}", {NOW}], ["{past}", {NOW}], [{h48}, {NOW}], ["{NOW}", {NOW}],
            [undefined, {NOW}], ["", {NOW}], ["0", {NOW}], ["soon", {NOW}], [null, {NOW}]]"#,
        h47 = NOW + 47 * HOUR,
        h49 = NOW + 49 * HOUR,
        past = NOW - 1,
        h48 = NOW + 48 * HOUR,
    );
    let out = states(&pairs);
    assert_eq!(out["warn"], json!(48 * HOUR));
    assert_eq!(
        out["states"],
        json!([
            "soon", "ok", "expired", "ok", "soon", "unknown", "unknown", "unknown", "unknown",
            "unknown"
        ])
    );
}

/// Renders the Host page of `web-2`, whose Hosts.Get answers `facts` as part of the host, with
/// the context's clock at `now`; returns the page's text, its warnings and its certificate rows.
fn host_page(facts: Value, now: u64) -> Value {
    let mut ui = Ui::start();
    let mut host = json!({"nodeId": "web-2", "role": "web", "live": true});
    if let (Some(host), Some(facts)) = (host.as_object_mut(), facts.as_object()) {
        host.extend(facts.clone());
    }
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.run(&format!(
        r#"
import {{ api, recorder, text, nodes, rows }} from "../test/kit.js";
import {{ render }} from "./pages/host.js";
export async function main() {{
  const {{ ctx }} = recorder(api("operator-token"), {{ nodeId: "web-2" }}, [], {now});
  const tree = await render(ctx);
  const section = nodes(tree).find((node) => node.attrs["class"] === "certificate");
  return {{
    warnings: nodes(section).filter((node) => node.attrs["role"] === "alert").map(text),
    rows: rows(section),
  }};
}}"#
    ))
}

fn rows(ends: &str, error: &str, skewed: &str) -> Value {
    json!([
        ["", ""],
        ["Ends", ends],
        ["Last renewal error", error],
        ["Heartbeats with the clock off", skewed]
    ])
}

/// Catches a certificate near its end, or a renewal error, shown without a warning, a warning
/// read from another clock than the context's, or a skew count left out: 47 hours left at the
/// context's time warns with the end, an error warns with its text, and the count is shown; the
/// same end 49 hours away with no error shows no warning.
#[test]
fn the_host_page_warns_of_an_end_under_48_hours_and_of_a_renewal_error() {
    let end = NOW + 47 * HOUR;
    let facts = json!({
        "certNotAfterUnix": end.to_string(),
        "lastRenewError": "renewal refused: host_excluded",
        "clockSkewBeats": "3",
    });
    let near = host_page(facts.clone(), NOW);
    assert_eq!(
        near["warnings"],
        json!([
            "The certificate ends in less than 48 hours, at 2027-01-17 07:00:00 UTC.",
            "The last renewal failed: renewal refused: host_excluded",
        ])
    );
    assert_eq!(
        near["rows"],
        rows(
            "2027-01-17 07:00:00 UTC",
            "renewal refused: host_excluded",
            "3"
        )
    );

    let far = host_page(
        json!({"certNotAfterUnix": end.to_string(), "clockSkewBeats": "3"}),
        NOW - 2 * HOUR,
    );
    assert_eq!(far["warnings"], json!([]));
    assert_eq!(far["rows"], rows("2027-01-17 07:00:00 UTC", "-", "3"));
}

/// Catches an expired certificate shown as near or as fine, and a host that reported nothing
/// shown with a made-up end or warning: past its end the page says it expired; a host whose
/// heartbeat said no end, no error and no skew shows `not reported`, `-`, `0` and no warning.
#[test]
fn the_host_page_says_an_expired_certificate_expired_and_a_silent_host_reported_nothing() {
    let expired = host_page(json!({"certNotAfterUnix": (NOW - HOUR).to_string()}), NOW);
    assert_eq!(
        expired["warnings"],
        json!(["The certificate expired at 2027-01-15 07:00:00 UTC."])
    );
    assert_eq!(expired["rows"], rows("2027-01-15 07:00:00 UTC", "-", "0"));

    let silent = host_page(json!({}), NOW);
    assert_eq!(silent["warnings"], json!([]));
    assert_eq!(silent["rows"], rows("not reported", "-", "0"));
}
