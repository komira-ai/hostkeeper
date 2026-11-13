//! The package page (`ui/pages/package.js`) against the fake gateway: a package's repository and
//! versions, its host groups with the desired version against the versions the hosts run, the
//! progress of a rollout worked out from the hosts' states, the manifest of a version (readable
//! and raw), the acts on it (set desired, pause, resume, hold, release), the status polled while
//! a rollout runs, and the routes of the Release, Rollout and Holds pages that lead here.

mod support;

use std::collections::BTreeSet;

use hostkeeper_ui_tests::{UiFile, listed_calls};
use serde_json::{Value, json};
use support::Ui;
use support::pages::{alerts_reply, hosts_reply, m, page_driver, shown, status_reply};

/// One host of a status entry: its version of `api` and the gate's word.
fn member(id: &str, version: Option<&str>, gate: &str, live: bool) -> Value {
    let mut view = json!({"service": "api", "gateReason": gate});
    if let Some(version) = version {
        view["installedVersion"] = json!(version);
    }
    json!({"nodeId": id, "role": "web", "live": live, "view": view})
}

/// A host that an operator holds `api` on.
fn held(id: &str) -> Value {
    let mut host = member(id, Some("1.3.2"), "GATE_REASON_HELD", true);
    host["view"]["holds"] = json!([{"scope": {"host": id}, "service": "api", "reason": "investigating",
                                    "setBy": "ops", "setUnix": "1800000000"}]);
    host
}

/// The hosts of group `web` while `api` is rolled to 1.4.0: eight run it, one holds the update
/// slot, one failed, two are held, and one is down (and still on the old version).
fn web_hosts() -> Vec<Value> {
    let mut hosts: Vec<Value> = (1..=8)
        .map(|i| {
            member(
                &format!("web-{i}"),
                Some("1.4.0"),
                "GATE_REASON_UP_TO_DATE",
                true,
            )
        })
        .collect();
    let mut updating = member("web-9", Some("1.3.2"), "GATE_REASON_UPDATING", true);
    updating["view"]["holdsSlot"] = json!(true);
    hosts.push(updating);
    hosts.push(member(
        "web-10",
        Some("1.3.2"),
        "GATE_REASON_FAILED_HERE",
        true,
    ));
    hosts.push(held("web-11"));
    hosts.push(held("web-12"));
    hosts.push(member(
        "web-13",
        Some("1.3.2"),
        "GATE_REASON_NO_SLOT",
        false,
    ));
    hosts
}

fn web_status(hosts: Vec<Value>, plan: Value) -> Value {
    json!({"service": "api", "hostGroup": "web", "desired": {"version": "1.4.0"},
           "convergence": {"desired": "1.4.0", "state": "CONVERGENCE_STATE_IN_PROGRESS"},
           "hosts": hosts, "marks": {"lastGood": "1.3.2", "bad": ["1.3.0"]}, "plan": plan})
}

fn running_plan() -> Value {
    json!({"version": "1.4.0", "current": 1, "eligible": 13, "waiting": "PLAN_WAIT_BAKING",
           "stages": [{"number": 1, "percent": 100, "hosts": 13, "advance": "STAGE_ADVANCE_AUTO"}]})
}

fn db_status() -> Value {
    json!({"service": "api", "hostGroup": "db", "desired": {"version": "1.3.2"},
           "convergence": {"desired": "1.3.2", "state": "CONVERGENCE_STATE_CONVERGED"},
           "hosts": [member("db-1", Some("1.3.2"), "GATE_REASON_UP_TO_DATE", true),
                     member("db-2", Some("1.3.2"), "GATE_REASON_UP_TO_DATE", true)],
           "plan": {"version": "1.3.2", "current": 1, "eligible": 2, "finished": true, "waiting": "PLAN_WAIT_NONE",
                    "stages": [{"number": 1, "percent": 100, "hosts": 2, "advance": "STAGE_ADVANCE_AUTO"}]}})
}

fn status(entries: Vec<Value>) -> Value {
    json!({"services": entries})
}

fn services_list() -> Value {
    json!({"services": [{"service": "api", "repository": "komira",
                         "minVersions": [{"role": "web", "version": "1.2.0"}]}],
           "holds": [{"scope": {"host": "web-11"}, "service": "api", "reason": "investigating",
                      "setBy": "ops", "setUnix": "1800000000"},
                     {"scope": {"fleet": {}}, "service": "other", "reason": "not this package"}]})
}

fn release() -> Value {
    json!({"targets": {
        "linux-x86_64": {"entries": [{"installer": "deb", "digest": "11".repeat(32), "bytes": "4096",
                                      "manifest": {"package": "api-server", "minHostkeeper": "0.1.170"}}]},
        "macos-arm64": {"entries": [{"installer": "hostkeeper-archive", "digest": "22".repeat(32)}]},
    }, "releaseIndexDigest": "33".repeat(32)})
}

/// Scripts what the page reads first, in the order it reads it.
fn script(ui: &Ui, entries: Vec<Value>) {
    ui.reply(&m("Releases/Status"), status(entries));
    ui.reply(&m("Services/List"), services_list());
    ui.reply(
        &m("Repositories/List"),
        json!({"repositories": [{"repository": {"name": "komira", "uri": "https://packages.example.test/apt"}}]}),
    );
    ui.reply(&m("Releases/GetRelease"), release());
    ui.reply(&m("Hosts/List"), json!({"hosts": []}));
}

/// Scripts what the Fleet page, which the frame opens first, reads.
fn fleet_replies(ui: &Ui) {
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Hosts/List"), hosts_reply());
    ui.reply(&m("Alerts/List"), alerts_reply());
}

/// Renders the package page of `api`, then runs `then`.
fn package(ui: &mut Ui, params: &str, answers: &str, then: &str) -> Value {
    ui.run(&page_driver("package", params, answers, then))
}

fn bodies_of(ui: &Ui, method: &str) -> Vec<Value> {
    ui.requests()
        .iter()
        .filter(|r| r.method() == m(method))
        .map(support::Request::body)
        .collect()
}

#[test]
fn the_page_shows_the_repository_the_versions_the_groups_and_each_rollouts_progress() {
    // Catches: a page that leaves out the package's repository or its address, a version list
    // that misses a version a status names or is not highest first, a group row that drops the
    // desired, minimum or current versions, and a bar whose counts are not the hosts' states.
    let mut ui = Ui::start();
    script(
        &ui,
        vec![web_status(web_hosts(), running_plan()), db_status()],
    );
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return rows(tree);");
    let text = &shown(&out).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        text.contains("Repository: komira (https://packages.example.test/apt)"),
        "{text}"
    );
    assert!(text.contains("Versions: 1.4.0 1.3.2 1.3.0"), "{text}");
    let rows = out["out"].as_array().expect("rows");
    let row = |first: &str| {
        rows.iter()
            .find(|r| r[0] == first && r.as_array().map_or(0, Vec::len) == 6)
            .unwrap_or_else(|| panic!("no group row {first}: {rows:?}"))
            .clone()
    };
    assert_eq!(
        row("web"),
        json!([
            "web",
            "1.4.0",
            "1.2.0",
            "1.4.0 (8 hosts), 1.3.2 (5 hosts)",
            "in_progress",
            "8 of 13 updated, 1 updating, 1 failed, 2 held, 1 down"
        ])
    );
    assert_eq!(
        row("db"),
        json!([
            "db",
            "1.3.2",
            "-",
            "1.3.2 (2 hosts)",
            "converged",
            "2 of 2 updated"
        ])
    );
    assert!(
        text.contains("web: rollout under way") && text.contains("db: no rollout under way"),
        "{text}"
    );
}

#[test]
fn a_bar_is_a_progress_element_that_counts_updated_hosts_of_the_group() {
    // Catches: a bar that is not the native progress element (a colour or an inline style would
    // be the only sign), one whose value or maximum is not the hosts updated of the hosts of the
    // group, and one with no words beside it.
    let mut ui = Ui::start();
    script(&ui, vec![web_status(web_hosts(), running_plan())]);
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[]",
        r#"const bar = nodes(tree).find((n) => n.tag === "progress" && n.attrs.name === "progress-web");
return { value: bar.attrs.value, max: bar.attrs.max, label: bar.attrs["aria-label"],
         words: text(named(tree, "progress-text-web")[0]) };"#,
    );
    assert_eq!(
        out["out"],
        json!({"value": 8, "max": 13, "label": "web: 61% updated",
               "words": " 8 of 13 updated, 1 updating, 1 failed, 2 held, 1 down"})
    );
}

#[test]
fn progress_is_computed_from_the_hosts_states() {
    // Catches: a count that takes a host for another state, a rollout reported finished while a
    // host runs the old version, one reported active with no desired version, a paused plan not
    // marked, and a percent that rounds up to 100 before the last host.
    let mut ui = Ui::start();
    let mut paused = running_plan();
    paused["paused"] = json!(true);
    paused["pausedBy"] = json!("ops");
    paused["pauseReason"] = json!("freeze");
    paused["waiting"] = json!("PLAN_WAIT_PAUSED");
    let mut done = running_plan();
    done["finished"] = json!(true);
    let cases = json!({
        "empty": {"service": "api"},
        "all_down": {"service": "api", "desired": {"version": "1.4.0"},
                     "hosts": [member("a", Some("1.3.2"), "GATE_REASON_NONE", false),
                               member("b", None, "GATE_REASON_NONE", false)]},
        "paused": web_status(web_hosts(), paused),
        "plan_says_finished": web_status(web_hosts(), done),
        "no_desired": {"service": "api", "hosts": [member("a", Some("1.4.0"), "GATE_REASON_NONE", true)]},
        "one_short": {"service": "api", "desired": {"version": "1.4.0"},
                      "hosts": [member("a", Some("1.4.0"), "GATE_REASON_NONE", true),
                                member("b", Some("1.4.0"), "GATE_REASON_NONE", true),
                                member("c", Some("1.3.0"), "GATE_REASON_UPDATING", true)]},
        "all_there": {"service": "api", "desired": {"version": "1.4.0"},
                      "hosts": [member("a", Some("1.4.0"), "GATE_REASON_NONE", true)]},
    });
    let out = ui.run(&format!(
        r#"
import {{ rolloutProgress }} from "./pages/package.js";
export async function main() {{
  const cases = {cases};
  const out = {{}};
  for (const [name, svc] of Object.entries(cases)) {{
    out[name] = rolloutProgress(svc);
  }}
  return out;
}}"#
    ));
    let zero = json!({"total": 0, "updated": 0, "updating": 0, "failed": 0, "held": 0, "down": 0,
                      "waiting": "-", "paused": false, "finished": false, "active": false, "percent": 0});
    let with = |changes: Value| {
        let mut base = zero.clone();
        for (key, value) in changes.as_object().expect("changes") {
            base[key] = value.clone();
        }
        base
    };
    assert_eq!(out["empty"], zero);
    assert_eq!(
        out["all_down"],
        with(json!({"total": 2, "down": 2, "active": true}))
    );
    assert_eq!(out["no_desired"], with(json!({"total": 1})));
    assert_eq!(
        out["one_short"],
        with(json!({"total": 3, "updated": 2, "updating": 1, "active": true, "percent": 66}))
    );
    assert_eq!(
        out["all_there"],
        with(json!({"total": 1, "updated": 1, "finished": true, "percent": 100}))
    );
    assert_eq!(out["paused"]["paused"], true);
    assert_eq!(out["paused"]["waiting"], "paused by ops: freeze");
    assert_eq!(out["paused"]["active"], true);
    assert_eq!(out["plan_says_finished"]["finished"], true);
    assert_eq!(out["plan_says_finished"]["active"], false);
    assert_eq!(out["plan_says_finished"]["updated"], 8);
}

#[test]
fn the_manifest_is_readable_and_the_raw_toggle_shows_the_reply() {
    // Catches: a manifest that drops a platform or an entry, the digest of the release index, the
    // version asked for (the highest desired one by default, the named one when named), a toggle
    // that does not switch to the reply as the API gave it and back, and a refused read shown as
    // anything but its refusal.
    let mut ui = Ui::start();
    script(
        &ui,
        vec![web_status(web_hosts(), running_plan()), db_status()],
    );
    let out = package(
        &mut ui,
        r#"{ package: "api", version: "1.3.2" }"#,
        "[]",
        r#"
await click(tree, "manifest-toggle");
const raw = seen.shown[0];
const pre = nodes(raw).find((n) => n.tag === "pre");
const toggled = text(named(raw, "manifest-toggle")[0]);
await click(raw, "manifest-toggle");
return { rows: rows(tree), raw: JSON.parse(text(pre)), toggled, back: text(named(seen.shown[1], "manifest-toggle")[0]),
         platforms: nodes(raw).filter((n) => n.attrs["class"] === "platform").length };"#,
    );
    assert!(
        shown(&out).contains("Manifest of api 1.3.2"),
        "{}",
        shown(&out)
    );
    assert_eq!(
        bodies_of(&ui, "Releases/GetRelease"),
        vec![json!({"service": "api", "version": "1.3.2"})]
    );
    assert_eq!(out["out"]["raw"], release());
    assert_eq!(out["out"]["toggled"], "Readable");
    assert_eq!(out["out"]["back"], "Raw");
    assert_eq!(out["out"]["platforms"], 0);
    let rows = out["out"]["rows"].as_array().expect("rows");
    assert!(rows.contains(&json!(["Platforms", "linux-x86_64, macos-arm64"])));
    assert!(
        rows.iter().any(|r| r[1] == "deb" && r[6] == "api-server"),
        "{rows:?}"
    );

    let mut ui = Ui::start();
    script(&ui, vec![web_status(web_hosts(), running_plan())]);
    let out = package(&mut ui, "{ package: \"api\" }", "[]", "return null;");
    assert_eq!(
        bodies_of(&ui, "Releases/GetRelease"),
        vec![json!({"service": "api", "version": "1.4.0", "hostGroup": "web"})],
        "{}",
        shown(&out)
    );

    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status(vec![db_status()]));
    ui.refuse(
        &m("Releases/GetRelease"),
        404,
        "NOT_FOUND",
        "release_unknown",
    );
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return null;");
    assert!(
        shown(&out).contains("NOT_FOUND release_unknown"),
        "{}",
        shown(&out)
    );
}

#[test]
fn the_buttons_call_the_operator_methods_with_the_group_and_the_reason() {
    // Catches: a Set desired that names no host group (and so sets every group), one sent without
    // the question answered yes, a Pause or Resume that leaves out the reason or goes to the wrong
    // method, a Hold that sends no scope or no reason, and a Release that lifts another hold.
    let mut ui = Ui::start();
    let mut paused = running_plan();
    paused["paused"] = json!(true);
    paused["waiting"] = json!("PLAN_WAIT_PAUSED");
    script(
        &ui,
        vec![web_status(web_hosts(), running_plan()), {
            let mut db = db_status();
            db["plan"] = paused;
            db["plan"]["finished"] = json!(false);
            db
        }],
    );
    ui.reply(&m("Rollouts/Pause"), json!({}));
    ui.reply(&m("Rollouts/Resume"), json!({}));
    ui.reply(&m("Releases/SetDesired"), json!({}));
    ui.reply(&m("Services/Hold"), json!({}));
    ui.reply(&m("Services/Unhold"), json!({}));
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[true, true, true]",
        r#"
type(tree, "desired-version", "1.5.0", 0);
await click(tree, "set-desired", 0);
type(tree, "reason", "freeze for audit", 0);
await click(tree, "pause", 0);
type(tree, "reason", "thaw", 1);
await click(tree, "resume", 0);
choose(tree, "hold-scope", "role");
type(tree, "hold-value", "web", 0);
await click(tree, "hold");
type(tree, "hold-reason", "canary looks bad", 0);
await click(tree, "hold");
type(tree, "unhold-reason", "fixed", 0);
await click(tree, "unhold");
return seen.reports;"#,
    );
    assert_eq!(
        bodies_of(&ui, "Releases/SetDesired"),
        vec![json!({"service": "api", "desired": {"version": "1.5.0"}, "hostGroup": "web"})]
    );
    assert_eq!(
        bodies_of(&ui, "Rollouts/Pause"),
        vec![json!({"service": "api", "reason": "freeze for audit"})]
    );
    assert_eq!(
        bodies_of(&ui, "Rollouts/Resume"),
        vec![json!({"service": "api", "reason": "thaw"})]
    );
    assert_eq!(
        bodies_of(&ui, "Services/Hold"),
        vec![json!({"scope": {"role": "web"}, "service": "api", "reason": "canary looks bad"})]
    );
    assert_eq!(
        bodies_of(&ui, "Services/Unhold"),
        vec![json!({"scope": {"host": "web-11"}, "service": "api", "reason": "fixed"})]
    );
    let reports = out["out"].as_array().expect("reports");
    assert!(
        reports.contains(&json!("A hold needs a reason.")),
        "{reports:?}"
    );
}

#[test]
fn only_the_holds_of_this_package_are_listed() {
    // Catches: a Holds section that lists the holds of every package, or leaves out the count.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return rows(tree);");
    assert!(shown(&out).contains("Holds on api (1)"), "{}", shown(&out));
    assert!(!shown(&out).contains("not this package"));
    assert!(
        out["out"].as_array().expect("rows").contains(&json!([
            "host web-11",
            "investigating",
            "ops",
            "2027-01-15 08:00:00 UTC",
            "Release"
        ])),
        "{}",
        out["out"]
    );
}

#[test]
fn a_running_rollout_is_polled_and_the_bar_moves_until_it_finishes() {
    // Catches: a page that never asks again while a rollout runs, one that asks at another
    // interval than five seconds, a bar that keeps its old counts after the next status, polling
    // that goes on after the rollout has finished, and a failed refresh that hides the old counts
    // without saying so.
    let mut ui = Ui::start();
    script(&ui, vec![web_status(web_hosts(), running_plan())]);
    // The second status: web-9 has finished its update; the plan still runs.
    let mut more = web_hosts();
    more[8]["view"]["installedVersion"] = json!("1.4.0");
    more[8]["view"]["gateReason"] = json!("GATE_REASON_UP_TO_DATE");
    more[8]["view"]["holdsSlot"] = json!(false);
    ui.reply(
        &m("Releases/Status"),
        status(vec![web_status(more, running_plan())]),
    );
    ui.refuse(
        &m("Releases/Status"),
        503,
        "UNAVAILABLE",
        "coordinator_starting",
    );
    // The fourth: every host runs 1.4.0 and the plan is finished.
    let all: Vec<Value> = (1..=13)
        .map(|i| {
            member(
                &format!("web-{i}"),
                Some("1.4.0"),
                "GATE_REASON_UP_TO_DATE",
                true,
            )
        })
        .collect();
    let mut done_plan = running_plan();
    done_plan["finished"] = json!(true);
    ui.reply(
        &m("Releases/Status"),
        status(vec![web_status(all, done_plan)]),
    );
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[]",
        r#"
const ticks = seen.repeats;
const words = () => text(named(seen.shown[seen.shown.length - 1], "progress-text-web")[0]);
const first = ticks.length;
await ticks[0].run();
const second = words();
await ticks[0].run();
const failed = text(seen.shown[seen.shown.length - 1]);
const failedWords = words();
const stopsBefore = ticks[0].stops;
await ticks[0].run();
return { first, seconds: ticks[0].seconds, second, failed, failedWords, stopsBefore, third: words(),
         stops: ticks[0].stops };"#,
    );
    assert_eq!(out["out"]["first"], 1);
    assert_eq!(out["out"]["seconds"], 5);
    assert_eq!(
        out["out"]["second"],
        " 9 of 13 updated, 1 failed, 2 held, 1 down"
    );
    let failed = out["out"]["failed"].as_str().expect("text");
    assert!(
        failed.contains("Progress was not refreshed: UNAVAILABLE coordinator_starting"),
        "{failed}"
    );
    assert_eq!(
        out["out"]["failedWords"],
        " 9 of 13 updated, 1 failed, 2 held, 1 down"
    );
    assert_eq!(out["out"]["stopsBefore"], 0);
    assert_eq!(out["out"]["third"], " 13 of 13 updated");
    assert_eq!(out["out"]["stops"], 1);
}

#[test]
fn a_package_with_no_rollout_under_way_is_not_polled() {
    // Catches: a repeat started for a package whose groups are all converged.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[]",
        "return seen.repeats.length;",
    );
    assert_eq!(out["out"], 0);
}

#[test]
fn the_frame_stops_a_repeat_when_the_page_is_left() {
    // Catches: a repeat that goes on asking for a package's status after the operator has gone
    // to another page, and one that never starts under the frame.
    let mut ui = Ui::start();
    ui.session("tok_a");
    fleet_replies(&ui);
    script(&ui, vec![web_status(web_hosts(), running_plan())]);
    ui.reply(&m("Services/List"), services_list());
    ui.reply(&m("Repositories/List"), json!({}));
    let out = ui.run(
        r#"
import { fakeDocument, settle } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const timers = [];
  let stopped = 0;
  const app = startApp({ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    every: (run, ms) => { timers.push({ run, ms }); return () => { stopped += 1; }; } });
  await settle();
  app.navigate("package", { package: "api" });
  await settle();
  const started = timers.map((t) => t.ms);
  app.navigate("packages", {});
  await settle();
  timers[0].run();
  await settle();
  return { started, stopped };
}"#,
    );
    assert_eq!(out, json!({"started": [5000], "stopped": 1}));
    // The Fleet page's read and the package page's; the tick after the page was left asks nothing.
    assert_eq!(bodies_of(&ui, "Releases/Status").len(), 2);
}

#[test]
fn the_old_routes_lead_to_the_package_or_to_packages() {
    // Catches: a bookmark or link to Release, Rollout or Holds that opens nothing or the wrong
    // page, a route that drops the service or the version it names, and a route that is not one of
    // the three redirected.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { redirected } from "./app.js";
export async function main() {
  return [
    redirected("release", { service: "web", version: "1.0.0" }),
    redirected("release", { service: "web" }),
    redirected("rollout", { service: "web" }),
    redirected("rollout", {}),
    redirected("release", { service: 7 }),
    redirected("holds", { service: "web" }),
    redirected("holds"),
    redirected("host", { nodeId: "web-1" }),
    redirected("audit"),
  ];
}"#,
    );
    assert_eq!(
        out,
        json!([
            ["package", {"package": "web", "version": "1.0.0"}],
            ["package", {"package": "web"}],
            ["package", {"package": "web"}],
            ["packages", {}],
            ["packages", {}],
            ["packages", {}],
            ["packages", {}],
            ["host", {"nodeId": "web-1"}],
            ["audit", {}],
        ])
    );
}

#[test]
fn the_page_list_has_no_release_rollout_or_holds_and_their_routes_open_the_package_page() {
    // Catches: a Release, Rollout or Holds button left in the page list, a package page that is
    // listed (it is reached from Packages), a route that is not redirected by the frame, and a
    // route that opens the package page without asking for the package named.
    let mut ui = Ui::start();
    ui.session("tok_a");
    fleet_replies(&ui);
    script(&ui, vec![db_status()]);
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "api"}]}),
    );
    ui.reply(&m("Repositories/List"), json!({}));
    let out = ui.run(
        r#"
import { fakeDocument, elements, settle } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const app = startApp({ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000 });
  await settle();
  const names = () => elements(doc.root).map((e) => e.attributes["name"]).filter((n) => String(n).startsWith("page-"));
  const listed = names();
  app.navigate("rollout", { service: "api", version: "1.3.2" });
  await settle();
  const package_ = elements(doc.root).some((e) => e.attributes["name"] === "manifest-toggle");
  app.navigate("holds", {});
  await settle();
  const current = elements(doc.root).filter((e) => e.attributes["class"] === "current").map((e) => e.attributes["name"]);
  return { listed, package_, current };
}"#,
    );
    let listed: Vec<&str> = out["listed"]
        .as_array()
        .expect("names")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for gone in ["page-release", "page-rollout", "page-holds", "page-package"] {
        assert!(!listed.contains(&gone), "{gone} is listed: {listed:?}");
    }
    assert!(listed.contains(&"page-packages"), "{listed:?}");
    assert_eq!(out["package_"], true);
    assert_eq!(out["current"], json!(["page-packages"]));
    assert_eq!(
        bodies_of(&ui, "Releases/GetRelease"),
        vec![json!({"service": "api", "version": "1.3.2", "hostGroup": "db"})]
    );
    assert_eq!(
        bodies_of(&ui, "Releases/Status"),
        vec![json!({}), json!({"service": "api"})]
    );
}

#[test]
fn a_package_the_page_is_not_given_asks_for_the_packages_page_and_makes_no_call() {
    // Catches: a page opened with no package that reads the status of every service, or shows an
    // empty page with no way on.
    let mut ui = Ui::start();
    let out = package(
        &mut ui,
        "{}",
        "[]",
        r#"await click(tree, "open-packages"); return seen.navigations;"#,
    );
    assert_eq!(out["out"], json!([{"name": "packages", "params": {}}]));
    assert!(ui.methods().is_empty(), "{:?}", ui.methods());
}

#[test]
fn a_refused_status_is_shown_as_its_code_and_reason_alone() {
    // Catches: a page that shows nothing, or the text of the refusal, when the status is refused.
    let mut ui = Ui::start();
    ui.refuse(&m("Releases/Status"), 500, "INTERNAL", "secret-token-xyz");
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return null;");
    assert_eq!(shown(&out), "Package api INTERNAL");
}

#[test]
fn package_detail_calls_are_existing_operator_methods() {
    // Catches: a call the package page makes that no page of the app made before it (compatibility,
    // B8: the page adds no endpoint, field or message; `git diff main -- crates/hostkeeper-proto`
    // is empty for this change): every method of the page is also called by a page that exists on
    // main, so a coordinator of the previous release serves all of them.
    let file = UiFile::named("calls.json").expect("calls.json is held");
    let on_main: BTreeSet<&str> = [
        "rollout",
        "release",
        "holds",
        "host",
        "packages",
        "fleet",
        "hostgroups",
    ]
    .into_iter()
    .collect();
    let mut used = BTreeSet::new();
    for call in listed_calls(file.text).expect("calls.json parses") {
        // The trusted publishers of a package are the one addition of the page: three methods of
        // `Services` that the coordinator serves from the release that has them, and the section
        // that calls them shows its own error when it is served by an older one.
        let added = call.method.contains("TrustedPublisher");
        if call.pages.iter().any(|p| p == "package") && !added {
            assert!(
                call.pages.iter().any(|p| on_main.contains(p.as_str())),
                "{} is called by the package page only",
                call.method
            );
            used.insert(call.function.clone());
        }
    }
    assert_eq!(
        used,
        BTreeSet::from(
            [
                "hosts.list",
                "releases.getRelease",
                "releases.status",
                "repositories.list",
                "services.hold",
                "services.list",
                "services.unhold",
            ]
            .map(str::to_owned)
        )
    );
}

/// The entries of a package, as `Services.ListTrustedPublishers` answers: one made in the console
/// and one the coordinator imported from its allow-list file, with what that file pinned.
fn publishers_reply() -> Value {
    json!({"publishers": [
        {"repository": "komira-ai/api", "workflow": ".github/workflows/release.yml",
         "ref": "refs/heads/main", "environment": "production"},
        {"repository": "komira-ai/api", "ref": "refs/heads/main", "repositoryId": "1440761269",
         "calledWorkflows": ["komira-ai/hostkeeper/.github/workflows/set-desired.yml@2c767d1"],
         "limitedToHostGroups": true, "hostGroups": ["lab"]},
    ]})
}

#[test]
fn the_page_lists_the_trusted_publishers_of_the_package_with_what_the_file_pinned() {
    // Catches: a section that leaves out an entry's repository, workflow file, branch or
    // environment, shows an imported entry (no workflow file) as if any repository were trusted or
    // hides what the allow-list file pinned on it, or shows the full ref instead of the branch.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.reply(&m("Services/ListTrustedPublishers"), publishers_reply());
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return rows(tree);");
    assert_eq!(
        bodies_of(&ui, "Services/ListTrustedPublishers"),
        vec![json!({"service": "api"})]
    );
    let rows = out["out"].as_array().expect("rows");
    let row = |first: &str, env: &str| {
        rows.iter()
            .find(|r| r[0] == first && r[3] == env && r.as_array().map_or(0, Vec::len) == 6)
            .unwrap_or_else(|| panic!("no entry {first} {env}: {rows:?}"))
            .clone()
    };
    assert_eq!(
        row("komira-ai/api", "production"),
        json!([
            "komira-ai/api",
            ".github/workflows/release.yml",
            "main",
            "production",
            "-",
            "Remove"
        ])
    );
    assert_eq!(
        row("komira-ai/api", "-"),
        json!([
            "komira-ai/api",
            "any workflow of the repository",
            "main",
            "-",
            "repository id 1440761269; calls komira-ai/hostkeeper/.github/workflows/set-desired.yml@2c767d1; deploys to host group lab",
            "Remove"
        ])
    );
    assert!(
        shown(&out).contains("Trusted publishers"),
        "{}",
        shown(&out)
    );
}

#[test]
fn a_package_with_no_entry_says_no_run_is_trusted_and_a_failed_read_is_shown() {
    // Catches: an empty list drawn as a table with no rows and no words, and a refused read that
    // is shown as an empty list (the operator would take it for "nothing trusted").
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.reply(&m("Services/ListTrustedPublishers"), json!({}));
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return null;");
    assert!(
        shown(&out).contains("No run is trusted to set api."),
        "{}",
        shown(&out)
    );

    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.refuse(
        &m("Services/ListTrustedPublishers"),
        404,
        "NOT_FOUND",
        "unknown_service",
    );
    let out = package(&mut ui, r#"{ package: "api" }"#, "[]", "return null;");
    assert!(
        shown(&out).contains("Trusted publishers: NOT_FOUND unknown_service"),
        "{}",
        shown(&out)
    );
    assert!(
        !shown(&out).contains("No run is trusted"),
        "{}",
        shown(&out)
    );
}

#[test]
fn adding_an_entry_sends_the_four_fields_and_a_wrong_field_is_answered_before_it_sends() {
    // Catches: a form that sends a malformed repository, workflow file or branch, one that sends
    // an empty environment, one that does not default the branch to main, and one that does not
    // clear itself and draw the entries the API answers.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.reply(&m("Services/ListTrustedPublishers"), json!({}));
    ui.reply(
        &m("Services/PutTrustedPublisher"),
        json!({"publishers": [{"repository": "komira-ai/api", "workflow": ".github/workflows/release.yml",
                               "ref": "refs/heads/main"}]}),
    );
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[]",
        r#"
type(tree, "publisher-repository", "no-slash");
await click(tree, "add-publisher");
type(tree, "publisher-repository", "komira-ai/api");
type(tree, "publisher-workflow", "release.yml");
await click(tree, "add-publisher");
type(tree, "publisher-workflow", ".github/workflows/release.yml");
type(tree, "publisher-ref", "a b");
await click(tree, "add-publisher");
type(tree, "publisher-ref", "");
type(tree, "publisher-environment", "x".repeat(101));
await click(tree, "add-publisher");
type(tree, "publisher-environment", "");
await click(tree, "add-publisher");
return seen.reports;"#,
    );
    assert_eq!(
        out["out"],
        json!([
            "Name the repository as owner/name, for example komira-ai/tierstore.",
            "Name the workflow file under .github/workflows, for example .github/workflows/release.yml.",
            "Name the branch, for example main (or a full ref such as refs/tags/v1).",
            "An environment is printable text of at most 100 characters.",
            "komira-ai/api .github/workflows/release.yml may now set api."
        ])
    );
    assert_eq!(
        bodies_of(&ui, "Services/PutTrustedPublisher"),
        vec![json!({"service": "api", "repository": "komira-ai/api",
                    "workflow": ".github/workflows/release.yml", "ref": "main",
                    "reason": "set in the console"})]
    );
}

#[test]
fn an_environment_is_sent_only_when_typed_and_a_refused_add_is_shown() {
    // Catches: an environment dropped from the request, and a refusal that is not reported.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.reply(&m("Services/ListTrustedPublishers"), json!({}));
    ui.refuse(
        &m("Services/PutTrustedPublisher"),
        404,
        "NOT_FOUND",
        "unknown_service",
    );
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[]",
        r#"
type(tree, "publisher-repository", "komira-ai/api");
type(tree, "publisher-workflow", ".github/workflows/release.yml");
type(tree, "publisher-ref", "refs/tags/v1");
type(tree, "publisher-environment", "production");
await click(tree, "add-publisher");
return seen.reports;"#,
    );
    assert_eq!(out["out"], json!(["api: NOT_FOUND unknown_service"]));
    assert_eq!(
        bodies_of(&ui, "Services/PutTrustedPublisher"),
        vec![json!({"service": "api", "repository": "komira-ai/api",
                    "workflow": ".github/workflows/release.yml", "ref": "refs/tags/v1",
                    "environment": "production", "reason": "set in the console"})]
    );
}

#[test]
fn removing_an_entry_asks_first_and_sends_the_entry_as_listed() {
    // Catches: a remove without the operator's yes, one that names the entry by anything but the
    // four fields as listed (an imported entry has no workflow file), and a refusal not reported.
    let mut ui = Ui::start();
    script(&ui, vec![db_status()]);
    ui.reply(&m("Services/ListTrustedPublishers"), publishers_reply());
    ui.reply(
        &m("Services/RemoveTrustedPublisher"),
        json!({"publishers": []}),
    );
    ui.refuse(
        &m("Services/RemoveTrustedPublisher"),
        404,
        "NOT_FOUND",
        "publisher_unknown",
    );
    let out = package(
        &mut ui,
        r#"{ package: "api" }"#,
        "[false, true, true]",
        r#"
await click(tree, "remove-publisher", 1);
await click(tree, "remove-publisher", 1);
await click(tree, "remove-publisher", 0);
return seen.reports;"#,
    );
    assert_eq!(
        out["out"],
        json!([
            "Not removed.",
            "Removed komira-ai/api any workflow.",
            "api: NOT_FOUND publisher_unknown"
        ])
    );
    assert_eq!(
        bodies_of(&ui, "Services/RemoveTrustedPublisher"),
        vec![
            json!({"service": "api", "repository": "komira-ai/api", "workflow": "",
                   "ref": "refs/heads/main", "environment": "", "reason": "removed in the console"}),
            json!({"service": "api", "repository": "komira-ai/api",
                   "workflow": ".github/workflows/release.yml", "ref": "refs/heads/main",
                   "environment": "production", "reason": "removed in the console"}),
        ]
    );
}
