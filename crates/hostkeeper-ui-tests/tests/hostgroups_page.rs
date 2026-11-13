//! The Host groups page, run in the JavaScript engine against the fake gateway of `support`: the
//! list, a group's detail and its hosts, the refusals, the removal of a host from its group, the
//! absence of any form that moves a host (that is the Host page's), and the form that creates or
//! changes a group's ordered package list.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

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
fn the_page_lists_every_page_of_host_groups_with_their_summary() {
    // Proves: the list follows nextPageToken, and a row shows the name, packages, the policy keys
    // that are set, generation, host count and desired versions.
    // Catches: a list that stops at the first page, or a row that shows a default as a value.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list());
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [{"group": {"name": "zeta", "packages": ["hostkeeper"]},
                               "generation": "2", "hosts": 1,
                               "packages": [{"package": "hostkeeper", "desiredVersion": "1.0.0"}]}]}),
    );
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", "return rows(tree);"));
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
            "hostkeeper 0.9.0, kbf none set",
            "Edit"
        ])
    );
    assert!(rows.contains(&json!(["web", "-", "default", "1", "0", "-", "Edit"])));
    assert!(rows.contains(&json!([
        "zeta",
        "hostkeeper",
        "default",
        "2",
        "1",
        "hostkeeper 1.0.0",
        "Edit"
    ])));
}

#[test]
fn an_empty_list_says_no_host_group_is_held_yet() {
    // Proves: an empty list is said in words, and the form to create a group is there.
    // Catches: an empty table, or no way to create the first group.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "No host group is held yet.",
            "Create or change a host group",
        ],
    );
}

#[test]
fn a_refused_list_shows_an_alert_and_the_rest_of_the_page() {
    // Proves: a refused HostGroups.List shows its code and reason, and the forms still render.
    // Catches: a Host groups page that fails whole because its list did.
    let mut ui = Ui::start();
    ui.refuse(&m("HostGroups/List"), 403, "PERMISSION_DENIED", "forbidden");
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Host groups: PERMISSION_DENIED forbidden",
            "Create or change a host group",
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
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.reply(&m("HostGroups/Get"), detail_reply(None));
    let out = ui.run(&page_driver(
        "hostgroups",
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
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.reply(&m("HostGroups/Get"), detail_reply(Some("store-2")));
    let mut second = detail_reply(None);
    second["nodeIds"] = json!(["store-3"]);
    ui.reply(&m("HostGroups/Get"), second);
    ui.reply(&m("HostGroups/Get"), detail_reply(None));
    let out = ui.run(&page_driver(
        "hostgroups",
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
    ui.reply(&m("HostGroups/List"), storage_list_single());
    ui.refuse(&m("HostGroups/Get"), 404, "NOT_FOUND", "unknown_host_group");
    let out = ui.run(&page_driver(
        "hostgroups",
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

fn put_bodies(ui: &Ui) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Put"))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn a_new_group_is_put_with_its_ordered_packages_and_a_default_reason() {
    // Proves: Save of a name no group has creates it: Put carries the name, the packages in the
    // order typed (one per line, commas and spaces also divide), no generation and no policy,
    // and a default reason when none is typed; the page reports the generation and refreshes.
    // Catches: a create that names a generation, reorders or drops a package, or is refused for
    // want of a reason.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("HostGroups/Put"), json!({"generation": "1"}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        r#"
type(tree, "group-name", " web ");
type(tree, "group-packages", "hostkeeper\ntierstore, hello\n");
await click(tree, "put-group");
return null;
"#,
    ));
    assert_eq!(
        put_bodies(&ui),
        [
            json!({"hostGroup": {"name": "web", "packages": ["hostkeeper", "tierstore", "hello"]},
                "reason": "set in the console"})
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Saved host group web (generation 1)."])
    );
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn a_group_without_a_name_or_all_without_hostkeeper_or_with_a_bad_reason_is_not_put() {
    // Proves: an empty name, a list of `all` without hostkeeper and a reason with a control
    // character each report a sentence and send no Put; a specific group needs neither packages
    // nor hostkeeper.
    // Catches: a Put sent that the coordinator would refuse, or a bad reason sent.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        r#"
await click(tree, "put-group");
type(tree, "group-name", "all");
type(tree, "group-packages", "tierstore");
await click(tree, "put-group");
type(tree, "group-packages", "hostkeeper");
type(tree, "group-reason", "tab\there");
await click(tree, "put-group");
return seen.reports;
"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            "Name the host group.",
            "The packages of all must include hostkeeper.",
            "A reason has no control character (a tab or a line break, for example)."
        ])
    );
    assert_eq!(put_bodies(&ui), Vec::<Value>::new());
}

#[test]
fn changing_a_held_group_loads_it_and_puts_it_over_its_generation_keeping_its_policy() {
    // Proves: Edit loads the group's name and packages (one per line) into the form; Save of the
    // changed list sends the generation List gave, the group's policy as held and the typed
    // reason; taking a package out asks first, and a no sends nothing.
    // Catches: a change that drops the policy, names no generation, or uninstalls a package from
    // the group's hosts without asking.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list_single_with_policy());
    ui.reply(&m("HostGroups/Put"), json!({"generation": "5"}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[false, true]",
        r#"
await click(tree, "edit-group");
const loaded = seen.shown[seen.shown.length - 1];
const before = [named(loaded, "group-name")[0].attrs.value, named(loaded, "group-packages")[0].attrs.value];
type(loaded, "group-packages", "hostkeeper\nhello");
await click(loaded, "put-group");
type(loaded, "group-reason", " one package fewer ");
await click(loaded, "put-group");
return { before, questions: seen.questions };
"#,
    ));
    assert_eq!(out["out"]["before"], json!(["storage", "hostkeeper\nkbf"]));
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Change host group storage? kbf will be uninstalled from its hosts.",
            "Change host group storage? kbf will be uninstalled from its hosts."
        ])
    );
    assert_eq!(
        put_bodies(&ui),
        [
            json!({"hostGroup": {"name": "storage", "packages": ["hostkeeper", "hello"],
                              "policy": {"bakeBeats": 2}},
                "ifGeneration": "4", "reason": "one package fewer"})
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Not saved.", "Saved host group storage (generation 5)."])
    );
}

fn storage_list_single_with_policy() -> Value {
    json!({"hostGroups": [{"group": {"name": "storage", "packages": ["hostkeeper", "kbf"],
                                      "policy": {"bakeBeats": 2}}, "generation": "4", "hosts": 3}]})
}

#[test]
fn adding_a_package_puts_without_asking_and_a_refused_put_shows_its_reason_and_does_not_refresh() {
    // Proves: a list that only adds packages is sent without a question; a refusal of Put is
    // reported as its code and reason and the page is not refreshed.
    // Catches: a question for a harmless change, or a refusal swallowed.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list_single_with_policy());
    ui.refuse(
        &m("HostGroups/Put"),
        412,
        "FAILED_PRECONDITION",
        "generation_mismatch",
    );
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        r#"
await click(tree, "edit-group");
const loaded = seen.shown[seen.shown.length - 1];
type(loaded, "group-packages", "hostkeeper\nkbf\nhello");
await click(loaded, "put-group");
return null;
"#,
    ));
    assert_eq!(out["seen"]["questions"], json!([]));
    assert_eq!(
        out["seen"]["reports"],
        json!(["FAILED_PRECONDITION generation_mismatch"])
    );
    assert_eq!(out["seen"]["refreshes"], json!(0));
    assert_eq!(put_bodies(&ui).len(), 1);
}

#[test]
fn remove_from_group_asks_first_and_moves_the_host_to_an_empty_group() {
    // Proves: each host of a specific group's detail has a Remove from group button that asks
    // first, sends HostGroups.Move with an empty hostGroup and a reason, refreshes once, and
    // reports; a refused call is reported and refreshes nothing; the detail of `all` has no such
    // button, as every host stays in it.
    // Catches: a removal sent without the yes, a body that names a group, a refresh after a refusal,
    // or a button on the group every host is in.
    let mut ui = Ui::start();
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [
            {"group": {"name": "storage", "packages": ["kbf"]}, "generation": "4", "hosts": 2},
            {"group": {"name": "all", "packages": ["hostkeeper"]}, "generation": "1", "hosts": 2},
        ]}),
    );
    ui.reply(&m("HostGroups/Get"), detail_reply(None));
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "storage"}));
    ui.reply(&m("HostGroups/Move"), json!({}));
    ui.refuse(
        &m("HostGroups/Move"),
        412,
        "FAILED_PRECONDITION",
        "same_host_group",
    );
    ui.reply(
        &m("HostGroups/Get"),
        json!({"hostGroup": {"group": {"name": "all"}}, "nodeIds": ["store-1"]}),
    );
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[false, true, true, true]",
        r#"
await click(tree, "open-group", 0);
const detail = seen.shown[seen.shown.length - 1];
await click(detail, "remove-from-group", 0);
const declined = seen.refreshes;
await click(detail, "remove-from-group", 0);
const once = seen.refreshes;
await click(detail, "remove-from-group", 1);
await click(detail, "remove-from-group", 1);
await click(tree, "open-group", 1);
const all = seen.shown[seen.shown.length - 1];
return { declined, once, allButtons: nodes(all).filter((node) => (node.attrs || {})["name"] === "remove-from-group").length };
"#,
    ));
    let moves: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Move"))
        .map(|(_, body)| body)
        .collect();
    let reason = "removed from the group in the console";
    assert_eq!(
        moves,
        [
            json!({"nodeId": "store-1", "hostGroup": "", "reason": reason}),
            json!({"nodeId": "store-2", "hostGroup": "", "reason": reason}),
            json!({"nodeId": "store-2", "hostGroup": "", "reason": reason}),
        ]
    );
    assert_eq!(out["out"]["declined"], json!(0));
    assert_eq!(out["out"]["once"], json!(1));
    assert_eq!(out["out"]["allButtons"], json!(0));
    assert_eq!(out["seen"]["refreshes"], json!(2));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not removed.",
            "Removed store-1 from host group storage: it runs the packages of all alone.",
            "Removed store-2 from host group storage: it runs the packages of all alone.",
            "FAILED_PRECONDITION same_host_group",
        ])
    );
}

#[test]
fn the_page_has_no_control_that_moves_a_host_and_asks_for_no_host_list() {
    // Proves: the Host groups page shows no "Move a host" form, no host or group picker and no
    // Move button, and calls HostGroups.List alone (Hosts.List is not called).
    // Catches: a move form left on the page the owner moved it from, or a host list fetched only
    // to feed it.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list_single());
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        r#"
const names = nodes(tree).map((n) => (n.attrs || {})["name"]).filter((n) => n !== undefined);
return { move: names.filter((n) => ["move", "pick-host", "move-host", "move-group", "move-reason"].includes(n)),
         opened: names.includes("open-group") };
"#,
    ));
    assert_eq!(out["out"], json!({"move": [], "opened": true}));
    let text = out["text"].as_str().expect("text");
    assert!(!text.contains("Move a host"), "{text}");
    assert!(!text.contains("Choose a group"), "{text}");
    assert_eq!(
        bodies(&ui)
            .iter()
            .map(|(method, _)| method.clone())
            .collect::<Vec<_>>(),
        [m("HostGroups/List")]
    );
}
