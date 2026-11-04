//! What the pages show, row by row; what they make of a reply from a newer coordinator, of a
//! failed second call and of times a JavaScript date cannot hold; and the canonical-JSON check the
//! fake gateway holds every request and reply to.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{hosts_reply, m, render, shown, status_reply};

/// Renders `page` with `params` and returns its table rows (each cell's text, white space run
/// together).
fn rows(ui: &mut Ui, page: &str, params: &str) -> Value {
    ui.run(&format!(
        r#"
import {{ api, recorder, rows }} from "../test/kit.js";
import {{ render }} from "./pages/{page}.js";
export async function main() {{
  const {{ ctx }} = recorder(api("operator-token"), {params}, []);
  return rows(await render(ctx));
}}"#
    ))
}

const AT: &str = "2027-01-15 08:00:00 UTC";

#[test]
fn the_fleet_table_shows_each_host_in_its_own_row() {
    // Catches: a field that disappears from one host's row while a neighbour's still matches a
    // substring (healthy within unhealthy), and a mark shown twice or on the wrong host.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Status"), status_reply());
    assert_eq!(
        rows(&mut ui, "fleet", "{}"),
        json!([
            ["Host", "Role", "Heartbeat", "Last heartbeat", "Services"],
            [
                "web-1",
                "web",
                "live",
                AT,
                "api 1.4.0, healthy [holds the update slot]"
            ],
            ["web-2", "web", "live", "-", "api 1.3.0, unhealthy [held]"],
            [
                "new-1",
                "web",
                "silent",
                "-",
                "api not installed, - [waiting_for_operator]"
            ],
            [
                "clone-1",
                "db",
                "silent",
                "-",
                "api 1.4.0, - [excluded, duplicate]"
            ],
        ])
    );
}

#[test]
fn the_host_page_shows_each_fact_beside_its_label() {
    // Catches: a fact of the host, of a service or of the last update shown under another label,
    // or left out, which a search of the whole page's text would not notice.
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
                 "lastUpdate": {"target": "1.4.0", "outcome": "OUTCOME_FAILED", "forced": true,
                                "phase": "FAILURE_PHASE_AFTER_COMMIT", "class": "FAILURE_CLASS_RELEASE",
                                "reason": "health_check", "orderTry": "2", "orderId": "order-17",
                                "detail": "probe returned 503", "startedUnix": "1800000000"}},
                {"service": "metrics", "up": true, "membership": "MEMBERSHIP_EXCLUDED", "excludedReason": "noisy",
                 "installedVersion": "2.0.0", "health": "HEALTH_HEALTHY"},
            ],
        }}),
    );
    let all = rows(&mut ui, "host", r#"{ nodeId: "web-2" }"#);
    let pairs: Vec<&Value> = all
        .as_array()
        .expect("rows")
        .iter()
        .filter(|r| r.as_array().is_some_and(|c| c.len() == 2))
        .collect();
    assert_eq!(
        json!(pairs),
        json!([
            ["", ""],
            ["Role", "web"],
            ["Heartbeat", "live"],
            ["Last heartbeat", AT],
            ["Agent", "hostkeeper 0.4.1"],
            ["Labels", "rack=r1"],
            ["Excluded from", "metrics (noisy)"],
            ["", ""],
            ["Installed version", "1.3.0"],
            ["Health", "unhealthy"],
            ["Up", "no"],
            ["Update slot", "holds the update slot"],
            ["Gate", "held"],
            ["Membership", "present"],
            ["Excluded", "no"],
            ["Last update", ""],
            ["Target", "1.4.0"],
            ["Verdict", "failed (forced)"],
            ["Phase", "after_commit"],
            ["Class", "release"],
            ["Reason", "health_check"],
            ["Detail", "probe returned 503"],
            ["Try", "2"],
            ["Order", "order-17"],
            ["Started", AT],
            ["", ""],
            ["Installed version", "2.0.0"],
            ["Health", "healthy"],
            ["Up", "yes"],
            ["Update slot", "-"],
            ["Gate", "-"],
            ["Membership", "excluded"],
            ["Excluded", "yes: noisy"],
        ])
    );
}

#[test]
fn the_enrollment_page_shows_every_claim_and_fact_beside_its_label() {
    // Catches: a claim (the UUIDs included), the image, the first-seen time or the renewal left
    // out or shown under another label.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [
            {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "image": "debian-hk",
             "firstSeenUnix": "1800000000", "renewalOf": "web-3", "macNoncePresented": true,
             "claims": {"machineId": "m-0001", "dmiSerial": "SN-42", "productUuid": "4c4c4544-0042",
                        "addresses": ["192.0.2.10", "192.0.2.11"], "macSerial": "C02-X",
                        "platformUuid": "a1b2c3d4-0042"}},
            {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32)},
        ]}),
    );
    assert_eq!(
        rows(&mut ui, "enrollment", "{}"),
        json!([
            ["", ""],
            ["Key SHA-256", "ab".repeat(32)],
            ["Image", "debian-hk"],
            ["First seen", AT],
            ["Renewal of", "web-3"],
            ["Mac nonce presented", "yes"],
            ["Claim", ""],
            ["Machine id", "m-0001"],
            ["DMI serial", "SN-42"],
            ["Product UUID", "4c4c4544-0042"],
            ["Addresses", "192.0.2.10, 192.0.2.11"],
            ["Mac serial", "C02-X"],
            ["Platform UUID", "a1b2c3d4-0042"],
            ["", ""],
            ["Key SHA-256", "cd".repeat(32)],
            ["Image", "-"],
            ["First seen", "-"],
            ["Renewal of", "-"],
            ["Mac nonce presented", "no"],
        ])
    );
}

#[test]
fn the_fleet_page_shows_a_failed_status_call_and_a_halt_without_marks() {
    // Catches: a Fleet page that hides the failure of its second call, and a halt alert that
    // needs the halt's marks when the state alone says the service is halted.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.refuse(&m("Releases/Status"), 500, "INTERNAL", "store_failed");
    let out = render(&mut ui, "fleet", "{}");
    assert_eq!(shown(&out), "Fleet INTERNAL store_failed");
    assert_eq!(ui.methods(), [m("Hosts/List"), m("Releases/Status")]);

    let mut ui = Ui::start();
    let alerts = ui.run(
        r#"
import { fleetAlerts } from "./pages/fleet.js";
export async function main() {
  return fleetAlerts([], { services: [{ service: "api", convergence: { state: "CONVERGENCE_STATE_HALTED" } }] });
}"#,
    );
    assert_eq!(alerts, json!([{"kind": "halt", "text": "api is halted"}]));
}

#[test]
fn a_reply_from_a_newer_coordinator_still_renders() {
    // Catches: a page that fails, or calls again, on a field it does not know or an enum value
    // newer than it (a number, or a name it has no label for).
    let mut ui = Ui::start();
    ui.reply_raw(
        &m("Hosts/List"),
        200,
        &json!({"hosts": [
            {"nodeId": "web-1", "role": "web", "live": true, "future": {"x": 1},
             "services": [{"service": "api", "installedVersion": "1.4.0", "health": 7, "gateReason": 42,
                           "membership": "MEMBERSHIP_SOMETHING_NEW", "newField": [true]}]}],
            "cursor": "next"})
        .to_string(),
    );
    ui.reply_raw(
        &m("Releases/Status"),
        200,
        &json!({"services": [{"service": "api", "marks": {"bad": [], "later": 1}, "convergence": {"state": 9}}],
                "extra": [1]})
        .to_string(),
    );
    let fleet = rows(&mut ui, "fleet", "{}");
    assert_eq!(
        fleet,
        json!([
            ["Host", "Role", "Heartbeat", "Last heartbeat", "Services"],
            ["web-1", "web", "live", "-", "api 1.4.0, -"],
        ])
    );
    assert_eq!(ui.methods(), [m("Hosts/List"), m("Releases/Status")]);
    let labels = ui.run(
        r#"
import { enumLabel } from "./view.js";
export async function main() {
  return [enumLabel(7, "HEALTH_"), enumLabel("MEMBERSHIP_SOMETHING_NEW", "MEMBERSHIP_"),
          enumLabel("HEALTH_UNSPECIFIED", "HEALTH_"), enumLabel(undefined, "HEALTH_")];
}"#,
    );
    assert_eq!(labels, json!(["-", "something_new", "-", "-"]));
}

#[test]
fn a_time_a_javascript_date_cannot_hold_is_shown_as_its_number() {
    // Catches: a page that fails on a valid uint64 time beyond what a JavaScript date holds, and
    // each boundary of the times shown.
    let mut ui = Ui::start();
    let shown = ui.run(
        r#"
import { fmtUnix } from "./view.js";
export async function main() {
  return [undefined, "", "0", "-5", "abc", "NaN", "1800000000", "8640000000000", "8640000000001",
          "18446744073709551615"].map(fmtUnix);
}"#,
    );
    assert_eq!(
        shown,
        json!([
            "-",
            "-",
            "-",
            "-",
            "-",
            "-",
            AT,
            "+275760-09-13 00:00:00 UTC",
            "8640000000001 (out of range)",
            "18446744073709551615 (out of range)",
        ])
    );
}

#[test]
fn the_canonical_json_check_refuses_aliases_and_numeric_64_bit_integers() {
    // Catches: a check that only asks whether ProtoJSON reads the value, which accepts a field's
    // proto name (`node_id`), a 64-bit integer as a number and an enum as a number.
    let ui = Ui::start();
    let request = |method: &str, value: Value| ui.shape(&m(method), false, &value);
    let reply = |method: &str, value: Value| ui.shape(&m(method), true, &value);
    assert_eq!(request("Hosts/Get", json!({"nodeId": "web-1"})), Ok(()));
    assert_eq!(
        request("Audit/List", json!({"sinceSeq": "5", "limit": 100})),
        Ok(())
    );
    assert_eq!(
        reply(
            "Audit/List",
            json!({"entries": [{"seq": "1", "by": "ACTOR_CI"}], "nextSeq": "1"})
        ),
        Ok(())
    );
    assert_eq!(
        request(
            "Hosts/SetLabels",
            json!({"nodeId": "a", "labels": {"k": "v"}})
        ),
        Ok(())
    );
    assert_eq!(
        request("Releases/Force", json!({"service": "api", "fleet": {}})),
        Ok(())
    );
    let refused = [
        request("Hosts/Get", json!({"node_id": "web-1"})),
        request("Audit/List", json!({"sinceSeq": 5})),
        request("Audit/List", json!({"sinceSeq": "-5"})),
        request("Audit/List", json!({"sinceSeq": "5x"})),
        reply("Audit/List", json!({"entries": [{"seq": 1}]})),
        reply("Audit/List", json!({"entries": [{"by": 1}]})),
        reply("Hosts/List", json!({"hosts": [{"node_id": "a"}]})),
        reply("Hosts/List", json!({"hosts": {"nodeId": "a"}})),
        reply("Hosts/Get", json!({"host": 5})),
        request(
            "Hosts/SetLabels",
            json!({"nodeId": "a", "labels": {"k": 1}}),
        ),
        request("Hosts/SetLabels", json!({"nodeId": "a", "labels": ["k"]})),
        request("Enrollment/List", json!({"limit": "5"})),
        request("Hosts/Exclude", json!({"abandon": "true"})),
        request("Hosts/Get", json!({"nodeId": null})),
        request("Hosts/Get", json!({"nodeId": "a", "extra": 1})),
        request("Hosts/Get", json!([])),
    ];
    for (i, outcome) in refused.iter().enumerate() {
        assert!(outcome.is_err(), "case {i} was accepted");
    }
    assert!(
        request("Hosts/Get", json!({"node_id": "web-1"}))
            .expect_err("an alias")
            .contains(
                "node_id is not the JSON name of a field of hostkeeper.operator.v1.GetHostRequest"
            ),
    );
    assert!(
        ui.shape("hostkeeper.operator.v1.Hosts/Teleport", false, &json!({}))
            .is_err()
    );
}
