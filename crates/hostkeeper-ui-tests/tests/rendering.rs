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
fn the_hosts_table_shows_each_host_in_its_own_row() {
    // Catches: a field that disappears from one host's row while a neighbour's still matches a
    // substring (healthy within unhealthy), and a mark shown twice or on the wrong host.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Status"), status_reply());
    assert_eq!(
        rows(&mut ui, "home", "{}"),
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

/// The reply to `Hosts.List` the coordinator gives for one enrolled host that heartbeated, with
/// a label and a hold. The coordinator's test of `Hosts.List` end to end (in
/// `src/grpc/tests/hosts.rs`) fails when its answer and this file part.
const HOSTS_LIST: &str = include_str!("fixtures/hosts_list.json");

fn coordinator_hosts() -> Value {
    serde_json::from_str(HOSTS_LIST).expect("the fixture is JSON")
}

#[test]
fn the_hosts_table_shows_a_host_of_the_coordinators_own_reply() {
    // Catches: a Home page that renders only the replies written for it and not the one the
    // coordinator gives (a field name, a 64-bit time as a string, an enum's spelling).
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), coordinator_hosts());
    ui.reply(&m("Releases/Status"), json!({}));
    assert_eq!(
        rows(&mut ui, "home", "{}"),
        json!([
            ["Host", "Role", "Heartbeat", "Last heartbeat", "Services"],
            [
                "s1",
                "storage",
                "live",
                AT,
                "kbf 1.0, healthy [held, no_desired]"
            ],
        ])
    );
}

#[test]
fn the_host_page_shows_a_host_of_the_coordinators_own_reply() {
    // Catches: a Host page that cannot read the host `Hosts.Get` answers, which is the host
    // `Hosts.List` lists.
    let mut ui = Ui::start();
    let host = coordinator_hosts()["hosts"][0].clone();
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.reply(&m("Services/List"), json!({}));
    let all = rows(&mut ui, "host", r#"{ nodeId: "s1" }"#);
    let pairs: Vec<&Value> = all
        .as_array()
        .expect("rows")
        .iter()
        .filter(|r| r.as_array().is_some_and(|c| c.len() == 2))
        .collect();
    assert_eq!(
        json!(pairs),
        json!([
            ["Role", "storage"],
            ["Heartbeat", "live"],
            ["Last heartbeat", AT],
            ["Agent", "hostkeeper-daemon 1.0"],
            ["Labels", "rack=r1"],
            ["Excluded from", "-"],
            ["Inventory record", "none matched"],
            ["Data MACs", "-"],
            ["BMC MAC", "-"],
            ["Serial", "-"],
            ["Enrolled", "not enrolled"],
            ["First heartbeat", AT],
            ["Installed version", "1.0"],
            ["Health", "healthy"],
            ["Up", "yes"],
            ["Update slot", "-"],
            ["Gate", "no desired"],
            ["Membership", "present"],
            ["Excluded", "no"],
            ["Ends", "not reported"],
            ["Last renewal error", "-"],
            ["Heartbeats with the clock off", "0"],
            ["State", "not reported"],
            ["Age", "not reported"],
        ])
    );
}

#[test]
fn the_hosts_table_shows_every_page_of_the_host_list() {
    // Catches: a Home page that shows the first page of a fleet larger than one page (256
    // hosts) and leaves the rest out, or that asks for the next page with another token.
    let mut ui = Ui::start();
    let one = |id: &str| json!({"nodeId": id, "role": "web", "live": true});
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [one("web-1"), one("web-2")], "nextPageToken": "web-2"}),
    );
    ui.reply(&m("Hosts/List"), json!({"hosts": [one("web-3")]}));
    ui.reply(&m("Releases/Status"), json!({}));
    let shown = rows(&mut ui, "home", "{}");
    let hosts: Vec<&Value> = shown
        .as_array()
        .expect("rows")
        .iter()
        .skip(1)
        .map(|row| &row[0])
        .collect();
    assert_eq!(json!(hosts), json!(["web-1", "web-2", "web-3"]));
    let asked: Vec<Value> = ui
        .requests()
        .iter()
        .filter(|r| r.method() == m("Hosts/List"))
        .map(|r| r.body())
        .collect();
    assert_eq!(asked, vec![json!({}), json!({"pageToken": "web-2"})]);
}

#[test]
fn a_next_page_token_that_does_not_move_on_ends_the_host_list() {
    // Catches: a page that keeps asking for the same page when a reply names the token it was
    // asked with, or one that goes back.
    for token in ["web-2", "web-1"] {
        let mut ui = Ui::start();
        let one = |id: &str| json!({"nodeId": id, "role": "web"});
        ui.reply(
            &m("Hosts/List"),
            json!({"hosts": [one("web-2")], "nextPageToken": "web-2"}),
        );
        ui.reply(
            &m("Hosts/List"),
            json!({"hosts": [one("web-3")], "nextPageToken": token}),
        );
        ui.reply(&m("Hosts/List"), json!({"hosts": [one("web-9")]}));
        ui.reply(&m("Releases/Status"), json!({}));
        let shown = rows(&mut ui, "home", "{}");
        assert_eq!(shown.as_array().expect("rows").len(), 3, "{token}: {shown}");
        let asked = ui
            .methods()
            .iter()
            .filter(|name| **name == m("Hosts/List"))
            .count();
        assert_eq!(asked, 2, "{token}");
    }
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
            "inventoryRecordId": "web-2", "dataMacs": ["0c:c4:7a:00:00:02", "0c:c4:7a:00:00:03"],
            "bmcMac": "0c:c4:7a:00:00:01", "serial": "SN-2", "enrolledUnix": "1800000000",
            "firstHeartbeatUnix": "1800000090",
            "certNotAfterUnix": "1800604800", "lastRenewError": "renewal refused: host_excluded",
            "clockSkewBeats": "2",
            "trustedRootAgeSecs": "7862400", "trustedRootReason": "TRUSTED_ROOT_REASON_STALE",
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
            ["Role", "web"],
            ["Heartbeat", "live"],
            ["Last heartbeat", AT],
            ["Agent", "hostkeeper 0.4.1"],
            ["Labels", "rack=r1"],
            ["Excluded from", "metrics (noisy)"],
            ["Inventory record", "web-2"],
            ["Data MACs", "0c:c4:7a:00:00:02, 0c:c4:7a:00:00:03"],
            ["BMC MAC", "0c:c4:7a:00:00:01"],
            ["Serial", "SN-2"],
            ["Enrolled", AT],
            ["First heartbeat", "2027-01-15 08:01:30 UTC"],
            ["Installed version", "1.3.0"],
            ["Health", "unhealthy"],
            ["Up", "no"],
            ["Update slot", "holds the update slot"],
            ["Gate", "held"],
            ["Membership", "present"],
            ["Excluded", "no"],
            ["Target", "1.4.0"],
            ["Verdict", "failed (forced)"],
            ["Phase", "after_commit"],
            ["Class", "release"],
            ["Reason", "health_check"],
            ["Detail", "probe returned 503"],
            ["Try", "2"],
            ["Order", "order-17"],
            ["Started", AT],
            ["Installed version", "2.0.0"],
            ["Health", "healthy"],
            ["Up", "yes"],
            ["Update slot", "-"],
            ["Gate", "-"],
            ["Membership", "excluded"],
            ["Excluded", "yes: noisy"],
            ["Ends", "2027-01-22 08:00:00 UTC"],
            ["Last renewal error", "renewal refused: host_excluded"],
            ["Heartbeats with the clock off", "2"],
            ["State", "stale"],
            ["Age", "91 days (7862400 s)"],
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
fn the_home_page_shows_a_failed_status_call_and_a_refused_alerts_call() {
    // Catches: a Home page that hides the failure of its second call; and one that hides a
    // refused `Alerts.List` (an empty box would read as "nothing needs a person") or drops the
    // hosts because of it.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.refuse(&m("Releases/Status"), 500, "INTERNAL", "store_failed");
    let out = render(&mut ui, "home", "{}");
    assert_eq!(shown(&out), "Lab INTERNAL store_failed");
    assert_eq!(ui.methods(), [m("Hosts/List"), m("Releases/Status")]);

    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Releases/Status"), status_reply());
    ui.refuse(&m("Alerts/List"), 500, "INTERNAL", "store_failed");
    let out = render(&mut ui, "home", "{}");
    let text = shown(&out);
    assert!(
        text.contains("Alerts The alerts could not be read: INTERNAL store_failed"),
        "{text}"
    );
    assert!(!text.contains("Nothing needs a person."), "{text}");
    assert!(text.contains("Hosts (4)"), "{text}");
}

/// The reply to `Alerts.List` the coordinator gives for a canary of kbf that failed 2.0. The
/// coordinator's test of `Alerts.List` end to end (in `src/grpc/tests/alerts.rs`) fails when its
/// answer and this file part.
const ALERTS_LIST: &str = include_str!("fixtures/alerts_list.json");

#[test]
fn the_home_page_shows_the_failed_canary_of_the_coordinators_own_reply() {
    // Catches: a failed canary missing from the Home page (it derived its alerts itself and
    // could not name a canary), and a page that renders only the replies written for it, not
    // the one the coordinator gives.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(
        &m("Alerts/List"),
        serde_json::from_str(ALERTS_LIST).expect("the fixture is JSON"),
    );
    let out = render(&mut ui, "home", "{}");
    let text = shown(&out).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        text.contains(
            "Alerts (1) failed canary (critical): d1, a canary of kbf, failed 2.0: it is marked \
             bad and rolled back (since 2027-01-15 08:00:00 UTC)"
        ),
        "{text}"
    );
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
    ui.reply_raw(
        &m("Alerts/List"),
        200,
        &json!({"alerts": [{"kind": 42, "severity": 9, "message": "something new", "later": true}],
                "extra": 1})
        .to_string(),
    );
    let fleet = rows(&mut ui, "home", "{}");
    assert_eq!(
        fleet,
        json!([
            ["Host", "Role", "Heartbeat", "Last heartbeat", "Services"],
            ["web-1", "web", "live", "-", "api 1.4.0, -"],
        ])
    );
    assert_eq!(
        ui.methods(),
        [
            m("Hosts/List"),
            m("Releases/Status"),
            m("Alerts/List"),
            m("Layout/Get")
        ]
    );
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
