//! The host groups section of the Rollout page, run in the JavaScript engine against the fake
//! gateway of `support`: the list, a group's detail and its hosts, the refusals, and the move.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

fn rollout_replies(ui: &mut Ui) {
    ui.reply(&m("Releases/Status"), json!({"services": []}));
    ui.reply(&m("Services/List"), json!({}));
}

fn storage_list() -> Value {
    json!({"hostGroups": [
        {"group": {"name": "storage", "packages": ["hostkeeper", "kbf"],
                   "policy": {"stages": [{"percent": 10}, {"percent": 100, "advance": "STAGE_ADVANCE_MANUAL"}],
                              "canary": {"count": 1}, "bakeBeats": 2,
                              "rollback": "ROLLBACK_MODE_AUTOMATIC",
                              "rollbackAfterFailed": {"percent": 20, "minHosts": 2},
                              "deferBackoff": {"initialSecs": "30", "factor": 2, "maxSecs": "600"},
                              "maxUpdating": 2, "minUp": 1, "liveRollouts": 3}},
         "generation": "4", "hosts": 7,
         "packages": [{"package": "hostkeeper", "desiredVersion": "0.9.0"}, {"package": "kbf"}]},
        {"group": {"name": "web"}, "generation": "1"},
    ], "nextPageToken": "web"})
}

#[test]
fn the_rollout_page_lists_every_page_of_host_groups_with_their_summary() {
    // Proves: the list follows nextPageToken, and a row shows the name, packages, the policy keys
    // that are set, generation, host count and desired versions.
    // Catches: a list that stops at the first page, or a row that shows a default as a value.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), storage_list());
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "zeta", "packages": ["hostkeeper"]},
                               "generation": "2", "hosts": 1,
                               "packages": [{"package": "hostkeeper", "desiredVersion": "1.0.0"}]}]}),
    );
    let out = ui.run(&page_driver("rollout", "{}", "[]", "return rows(tree);"));
    assert_eq!(
        bodies(&ui)
            .iter()
            .filter(|(method, _)| method == &m("HostGroups/List"))
            .map(|(_, body)| body.clone())
            .collect::<Vec<_>>(),
        [json!({}), json!({"pageToken": "web"})]
    );
    let rows = out["out"].as_array().expect("rows");
    let storage = rows
        .iter()
        .find(|r| r[0] == "storage")
        .expect("the storage row");
    assert_eq!(
        storage,
        &json!([
            "storage",
            "hostkeeper, kbf",
            "stages 10%/100% manual, canary 1, bake 2 beats, rollback automatic, rollback after 20% failed, at least 2 hosts, defer backoff 30s x2 to 600s, max updating 2, min up 1, live rollouts 3",
            "4",
            "7",
            "hostkeeper 0.9.0, kbf none set"
        ])
    );
    assert!(rows.contains(&json!(["web", "-", "default", "1", "0", "-"])));
    assert!(rows.contains(&json!([
        "zeta",
        "hostkeeper",
        "default",
        "2",
        "1",
        "hostkeeper 1.0.0"
    ])));
}

#[test]
fn an_empty_list_says_no_host_group_is_held_yet() {
    // Proves: an empty list is said in words, with the line about hkctl.
    // Catches: an empty table, or no word on how groups come about.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    let out = ui.run(&page_driver("rollout", "{}", "[]", ""));
    assert_shows(
        &out,
        &["No host group is held yet.", "hkctl host-groups put"],
    );
}

#[test]
fn a_refused_list_shows_an_alert_and_the_rest_of_the_rollout_page() {
    // Proves: a refused HostGroups.List shows its code and reason, and the services still render.
    // Catches: a Rollout page that fails whole because its host groups did.
    let mut ui = Ui::start();
    ui.reply(
        &m("Releases/Status"),
        json!({"services": [{"service": "api", "desired": {"version": "1.4.0"}}]}),
    );
    ui.reply(&m("Services/List"), json!({}));
    ui.refuse(&m("HostGroups/List"), 403, "PERMISSION_DENIED", "forbidden");
    let out = ui.run(&page_driver("rollout", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Host groups: PERMISSION_DENIED forbidden",
            "1.4.0",
            "Move a host",
        ],
    );
    assert!(
        !out["text"]
            .as_str()
            .expect("text")
            .contains("No host group is held yet.")
    );
}

fn detail_reply(next: Option<&str>) -> Value {
    let mut reply = json!({
        "hostGroup": {"group": {"name": "storage", "packages": ["kbf"]}, "generation": "4", "hosts": 3,
                      "packages": [{"package": "kbf", "desiredVersion": "2.0.0", "minVersion": "1.5.0",
                                    "convergence": {"state": "CONVERGENCE_STATE_IN_PROGRESS",
                                                    "roles": [{"role": "storage", "atDesired": 2, "updating": 1},
                                                              {"role": "x", "failed": 1, "down": 1}]}},
                                   {"package": "hostkeeper"}]},
        "nodeIds": ["store-1", "store-2"],
    });
    if let Some(token) = next {
        reply["nextPageToken"] = json!(token);
    }
    reply
}

#[test]
fn opening_a_group_shows_its_packages_minimums_convergence_and_hosts() {
    // Proves: a group's name asks HostGroups.Get for it; the detail shows each package's desired
    // and minimum version and convergence counts, and a host links to the Host page.
    // Catches: a detail that drops the minimum version or convergence, or a host that is not a link.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.reply(&m("HostGroups/Get"), detail_reply(None));
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[]",
        r#"
await click(tree, "open-group");
const shownTree = seen.shown[seen.shown.length - 1];
await click(shownTree, "open-host", 1);
return rows(shownTree);
"#,
    ));
    assert_eq!(
        bodies(&ui).last().cloned(),
        Some((
            m("HostGroups/Get"),
            json!({"hostGroup": "storage", "limit": 50})
        ))
    );
    let rows = out["out"].as_array().expect("rows");
    assert!(rows.contains(&json!([
        "kbf",
        "2.0.0",
        "1.5.0",
        "in_progress",
        "2 at desired, 1 updating, 1 failed, 1 down"
    ])));
    assert!(rows.contains(&json!([
        "hostkeeper",
        "none set",
        "-",
        "-",
        "0 at desired, 0 updating, 0 failed, 0 down"
    ])));
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "store-2"}}])
    );
}

fn storage_list_single() -> Value {
    json!({"hostGroups": [{"group": {"name": "storage", "packages": ["kbf"]}, "generation": "4", "hosts": 3}]})
}

#[test]
fn a_groups_hosts_page_with_next_and_first_page() {
    // Proves: Next asks for the page the reply named and replaces the hosts shown; First page goes
    // back to the first; neither button shows where it does not apply.
    // Catches: a Next that repeats the first page, or a first page that offers Next with no token.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.reply(&m("HostGroups/Get"), detail_reply(Some("store-2")));
    let mut second = detail_reply(None);
    second["nodeIds"] = json!(["store-3"]);
    ui.reply(&m("HostGroups/Get"), second);
    ui.reply(&m("HostGroups/Get"), detail_reply(None));
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[]",
        r#"
await click(tree, "open-group");
const first = seen.shown[seen.shown.length - 1];
await click(first, "next-hosts");
const second = seen.shown[seen.shown.length - 1];
const lastPage = { hosts: nodes(second).filter((n) => n.attrs["name"] === "open-host").map((n) => text(n)),
                   next: nodes(second).filter((n) => n.attrs["name"] === "next-hosts").length,
                   firstButton: nodes(first).filter((n) => n.attrs["name"] === "first-hosts").length };
await click(second, "first-hosts");
return lastPage;
"#,
    ));
    let gets: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Get"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        gets,
        [
            json!({"hostGroup": "storage", "limit": 50}),
            json!({"hostGroup": "storage", "limit": 50, "pageToken": "store-2"}),
            json!({"hostGroup": "storage", "limit": 50}),
        ]
    );
    assert_eq!(
        out["out"],
        json!({"hosts": ["store-3"], "next": 0, "firstButton": 0})
    );
}

#[test]
fn a_refused_get_shows_the_group_and_its_reason_inline() {
    // Proves: a refused HostGroups.Get names the group with its code and reason, the list stays.
    // Catches: a detail click that fails silently or blanks the page.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.refuse(&m("HostGroups/Get"), 404, "NOT_FOUND", "unknown_host_group");
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[]",
        r#"
await click(tree, "open-group");
return text(seen.shown[seen.shown.length - 1]);
"#,
    ));
    let shown = out["out"].as_str().expect("text");
    assert!(
        shown.contains("storage: NOT_FOUND unknown_host_group"),
        "{shown}"
    );
    assert!(shown.contains("Host groups"), "{shown}");
}

#[test]
fn move_without_a_reason_or_a_name_or_confirm_sends_nothing() {
    // Proves: an empty reason, a reason with a control character, a missing host or group, and a
    // declined confirm each report and send no HostGroups.Move.
    // Catches: a move sent without a reason, or one that ignores the operator's no.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[false]",
        r#"
await click(tree, "move");
type(tree, "move-reason", "tab\there");
await click(tree, "move");
type(tree, "move-reason", "rebuilt");
await click(tree, "move");
type(tree, "move-host", "store-9");
type(tree, "move-group", "storage");
await click(tree, "move");
return seen.reports;
"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            "Give a reason first.",
            "A reason has no control character (a tab or a line break, for example).",
            "Name the host and the host group.",
            "Not moved."
        ])
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Move store-9 to host group storage? It is ordered that group's packages at its next heartbeat."
        ])
    );
    assert!(
        bodies(&ui)
            .iter()
            .all(|(method, _)| method != &m("HostGroups/Move"))
    );
}

#[test]
fn a_confirmed_move_sends_the_exact_body_reports_and_refreshes_once() {
    // Proves: Move sends nodeId, hostGroup and the trimmed reason, reports the group the host left,
    // and refreshes the page once; a host with no group reports "no group".
    // Catches: a body with a wrong field name, no refresh, or a report that names the wrong group.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    ui.reply(&m("HostGroups/Move"), json!({}));
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[true, true]",
        r#"
type(tree, "move-host", " store-9 ");
type(tree, "move-group", "storage");
type(tree, "move-reason", " rebuilt ");
await click(tree, "move");
const once = seen.refreshes;
await click(tree, "move");
return { once, reports: seen.reports };
"#,
    ));
    let moves: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Move"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        moves,
        [
            json!({"nodeId": "store-9", "hostGroup": "storage", "reason": "rebuilt"}),
            json!({"nodeId": "store-9", "hostGroup": "storage", "reason": "rebuilt"}),
        ]
    );
    assert_eq!(
        out["out"],
        json!({"once": 1, "reports": ["Moved store-9 from web to storage.", "Moved store-9 from no group to storage."]})
    );
    assert_eq!(out["seen"]["refreshes"], json!(2));
}

#[test]
fn a_refused_move_shows_its_code_and_reason_and_does_not_refresh() {
    // Proves: a refused HostGroups.Move is reported as its code and reason and the page stays.
    // Catches: a refusal swallowed, or a refresh after a move that did not happen.
    let mut ui = Ui::start();
    rollout_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.refuse(
        &m("HostGroups/Move"),
        404,
        "NOT_FOUND",
        "unknown_host_group",
    );
    let out = ui.run(&page_driver(
        "rollout",
        "{}",
        "[true]",
        r#"
type(tree, "move-host", "store-9");
type(tree, "move-group", "nope");
type(tree, "move-reason", "rebuilt");
await click(tree, "move");
return null;
"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!(["NOT_FOUND unknown_host_group"])
    );
    assert_eq!(out["seen"]["refreshes"], json!(0));
}
