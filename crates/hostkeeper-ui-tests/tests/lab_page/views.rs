//! The Home page's views beyond the floor plan's structure: the state a host shows, the compact
//! alerts strip, the list view and the position of the rack strip. Shares the fixtures of
//! `lab_page.rs`.

use super::{home, hosts, layout, records, start};
use crate::support::Ui;
use crate::support::pages::m;
use serde_json::json;

#[test]
fn a_host_that_reports_a_fault_is_failed_and_never_healthy() {
    // Catches: a live host whose service reports itself unhealthy, or whose last update failed,
    // shown as healthy (the green state of a server that needs a person); a never-heard host or a
    // silent one that is shown as anything but pending or offline.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { hostState, worstState } from "./lab.js";
export async function main() {
  const beat = { lastHeartbeatUnix: "1800000000", live: true };
  const svc = (o) => ({ service: "api", ...o });
  return {
    healthy: hostState({ ...beat, services: [svc({ health: "HEALTH_HEALTHY" })] }),
    none: hostState({ ...beat }),
    unhealthy: hostState({ ...beat, services: [svc({ health: "HEALTH_HEALTHY" }), svc({ health: "HEALTH_UNHEALTHY" })] }),
    lastFailed: hostState({ ...beat, services: [svc({ health: "HEALTH_HEALTHY", lastUpdate: { outcome: "OUTCOME_FAILED" } })] }),
    lastDone: hostState({ ...beat, services: [svc({ lastUpdate: { outcome: "OUTCOME_SUCCEEDED" } })] }),
    updating: hostState({ ...beat, services: [svc({ holdsSlot: true })] }),
    failedWhileUpdating: hostState({ ...beat, services: [svc({ holdsSlot: true, health: "HEALTH_UNHEALTHY" })] }),
    silent: hostState({ live: false, lastHeartbeatUnix: "1799990000", services: [svc({ health: "HEALTH_UNHEALTHY" })] }),
    never: hostState({ live: false }),
    zero: hostState({ live: true, lastHeartbeatUnix: "0" }),
    worst: [
      worstState({ healthy: 1, updating: 1, failed: 1, offline: 1, pending: 1 }),
      worstState({ healthy: 1, updating: 1, failed: 0, offline: 1, pending: 1 }),
      worstState({ healthy: 1, updating: 1, failed: 0, offline: 0, pending: 1 }),
      worstState({ healthy: 1, updating: 1, failed: 0, offline: 0, pending: 0 }),
      worstState({ healthy: 2, updating: 0, failed: 0, offline: 0, pending: 0 }),
      worstState({ healthy: 0, updating: 0, failed: 0, offline: 0, pending: 0 }),
    ],
  };
}"#,
    );
    assert_eq!(out["healthy"], "healthy");
    assert_eq!(out["none"], "healthy");
    assert_eq!(out["unhealthy"], "failed");
    assert_eq!(out["lastFailed"], "failed");
    assert_eq!(out["lastDone"], "healthy");
    assert_eq!(out["updating"], "updating");
    assert_eq!(out["failedWhileUpdating"], "failed");
    assert_eq!(out["silent"], "offline");
    assert_eq!(out["never"], "pending");
    assert_eq!(out["zero"], "pending");
    assert_eq!(
        out["worst"],
        json!([
            "failed", "offline", "pending", "updating", "healthy", "empty"
        ])
    );
}

#[test]
fn an_unhealthy_server_is_counted_failed_and_its_plate_and_footprint_are_red_not_green() {
    // Catches: a placed server of an unhealthy host drawn with the healthy LED, counted among the
    // healthy ones, or a rack footprint whose segment for it is green; the floor plan's row tile
    // that does not say the worst state of its racks.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), records());
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [
            {"nodeId": "web-1", "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
             "services": [{"service": "hostkeeper", "installedVersion": "1.4.0", "health": "HEALTH_UNHEALTHY"}]},
        ]}),
    );
    ui.reply(&m("Layout/Get"), layout());
    let out = home(
        &mut ui,
        "{}",
        r#"
return {
  pills: cls(tree, "pill").map(words),
  segs: cls(tree, "seg").map((n) => [n.attrs.class, n.attrs.vars]),
  tile: named(tree, "open-row")[0].attrs.class,
  foot: cls(tree, "foot").map((n) => n.attrs.class),
};"#,
    );
    assert_eq!(
        out["pills"],
        json!([
            "healthy 0",
            "updating 0",
            "failed 1",
            "offline 0",
            "pending 2"
        ])
    );
    assert_eq!(
        out["segs"],
        json!([
            ["seg s-pending", {"--b": 0.0, "--h": 2.38}],
            ["seg s-failed", {"--b": 95.24, "--h": 4.76}]
        ])
    );
    assert_eq!(out["tile"], "row-open s-failed");
    assert_eq!(out["foot"][0], "foot s-failed");
    assert_eq!(out["foot"][1], "foot s-empty");
}

#[test]
fn the_alerts_are_one_compact_strip_toned_by_the_worst_alert() {
    // Catches: a big box for the alerts, one that shows a critical alert in the warning tone (or
    // the reverse), one that hides an alert, and a strip that does not say when nothing is wrong.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts());
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(
        &m("Alerts/List"),
        json!({"alerts": [
            {"kind": "ALERT_KIND_SILENT_HOST", "severity": "ALERT_SEVERITY_WARNING",
             "message": "no heartbeat", "firstObservedUnix": "1800000000"},
            {"kind": "ALERT_KIND_HALT", "severity": "ALERT_SEVERITY_CRITICAL",
             "message": "update failed", "firstObservedUnix": "1800000000"},
        ]}),
    );
    ui.reply(&m("Layout/Get"), layout());
    ui.reply(&m("Enrollment/ListRecords"), records());
    let out = home(
        &mut ui,
        "{}",
        r#"
const strip = cls(tree, "alert-strip")[0];
return { tone: strip.attrs.class, lines: cls(strip, "alert").map(words), dots: cls(strip, "sev").map((n) => n.attrs.class) };"#,
    );
    assert_eq!(out["tone"], "alert-strip bad");
    assert_eq!(
        out["lines"],
        json!([
            "silent host no heartbeat since 2027-01-15 08:00:00 UTC",
            "halt critical update failed since 2027-01-15 08:00:00 UTC"
        ])
    );
    assert_eq!(out["dots"], json!(["sev warn", "sev bad"]));

    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), hosts());
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(
        &m("Alerts/List"),
        json!({"alerts": [{"kind": "ALERT_KIND_SILENT_HOST", "severity": "ALERT_SEVERITY_WARNING",
                           "message": "no heartbeat", "firstObservedUnix": "1800000000"}]}),
    );
    ui.reply(&m("Layout/Get"), layout());
    ui.reply(&m("Enrollment/ListRecords"), records());
    let out = home(
        &mut ui,
        "{}",
        r#"return cls(tree, "alert-strip")[0].attrs.class;"#,
    );
    assert_eq!(out, "alert-strip warn");

    let mut ui = start();
    let out = home(
        &mut ui,
        "{}",
        r#"const strip = cls(tree, "alert-strip")[0]; return [strip.attrs.class, words(strip)];"#,
    );
    assert_eq!(
        out,
        json!(["alert-strip none", "No alerts: nothing needs a person."])
    );

    let mut ui = start();
    let out = home(
        &mut ui,
        "{ row: \"row-1\" }",
        r#"return cls(tree, "alert-strip").length;"#,
    );
    assert_eq!(out, 0, "a row's page carries no alerts strip");
}

#[test]
fn the_list_view_replaces_the_floor_plan_and_the_switch_says_which_is_shown() {
    // Catches: a list view that also draws the floor plan or the Unplaced tray, a Floor / List
    // switch that does not say which view is on or that loses the edit mode, and a server listed
    // without where it stands.
    let mut ui = start();
    let out = home(
        &mut ui,
        "{ list: true }",
        r#"
const tool = (name) => named(tree, name)[0];
const toggled = [];
tool("view-floor").on.click();
tool("view-list").on.click();
return {
  floor: cls(tree, "floor").length,
  tray: cls(tree, "tray").length,
  on: [tool("view-floor").attrs["aria-pressed"], tool("view-list").attrs["aria-pressed"]],
  go: seen.navigations,
  rows: nodes(tree).filter((n) => n.tag === "tr").map(words),
};"#,
    );
    assert_eq!(out["floor"], 0);
    assert_eq!(out["tray"], 0);
    assert_eq!(out["on"], json!(["false", "true"]));
    assert_eq!(
        out["go"],
        json!([
            {"name": "home", "params": {"list": false}},
            {"name": "home", "params": {"list": true}}
        ])
    );
    let rows = out["rows"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r.as_str().unwrap().contains("web-1")
            && r.as_str().unwrap().contains("Row 1 / Rack 1, U41")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.as_str().unwrap().contains("web-2")
                && r.as_str().unwrap().contains("Unplaced")),
        "{rows:?}"
    );

    let mut ui = start();
    let out = home(
        &mut ui,
        "{ list: true }",
        r#"
named(tree, "open-host").find((n) => words(n) === "new-1").on.click();
named(tree, "open-host").find((n) => words(n) === "web-1").on.click();
return seen.navigations;"#,
    );
    assert_eq!(
        out,
        json!([
            {"name": "inventory", "params": {"record": "new-1"}},
            {"name": "host", "params": {"nodeId": "web-1"}}
        ])
    );

    let mut ui = start();
    let out = home(
        &mut ui,
        "{ edit: true }",
        r#"named(tree, "view-list")[0].on.click(); return seen.navigations;"#,
    );
    assert_eq!(
        out,
        json!([{"name": "home", "params": {"edit": true, "list": true}}])
    );

    let mut ui = start();
    let out = home(
        &mut ui,
        "{ row: \"row-1\" }",
        r#"return nodes(tree).filter((n) => n.attrs.name === "view-list").length;"#,
    );
    assert_eq!(out, 0, "a row has no Floor / List switch");
}

#[test]
fn the_strip_tells_which_rack_is_in_view_and_where_it_ends() {
    // Catches: a strip with no sign of where it is (the rack at its left edge out of how many), arrows
    // that stay on at the ends or move by a fixed distance in place of a rack, and edge fades that
    // show with nothing beyond them.
    let mut ui = start();
    let out = home(
        &mut ui,
        r#"{ row: "row-1" }"#,
        r#"
const make = (extra) => {
  const classes = new Set(["strip-frame", "at-start"]);
  const pos = { textContent: "" };
  const prev = { disabled: false };
  const next = { disabled: false };
  const frame = {
    classList: { toggle: (c, on) => (on ? classes.add(c) : classes.delete(c)) },
    querySelector: (sel) => ({ ".strip-pos": pos, "[name=strip-prev]": prev, "[name=strip-next]": next, ".rack-strip": strip })[sel],
  };
  const strip = { parentNode: frame, scrollLeft: 0, scrollWidth: 1000, clientWidth: 600,
    firstElementChild: { offsetWidth: 276 }, scrollBy: (o) => moves.push(o.left), ...extra };
  return { frame, strip, classes, pos, prev, next };
};
const moves = [];
const bar = cls(tree, "strip-frame")[0];
const scroller = cls(tree, "rack-strip")[0];
const arrow = (name) => named(tree, name)[0];
const at = (scrollLeft) => {
  const els = make({ scrollLeft });
  scroller.on.scroll({ currentTarget: els.strip });
  return [els.pos.textContent, els.prev.disabled, els.next.disabled, [...els.classes].sort()];
};
const middle = at(300);
const start = at(0);
const end = at(400);
const els = make({});
arrow("strip-next").on.click({ currentTarget: { closest: () => els.frame } });
arrow("strip-prev").on.click({ currentTarget: { closest: () => els.frame } });
return {
  initial: [words(cls(tree, "strip-pos")[0]), arrow("strip-prev").attrs.disabled, arrow("strip-next").attrs.disabled, bar.attrs.class],
  start, middle, end, moves,
};"#,
    );
    assert_eq!(
        out["initial"],
        json!(["Rack 1 of 2", true, null, "strip-frame at-start"])
    );
    assert_eq!(
        out["start"],
        json!(["Rack 1 of 2", true, false, ["at-start", "strip-frame"]])
    );
    assert_eq!(
        out["middle"],
        json!(["Rack 2 of 2", false, false, ["strip-frame"]])
    );
    assert_eq!(
        out["end"],
        json!(["Rack 2 of 2", false, true, ["at-end", "strip-frame"]])
    );
    assert_eq!(out["moves"], json!([300, -300]));
}
