//! The shape of the Host page: a status header with the host's state and a small elevation of its
//! rack, tabs over the sections, and Retire apart in a danger zone at the bottom.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{m, page_driver};

fn layout() -> Value {
    json!({
        "rows": [{"id": "row-1", "number": 0}],
        "racks": [{"id": "rack-1", "row": "row-1", "number": 1, "heightU": 10}],
        "sleds": [
            {"id": "sled-1", "rack": "rack-1", "u": 4, "heightU": 2, "slots": 1, "address": "C0-R0-R1-S1"},
            {"id": "sled-2", "rack": "rack-1", "u": 1, "heightU": 1, "slots": 1, "address": "C0-R0-R1-S0"},
        ],
        "placements": [
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-2"}, "rack": "rack-1", "u": 4, "heightU": 2,
             "sled": "sled-1", "address": "C0-R0-R1-S1-S0"},
            {"subject": {"kind": "LAYOUT_SUBJECT_KIND_HOST", "id": "web-3"}, "rack": "rack-1", "u": 1, "heightU": 1,
             "sled": "sled-2", "address": "C0-R0-R1-S0-S0"},
        ],
    })
}

/// A gateway that answers the three reads of the page `renders` times (a scripted reply is
/// used once).
fn ui_renders(host: &Value, renders: usize) -> Ui {
    let ui = Ui::start();
    for _ in 0..renders {
        ui.reply(&m("Hosts/Get"), json!({ "host": host }));
        ui.reply(&m("Services/List"), json!({}));
        ui.reply(&m("Layout/Get"), layout());
    }
    ui
}

fn ui_with(host: Value) -> Ui {
    ui_renders(&host, 1)
}

const HEADER: &str = r#"
const bar = nodes(tree).find((n) => n.attrs["class"] === "hostbar");
const pillOf = nodes(bar).find((n) => String(n.attrs["class"]).startsWith("pill"));
return { state: text(pillOf), tone: pillOf.attrs["class"] };"#;

#[test]
fn the_header_says_degraded_for_a_live_host_with_an_unhealthy_service_not_healthy() {
    // Asserts: a live host is `healthy` only when none of its services is unhealthy and none of
    // their last updates failed; a silent host is `offline`; one never heard from is `pending`;
    // one holding an update slot is `updating`. Catches: an unhealthy service shown as healthy.
    let live = json!({"nodeId": "web-2", "live": true, "lastHeartbeatUnix": "1800000000"});
    let with = |services: Value| {
        let mut host = live.clone();
        host["services"] = services;
        host
    };
    let cases = [
        (live.clone(), "healthy", "pill ok"),
        (
            with(json!([{"service": "api", "health": "HEALTH_UNHEALTHY"}])),
            "degraded",
            "pill bad",
        ),
        (
            with(json!([{"service": "api", "health": "HEALTH_HEALTHY",
                         "lastUpdate": {"outcome": "OUTCOME_FAILED"}}])),
            "degraded",
            "pill bad",
        ),
        (
            with(json!([{"service": "api", "holdsSlot": true}])),
            "updating",
            "pill info",
        ),
        (
            json!({"nodeId": "web-2", "live": false, "lastHeartbeatUnix": "1799999000"}),
            "offline",
            "pill bad",
        ),
        (json!({"nodeId": "web-2"}), "pending", "pill muted"),
    ];
    for (host, state, tone) in cases {
        let mut ui = ui_with(host.clone());
        let out = ui.run(&page_driver("host", r#"{ nodeId: "web-2" }"#, "[]", HEADER));
        assert_eq!(out["out"], json!({"state": state, "tone": tone}), "{host}");
    }
}

#[test]
fn the_page_has_one_tab_open_at_a_time_and_keeps_it_across_a_refresh_of_the_same_host() {
    // Asserts: the six tabs in order, Overview on at first, a click on a tab shows that panel and
    // hides the others in the tree the page hands to `show`, a refresh of the same host opens the
    // same tab, and another host opens Overview.
    let mut ui = ui_renders(
        &json!({"nodeId": "web-2", "live": true, "lastHeartbeatUnix": "1800000000"}),
        3,
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const tabs = (t) => nodes(t).filter((n) => n.tag === "button" && String(n.attrs["name"]).startsWith("tab-"));
const on = (t) => tabs(t).filter((n) => n.attrs["aria-pressed"] === "true").map((n) => n.attrs["name"]);
const open = (t) => nodes(t).filter((n) => n.attrs["class"] === "panel").length;
const first = { names: tabs(tree).map((n) => n.attrs["name"]), on: on(tree), open: open(tree) };
await click(tree, "tab-services");
const shown = seen.shown[seen.shown.length - 1];
const second = { on: on(shown), open: open(shown) };
const again = await render(ctx);
const kept = on(again);
const other = recorder(api("operator-token"), { nodeId: "web-9" }, []);
const reset = on(await render(other.ctx));
return { first, second, kept, reset };"#,
    ));
    assert_eq!(
        out["out"]["first"],
        json!({"names": ["tab-overview", "tab-services", "tab-trust", "tab-labels", "tab-placement", "tab-group"],
               "on": ["tab-overview"], "open": 1})
    );
    assert_eq!(
        out["out"]["second"],
        json!({"on": ["tab-services"], "open": 1})
    );
    assert_eq!(out["out"]["kept"], json!(["tab-services"]));
    assert_eq!(out["out"]["reset"], json!(["tab-overview"]));
}

#[test]
fn a_tab_shows_a_dot_when_its_section_needs_attention() {
    // Asserts: the Services tab carries a bad dot for an unhealthy service, the Certificate tab a
    // warn dot for a certificate that has ended, and a healthy host's tabs carry none but the
    // muted one of Placement for a host in no rack.
    let mut ui = ui_with(json!({
        "nodeId": "web-9", "live": true, "lastHeartbeatUnix": "1800000000",
        "certNotAfterUnix": "1700000000",
        "services": [{"service": "api", "health": "HEALTH_UNHEALTHY"}],
    }));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-9" }"#,
        "[]",
        r#"
return nodes(tree).filter((n) => n.attrs["class"] && String(n.attrs["class"]).startsWith("dot"))
  .map((n) => n.attrs["class"]);"#,
    ));
    assert_eq!(out["out"], json!(["dot bad", "dot warn", "dot muted"]));
}

#[test]
fn retire_is_the_last_thing_on_the_page_in_a_danger_zone_with_a_danger_button() {
    // Asserts: the Retire fieldset is the danger zone, after every tab panel, and its button is
    // the one of class `danger`.
    let mut ui =
        ui_with(json!({"nodeId": "web-2", "live": true, "lastHeartbeatUnix": "1800000000"}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const last = tree.children[tree.children.length - 1];
const button = named(tree, "retire")[0];
return { zone: last.attrs["class"], legend: text(last.children[0]), button: button.attrs["class"] };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"zone": "danger-zone", "legend": "Danger zone", "button": "danger"})
    );
}

#[test]
fn the_mini_elevation_lights_the_hosts_u_among_its_neighbours_or_says_unplaced() {
    // Asserts: a placed host's elevation has the rack's 10 U, a dim block for the neighbouring sled at
    // U1 and a lit block of 2 U at U4 (rows counted from the top), with the caption naming the rack,
    // the U and, under it, the host's address; a host on no sled gets an empty elevation captioned
    // Unplaced.
    let mut ui =
        ui_with(json!({"nodeId": "web-2", "live": true, "lastHeartbeatUnix": "1800000000"}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"
const rack = nodes(tree).find((n) => n.attrs["class"] === "mini-rack");
const blocks = nodes(rack).map((n) => ({ c: n.attrs["class"], r: n.attrs.vars["--r"], s: n.attrs.vars["--s"] })).slice(1);
const cap = text(nodes(tree).find((n) => n.attrs["class"] === "mini-cap"));
const address = text(named(tree, "address")[0]);
return { units: rack.attrs.vars["--units"], blocks, cap, address };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "units": 10.0,
            "blocks": [
                {"c": "mini-u", "r": 10.0, "s": 1.0},
                {"c": "mini-u me", "r": 6.0, "s": 2.0},
            ],
            "cap": "Row 0 / Rack 1 U4-5 of 10",
            "address": "C0-R0-R1-S1-S0",
        })
    );

    let mut ui =
        ui_with(json!({"nodeId": "web-7", "live": true, "lastHeartbeatUnix": "1800000000"}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-7" }"#,
        "[]",
        r#"
const rack = nodes(tree).find((n) => String(n.attrs["class"]).startsWith("mini-rack"));
return { rack: rack.attrs["class"], cap: text(nodes(tree).find((n) => n.attrs["class"] === "mini-cap")) };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"rack": "mini-rack empty", "cap": "Unplaced on no sled"})
    );
}
