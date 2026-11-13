//! What an operator does with services in the web app: the Rollout page's assignments, its
//! Set desired, Assign, Unassign and minimum version, and the Host page's assigned services and
//! cancelled uninstalls, run in the JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m};

const MARKUP: &str = "<img src=x onerror=alert(1)>";

/// Like `page_driver`, with the queries these tests use in scope too.
fn driver(page: &str, params: &str, answers: &str, then: &str) -> String {
    format!(
        r#"
import {{ api, recorder, text, named, type, tick, click, nodes, rows, cell, mounted, domTexts }} from "../test/kit.js";
import {{ render }} from "./pages/{page}.js";
export async function main() {{
  const {{ ctx, seen }} = recorder(api("operator-token"), {params}, {answers});
  const tree = await render(ctx);
  const shown = text(tree);
  const sections = nodes(tree).filter((n) => n.attrs["class"] === "service");
  const out = await (async () => {{ {then} }})();
  return {{ text: shown, seen, out: out === undefined ? null : out }};
}}"#
    )
}

/// A release index as typed in the page: inner line breaks and a final newline, which its digest
/// covers.
const INDEX: &str = "{\n  \"service\": \"api\"\n}\n";

fn digest() -> String {
    "ab".repeat(32)
}

/// Rollout's Status: `api` with CI's limits and a package digest, and `web` with no desired state.
fn status() -> Value {
    json!({"services": [
        {"service": "api",
         "desired": {"version": "1.4.0", "maxUpdating": {"web": 1, "db": 0}, "minUp": {"web": 2},
                     "packageDigest": "ff".repeat(32)}},
        {"service": "web"},
    ]})
}

fn run_rollout(ui: &mut Ui, then: &str, answers: &str) -> Value {
    ui.reply(&m("Releases/Status"), status());
    ui.reply(&m("Services/List"), json!({"services": []}));
    ui.run(&driver("rollout", "{}", answers, then))
}

fn reports(out: &Value) -> Value {
    out["seen"]["reports"].clone()
}

#[test]
fn rollout_shows_assigned_roles_operator_assignments_hosts_and_uninstalling() {
    // Catches: a Rollout page that leaves out a service's roles, an operator assignment (or shows
    // a removal as an assignment), the hosts count or the hosts being uninstalled; that matches
    // the entry of another service; or that calls anything but Status then List with empty bodies.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status());
    ui.reply(
        &m("Services/List"),
        json!({"services": [
            {"service": "web", "hosts": ["w-1"], "roles": ["edge"]},
            {"service": "api", "desiredVersion": "1.4.0", "roles": ["web", "db"],
             "hosts": ["web-1", "web-2", "db-1"],
             "assignments": [{"role": "web"}, {"host": "db-1"}, {"role": "old", "removed": true},
                             {"host": "gone-1", "removed": true}],
             "assigned": [{"nodeId": "web-1", "role": "web"}, {"nodeId": "web-2", "role": "web"}],
             "uninstalling": ["db-9", "db-8"]},
        ]}),
    );
    let out = ui.run(&driver(
        "rollout",
        "{}",
        "[]",
        r#"return sections.map((s) => ["Desired", "Assigned roles", "Operator assignments", "Hosts assigned", "Uninstalling"].map((l) => cell(s, l)));"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            [
                "1.4.0",
                "web, db",
                "role web, host db-1, removed from role old, removed from host gone-1",
                "2",
                "db-9, db-8"
            ],
            ["none set", "edge", "-", "1", "-"],
        ])
    );
    assert_eq!(
        bodies(&ui),
        [
            (m("Releases/Status"), json!({})),
            (m("Services/List"), json!({}))
        ]
    );
}

#[test]
fn the_set_desired_form_names_a_release_index_and_its_digest_not_a_package_digest() {
    // Catches: a form that still asks for one package digest (which fails every host of another
    // platform), that leaves out the index or its digest, or that shows either under another label.
    let mut ui = Ui::start();
    let out = run_rollout(&mut ui, "return null;", "[]");
    let shown = out["text"].as_str().expect("the page text");
    assert!(
        shown.contains("Release index digest (64 hex characters)"),
        "{shown}"
    );
    assert!(
        shown.contains("Release index (the text of index.json)"),
        "{shown}"
    );
    assert!(!shown.contains("Package digest"), "{shown}");
}

#[test]
fn rollout_shows_no_assignments_for_a_service_the_list_does_not_name() {
    // Catches: a page that fails on a service with no entry, or shows another service's rows.
    let mut ui = Ui::start();
    let out = run_rollout(
        &mut ui,
        r#"return sections.map((s) => ["Assigned roles", "Operator assignments", "Hosts assigned", "Uninstalling"].map((l) => cell(s, l)));"#,
        "[]",
    );
    assert_eq!(
        out["out"],
        json!([["none", "-", "0", "-"], ["none", "-", "0", "-"]])
    );
}

#[test]
fn rollout_still_shows_status_when_the_service_list_fails() {
    // Catches: a Rollout page that fails whole, or shows no reason, when only Services.List
    // fails; the forms must still be there.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status());
    ui.refuse(&m("Services/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&driver(
        "rollout",
        "{}",
        "[]",
        r#"return [sections.length, cell(sections[0], "Desired"), named(tree, "set-desired").length,
                   rows(sections[0]).some((r) => r[0] === "Assigned roles")];"#,
    ));
    assert_eq!(out["out"], json!([2, "1.4.0", 2, false]));
    let text = out["text"].as_str().expect("text");
    assert_eq!(
        text.matches("UNAVAILABLE store_failed").count(),
        2,
        "{text}"
    );
    assert_eq!(ui.requests().len(), 2);
}

#[test]
fn set_desired_asks_first_and_names_the_version() {
    // Catches: a Set desired that sends with a blank version, that sends after the operator
    // declined, or that does not name the service and version in its question.
    let mut ui = Ui::start();
    let out = run_rollout(
        &mut ui,
        r#"
await click(tree, "set-desired");
type(tree, "desired-version", "  1.5.0  ");
await click(tree, "set-desired");
return null;"#,
        "[false]",
    );
    assert_eq!(
        reports(&out),
        json!(["Name the version.", "Desired version not changed."])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Set the desired version of api to 1.5.0? Its hosts are moved there stage by stage, by the service's update plan."
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 0);
    assert_eq!(ui.methods(), [m("Releases/Status"), m("Services/List")]);
}

#[test]
fn set_desired_sends_the_version_with_ci_limits_and_the_typed_release_index() {
    // Catches: a body that drops CI's limits, copies the old package digest, sends a blank
    // digest or index, sends a package digest, changes the index text (its trailing newline, its
    // inner line breaks) or sends ifGeneration or writeId; limits invented for a service with
    // none; a missing refresh; a dropped-roles notice that is not reported.
    let mut ui = Ui::start();
    ui.reply(
        &m("Releases/SetDesired"),
        json!({"desired": {"version": "1.5.0"}, "droppedRoles": ["cache", "db"]}),
    );
    ui.reply(
        &m("Releases/SetDesired"),
        json!({"desired": {"version": "1.6.0"}}),
    );
    ui.reply(
        &m("Releases/SetDesired"),
        json!({"desired": {"version": "0.1.0"}}),
    );
    let out = run_rollout(
        &mut ui,
        &format!(
            r#"
type(tree, "desired-version", "1.5.0");
type(tree, "desired-index-digest", "  {}  ");
type(tree, "desired-index", {INDEX:?});
await click(tree, "set-desired", 0);
type(tree, "desired-version", "1.6.0");
type(tree, "desired-index-digest", "   ");
type(tree, "desired-index", " \n ");
await click(tree, "set-desired", 0);
type(tree, "desired-version", "0.1.0", 1);
await click(tree, "set-desired", 1);
return null;"#,
            digest()
        ),
        "[true, true, true]",
    );
    let limits = json!({"maxUpdating": {"web": 1, "db": 0}, "minUp": {"web": 2}});
    let desired = |version: &str| {
        let mut d = limits.clone();
        d["version"] = json!(version);
        d
    };
    assert_eq!(
        bodies(&ui)[2..],
        [
            (
                m("Releases/SetDesired"),
                json!({"service": "api", "desired": desired("1.5.0"),
                       "releaseIndexDigest": digest(), "releaseIndex": INDEX})
            ),
            (
                m("Releases/SetDesired"),
                json!({"service": "api", "desired": desired("1.6.0")})
            ),
            (
                m("Releases/SetDesired"),
                json!({"service": "web", "desired": {"version": "0.1.0"}})
            ),
        ]
    );
    assert_eq!(
        reports(&out),
        json!([
            "Set the desired version of api to 1.5.0. Limits dropped for roles: cache, db.",
            "Set the desired version of api to 1.6.0.",
            "Set the desired version of web to 0.1.0.",
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 3);
}

#[test]
fn set_desired_reports_the_apis_refusal_as_is() {
    // Catches: a refusal that is reworded or hidden, and a page that refreshes as if the write
    // had landed.
    let mut ui = Ui::start();
    ui.refuse(
        &m("Releases/SetDesired"),
        412,
        "FAILED_PRECONDITION",
        "below_min_version",
    );
    ui.refuse(
        &m("Releases/SetDesired"),
        401,
        "UNAUTHENTICATED",
        "unauthorized",
    );
    let out = run_rollout(
        &mut ui,
        r#"
type(tree, "desired-version", "0.0.1");
await click(tree, "set-desired");
await click(tree, "set-desired");
return null;"#,
        "[true, true]",
    );
    assert_eq!(
        reports(&out),
        json!([
            "FAILED_PRECONDITION below_min_version",
            "UNAUTHENTICATED unauthorized"
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 0);
}

#[test]
fn assign_sends_the_service_and_role_and_shows_a_refusal() {
    // Catches: an Assign with a blank role, one that sends a nodeId, a role not trimmed, a
    // refusal that is reworded, and a missing refresh after success.
    let mut ui = Ui::start();
    ui.reply(&m("Services/Assign"), json!({}));
    ui.refuse(&m("Services/Assign"), 404, "NOT_FOUND", "unknown_role");
    let out = run_rollout(
        &mut ui,
        r#"
await click(tree, "assign");
type(tree, "assign-role", " farm ");
await click(tree, "assign");
await click(tree, "assign");
return null;"#,
        "[]",
    );
    assert_eq!(
        bodies(&ui)[2..],
        [
            (
                m("Services/Assign"),
                json!({"service": "api", "role": "farm"})
            ),
            (
                m("Services/Assign"),
                json!({"service": "api", "role": "farm"})
            ),
        ]
    );
    assert_eq!(
        reports(&out),
        json!([
            "Name the role.",
            "Assigned api to role farm.",
            "NOT_FOUND unknown_role"
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn unassign_asks_only_when_it_uninstalls() {
    // Catches: an Unassign that asks when nothing is uninstalled, sends after the operator
    // declined, loses the uninstall flag, or reports the wrong outcome.
    let mut ui = Ui::start();
    ui.reply(&m("Services/Unassign"), json!({}));
    ui.reply(&m("Services/Unassign"), json!({}));
    let out = run_rollout(
        &mut ui,
        r#"
await click(tree, "unassign");
type(tree, "assign-role", "farm");
await click(tree, "unassign");
tick(tree, "unassign-uninstall", true);
await click(tree, "unassign");
await click(tree, "unassign");
return null;"#,
        "[false, true]",
    );
    assert_eq!(
        bodies(&ui)[2..],
        [
            (
                m("Services/Unassign"),
                json!({"service": "api", "role": "farm", "uninstall": false})
            ),
            (
                m("Services/Unassign"),
                json!({"service": "api", "role": "farm", "uninstall": true})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Unassign api from role farm and uninstall it from each host that loses its last assignment?",
            "Unassign api from role farm and uninstall it from each host that loses its last assignment?",
        ])
    );
    assert_eq!(
        reports(&out),
        json!([
            "Name the role.",
            "Unassigned api from role farm; its hosts keep running it, unmanaged.",
            "Not unassigned.",
            "Unassigned api from role farm, uninstalling it where no assignment is left.",
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 2);
}

#[test]
fn unassign_shows_a_refusal_as_is() {
    // Catches: a refusal that is reworded, and a refresh after a refused unassign.
    let mut ui = Ui::start();
    ui.refuse(
        &m("Services/Unassign"),
        412,
        "FAILED_PRECONDITION",
        "not_assigned",
    );
    let out = run_rollout(
        &mut ui,
        r#"
type(tree, "assign-role", "farm");
await click(tree, "unassign");
return null;"#,
        "[]",
    );
    assert_eq!(reports(&out), json!(["FAILED_PRECONDITION not_assigned"]));
    assert_eq!(out["seen"]["refreshes"], 0);
}

#[test]
fn a_double_click_on_assign_sends_one_call() {
    // Catches: two Services.Assign calls from a double click.
    let mut ui = Ui::start();
    ui.reply(&m("Services/Assign"), json!({}));
    run_rollout(
        &mut ui,
        r#"
type(tree, "assign-role", "farm");
const button = named(tree, "assign")[0];
await Promise.all([button.on.click(), button.on.click()]);
return null;"#,
        "[]",
    );
    let assigns = ui
        .methods()
        .iter()
        .filter(|x| **x == m("Services/Assign"))
        .count();
    assert_eq!(assigns, 1);
}

fn host_entries() -> Value {
    json!({"services": [
        {"service": "api", "desiredVersion": "1.4.0", "assignments": [{"role": "web"}],
         "assigned": [{"nodeId": "web-2", "role": "web", "installedVersion": "1.3.0", "minVersion": "1.2.0"},
                      {"nodeId": "web-3", "role": "web", "installedVersion": "1.4.0"}]},
        {"service": "cache", "desiredVersion": "2.0.0", "assignments": [{"host": "web-2"}],
         "assigned": [{"nodeId": "web-2", "role": "web", "installedVersion": "2.0.0"}]},
        {"service": "hello", "desiredVersion": "1.0.0",
         "assigned": [{"nodeId": "web-2", "role": "web"}]},
        {"service": "tool",
         "assigned": [{"nodeId": "web-2", "role": "web", "installedVersion": "0.9.0"}]},
        {"service": "old", "desiredVersion": "1.0.0",
         "assignments": [{"host": "web-2", "removed": true}, {"role": "web"}],
         "assigned": [{"nodeId": "web-2", "role": "web", "installedVersion": "1.0.0"}]},
        {"service": "gone", "desiredVersion": "3.0.0", "uninstalling": ["web-2"]},
        {"service": "elsewhere", "desiredVersion": "1.0.0",
         "assigned": [{"nodeId": "web-9", "role": "db", "installedVersion": "1.0.0"}],
         "uninstalling": ["web-9"]},
    ]})
}

const ASSIGNED: &str =
    r#"return rows(nodes(tree).filter((n) => n.attrs["class"] === "assigned")[0]);"#;

#[test]
fn host_lists_its_assigned_services_with_installed_against_desired() {
    // Catches: a Host page that lists another host's services, mistakes a removed host
    // assignment for a direct one, or gets the state wrong (at desired, behind, not installed,
    // no desired version, uninstalling); or that sends anything but {nodeId} then {}.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.reply(&m("Services/List"), host_entries());
    ui.reply(&m("Audit/List"), json!({}));
    let out = ui.run(&driver("host", r#"{ nodeId: "web-2" }"#, "[]", ASSIGNED));
    assert_eq!(
        out["out"],
        json!([
            [
                "Service",
                "Through",
                "Installed",
                "Desired",
                "State",
                "Minimum"
            ],
            ["api", "role web", "1.3.0", "1.4.0", "behind", "1.2.0"],
            ["cache", "this host", "2.0.0", "2.0.0", "at desired", "-"],
            [
                "hello",
                "role web",
                "not installed",
                "1.0.0",
                "not installed",
                "-"
            ],
            ["tool", "role web", "0.9.0", "none set", "-", "-"],
            ["old", "role web", "1.0.0", "1.0.0", "at desired", "-"],
            ["gone", "-", "-", "3.0.0", "uninstalling", "-"],
        ])
    );
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-2"})),
            (m("Services/List"), json!({})),
            (m("Audit/List"), json!({"nodeId": "web-2"})),
            (m("HostGroups/List"), json!({}))
        ]
    );
}

#[test]
fn host_says_when_no_service_is_assigned() {
    // Catches: an empty table, or no words, for a host nothing is assigned to.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": "web-7"}}));
    ui.reply(&m("Services/List"), host_entries());
    let out = ui.run(&driver("host", r#"{ nodeId: "web-7" }"#, "[]", ""));
    let text = out["text"].as_str().expect("text");
    assert!(
        text.contains("No service is assigned to this host."),
        "{text}"
    );
}

#[test]
fn host_shows_assignments_when_hosts_get_is_unimplemented() {
    // Catches: a Host page that stops at the Hosts.Get refusal and never asks for the assigned
    // services, or drops the refusal or the heading.
    let mut ui = Ui::start();
    ui.reply(&m("Services/List"), host_entries());
    let out = ui.run(&driver("host", r#"{ nodeId: "web-2" }"#, "[]", ASSIGNED));
    let text = out["text"].as_str().expect("text");
    assert!(text.contains("Host web-2"), "{text}");
    assert!(text.contains("UNIMPLEMENTED unimplemented"), "{text}");
    assert_eq!(out["out"].as_array().expect("rows").len(), 7);
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-2"})),
            (m("Services/List"), json!({})),
            (m("Audit/List"), json!({"nodeId": "web-2"}))
        ]
    );
}

#[test]
fn host_shows_the_service_list_refusal_and_keeps_the_rest() {
    // Catches: a Host page that fails whole when Services.List fails, or hides its refusal.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.refuse(&m("Services/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"return cell(tree, "Role");"#,
    ));
    assert_eq!(out["out"], "web");
    let text = out["text"].as_str().expect("text");
    assert!(text.contains("UNAVAILABLE store_failed"), "{text}");
    assert!(!text.contains("No service is assigned"), "{text}");
}

#[test]
fn text_from_the_api_in_assignments_stays_text() {
    // Catches: a Rollout or Host page that builds markup from a role, a host, a version or a
    // service name that the API sent: each must become a text node, with no element made from it.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status());
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "api", "roles": [MARKUP], "assignments": [{"host": MARKUP}],
                             "uninstalling": [MARKUP]}]}),
    );
    ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": "web-2"}}));
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "api", "desiredVersion": MARKUP,
                             "assigned": [{"nodeId": "web-2", "role": MARKUP, "installedVersion": MARKUP}]}]}),
    );
    let out = ui.run(
        r#"
import { api, recorder, mounted, domTexts } from "../test/kit.js";
import * as rollout from "./pages/rollout.js";
import * as host from "./pages/host.js";
export async function main() {
  const client = api("operator-token");
  const out = [];
  for (const [page, params] of [[rollout, {}], [host, { nodeId: "web-2" }]]) {
    const { ctx } = recorder(client, params, []);
    const doc = mounted(await page.render(ctx));
    out.push({
      tags: Array.from(new Set(doc.created.map((e) => e.tagName))),
      attributes: doc.created.flatMap((e) => Object.keys(e.attributes)),
      attributeValues: doc.created.flatMap((e) => Object.values(e.attributes)),
      texts: domTexts(doc.root),
    });
  }
  return out;
}"#,
    );
    for (index, expected) in [3, 3].into_iter().enumerate() {
        let found = &out[index];
        let tags: Vec<&str> = found["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!tags.contains(&"IMG"), "{index}: {tags:?}");
        assert!(
            found["attributes"]
                .as_array()
                .expect("names")
                .iter()
                .all(|a| !a.as_str().unwrap_or("").starts_with("on")),
            "{index}"
        );
        assert!(!found["attributeValues"].to_string().contains("onerror"));
        let carrying = found["texts"]
            .as_array()
            .expect("texts")
            .iter()
            .filter(|t| t.as_str().unwrap_or("").contains(MARKUP))
            .count();
        assert!(
            carrying >= expected,
            "{index}: {carrying} text nodes carry the markup"
        );
    }
}

/// Rollout's Services.List: `api` with a role's minimum and a host's, `web` with none.
fn floors(role_min: &str) -> Value {
    json!({"services": [
        {"service": "api", "roles": ["web"],
         "minVersions": [{"role": "web", "version": role_min}, {"host": "db-1", "version": "1.1.0"}]},
        {"service": "web"},
    ]})
}

/// Renders Rollout over `floors("1.2.0")`, runs `then`, renders it again over `floors(after)`,
/// and returns each render's minimum versions per service.
fn run_floors(ui: &mut Ui, then: &str, answers: &str, after: &str) -> Value {
    for role_min in ["1.2.0", after] {
        ui.reply(&m("Releases/Status"), status());
        ui.reply(&m("Services/List"), floors(role_min));
    }
    ui.run(&driver(
        "rollout",
        "{}",
        answers,
        &format!(
            r#"
const floors = (t) => nodes(t).filter((n) => n.attrs["class"] === "service").map((s) => cell(s, "Minimum versions"));
const before = floors(tree);
{then}
const again = await render(ctx);
return [before, floors(again)];"#
        ),
    ))
}

#[test]
fn set_min_version_sends_the_role_scope_and_shows_the_new_floor() {
    // Catches: a Rollout page with no Set min version, one that sends with a blank role or
    // version, sends after the operator declined, sends a host scope or an untrimmed value, calls
    // another method, misses the refresh, or does not show each scope's minimum after it.
    let mut ui = Ui::start();
    ui.reply(&m("Services/SetMinVersion"), json!({}));
    let out = run_floors(
        &mut ui,
        r#"
await click(tree, "set-min-version");
type(tree, "assign-role", " web ");
await click(tree, "set-min-version");
type(tree, "min-version", " 1.3.0 ");
await click(tree, "set-min-version");
await click(tree, "set-min-version");"#,
        "[false, true]",
        "1.3.0",
    );
    assert_eq!(
        reports(&out),
        json!([
            "Name the role and the minimum version.",
            "Name the role and the minimum version.",
            "Minimum version not changed.",
            "Set the minimum version of api for role web to 1.3.0.",
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Set the minimum version of api for role web to 1.3.0? No host of the role is ordered below it, rollbacks included.",
            "Set the minimum version of api for role web to 1.3.0? No host of the role is ordered below it, rollbacks included.",
        ])
    );
    assert_eq!(
        bodies(&ui)[2..],
        [
            (
                m("Services/SetMinVersion"),
                json!({"service": "api", "minVersion": {"role": "web", "version": "1.3.0"}})
            ),
            (m("Releases/Status"), json!({})),
            (m("Services/List"), json!({})),
        ]
    );
    assert_eq!(out["seen"]["refreshes"], 1);
    assert_eq!(
        out["out"],
        json!([
            ["role web 1.2.0, host db-1 1.1.0", "-"],
            ["role web 1.3.0, host db-1 1.1.0", "-"],
        ])
    );
}

#[test]
fn set_min_version_shows_a_refusal_and_keeps_the_floor() {
    // Catches: a refusal (a minimum above the desired version, a role the coordinator does not
    // know) that is reworded or hidden, and a page that refreshes or shows a new floor as if the
    // write had landed.
    let mut ui = Ui::start();
    ui.refuse(
        &m("Services/SetMinVersion"),
        412,
        "FAILED_PRECONDITION",
        "below_min_version",
    );
    ui.refuse(
        &m("Services/SetMinVersion"),
        404,
        "NOT_FOUND",
        "unknown_role",
    );
    let out = run_floors(
        &mut ui,
        r#"
type(tree, "assign-role", "web");
type(tree, "min-version", "9.0.0");
await click(tree, "set-min-version");
type(tree, "assign-role", "nope");
await click(tree, "set-min-version");"#,
        "[true, true]",
        "1.2.0",
    );
    assert_eq!(
        reports(&out),
        json!([
            "FAILED_PRECONDITION below_min_version",
            "NOT_FOUND unknown_role"
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 0);
    assert_eq!(
        out["out"],
        json!([
            ["role web 1.2.0, host db-1 1.1.0", "-"],
            ["role web 1.2.0, host db-1 1.1.0", "-"],
        ])
    );
}

/// An audit entry the coordinator wrote for `service` on `node`.
fn ended(seq: u64, action: &str, service: &str, node: &str) -> Value {
    json!({"seq": seq.to_string(), "by": "ACTOR_COORDINATOR", "action": action,
           "service": service, "nodeId": node})
}

#[test]
fn host_marks_a_service_whose_last_uninstall_was_cancelled() {
    // Catches: a Host page that marks a service whose last uninstall finished, misses one
    // cancelled after an earlier finished one, takes another host's entry or another action for
    // a cancel, changes the row of a service still being uninstalled, or reads only the first
    // page of the audit (the `uninstalled` of hello is on the second), or pages without the
    // previous page's nextSeq.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.reply(&m("Services/List"), host_entries());
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [
            ended(1, "uninstall_cancelled", "api", "web-2"),
            ended(2, "uninstalled", "cache", "web-2"),
            ended(3, "uninstall_cancelled", "cache", "web-2"),
            ended(4, "uninstall_cancelled", "hello", "web-2"),
        ], "nextSeq": "4"}),
    );
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [
            ended(5, "uninstalled", "hello", "web-2"),
            ended(6, "uninstall_cancelled", "tool", "web-9"),
            ended(7, "unassign", "old", "web-2"),
            ended(8, "uninstall_cancelled", "gone", "web-2"),
            ended(9, "assign", "api", "web-2"),
        ], "nextSeq": "9"}),
    );
    ui.reply(&m("Audit/List"), json!({"nextSeq": "9"}));
    let out = ui.run(&driver("host", r#"{ nodeId: "web-2" }"#, "[]", ASSIGNED));
    assert_eq!(
        out["out"],
        json!([
            [
                "Service",
                "Through",
                "Installed",
                "Desired",
                "State",
                "Minimum"
            ],
            [
                "api",
                "role web",
                "1.3.0",
                "1.4.0",
                "behind; uninstall cancelled",
                "1.2.0"
            ],
            [
                "cache",
                "this host",
                "2.0.0",
                "2.0.0",
                "at desired; uninstall cancelled",
                "-"
            ],
            [
                "hello",
                "role web",
                "not installed",
                "1.0.0",
                "not installed",
                "-"
            ],
            ["tool", "role web", "0.9.0", "none set", "-", "-"],
            ["old", "role web", "1.0.0", "1.0.0", "at desired", "-"],
            ["gone", "-", "-", "3.0.0", "uninstalling", "-"],
        ])
    );
    let text = out["text"].as_str().expect("text");
    assert!(!text.contains("could not be read"), "{text}");
    assert_eq!(
        bodies(&ui)[2..],
        [
            (m("Audit/List"), json!({"nodeId": "web-2"})),
            (m("Audit/List"), json!({"nodeId": "web-2", "sinceSeq": "4"})),
            (m("Audit/List"), json!({"nodeId": "web-2", "sinceSeq": "9"})),
            (m("HostGroups/List"), json!({})),
        ]
    );
}

#[test]
fn host_stops_paging_the_audit_when_next_seq_does_not_move() {
    // Catches: a Host page that asks for the same page again and again when the coordinator
    // answers entries with the nextSeq it was given, instead of stopping after that page.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.reply(&m("Services/List"), host_entries());
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [ended(3, "uninstall_cancelled", "api", "web-2")], "nextSeq": "3"}),
    );
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [ended(3, "uninstall_cancelled", "api", "web-2")], "nextSeq": "3"}),
    );
    let out = ui.run(&driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"return rows(nodes(tree).filter((n) => n.attrs["class"] === "assigned")[0])[1][4];"#,
    ));
    assert_eq!(out["out"], "behind; uninstall cancelled");
    assert_eq!(
        bodies(&ui)[2..],
        [
            (m("Audit/List"), json!({"nodeId": "web-2"})),
            (m("Audit/List"), json!({"nodeId": "web-2", "sinceSeq": "3"})),
            (m("HostGroups/List"), json!({})),
        ]
    );
}

#[test]
fn host_shows_why_cancelled_uninstalls_could_not_be_read() {
    // Catches: a Host page that fails whole, drops its assigned services or hides the refusal
    // when only Audit.List fails; and one that marks a service with no audit to go on.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.reply(&m("Services/List"), host_entries());
    ui.refuse(&m("Audit/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[]",
        r#"const section = nodes(tree).filter((n) => n.attrs["class"] === "assigned")[0];
return [rows(section).map((r) => r[4]), nodes(section).filter((n) => n.attrs["class"] === "note").map(text)];"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            [
                "State",
                "behind",
                "at desired",
                "not installed",
                "-",
                "at desired",
                "uninstalling"
            ],
            ["Cancelled uninstalls could not be read: UNAVAILABLE store_failed"]
        ])
    );
}

#[test]
fn host_reads_no_audit_when_nothing_is_assigned_or_the_list_fails() {
    // Catches: a Host page that asks Audit.List for a host no service is assigned to (it can
    // have no cancelled uninstall to show), or after Services.List failed.
    for (node, listing) in [("web-7", true), ("web-2", false)] {
        let mut ui = Ui::start();
        ui.reply(&m("Hosts/Get"), json!({"host": {"nodeId": node}}));
        if listing {
            ui.reply(&m("Services/List"), host_entries());
        } else {
            ui.refuse(&m("Services/List"), 503, "UNAVAILABLE", "store_failed");
        }
        ui.run(&driver(
            "host",
            &format!("{{ nodeId: {node:?} }}"),
            "[]",
            "",
        ));
        assert_eq!(
            ui.methods(),
            [m("Hosts/Get"), m("Services/List"), m("HostGroups/List")],
            "{node}"
        );
    }
}
