//! The Host groups page, run in the JavaScript engine against the fake gateway of `support`: the
//! list, a group's detail and its hosts, the refusals, the move, the hosts in no group, and the
//! form that creates or changes a group's ordered package list.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

/// The page's other calls: the hosts it offers to move, none of them in no group, and the
/// registered packages its form offers, in name order.
fn hosts_replies(ui: &mut Ui) {
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "hello"}, {"service": "hostkeeper"}, {"service": "kbf"},
                            {"service": "tierstore"}]}),
    );
}

/// Driver helpers for the package picker: the checkbox labels of a tree, the last tree the page
/// showed, and the ticking of the box of a package by name.
const PICK: &str = r#"
const labels = (t) => nodes(t).filter((n) => n.tag === "label" && n.attrs.class === "check").map((n) => text(n).trim());
const last = () => seen.shown[seen.shown.length - 1];
const pick = (t, name, on) => tick(t, "pick-package", on, labels(t).indexOf(name));
"#;

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
    hosts_replies(&mut ui);
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
    hosts_replies(&mut ui);
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
    hosts_replies(&mut ui);
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Host groups: PERMISSION_DENIED forbidden",
            "Create or change a host group",
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
    hosts_replies(&mut ui);
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
    hosts_replies(&mut ui);
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
    hosts_replies(&mut ui);
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

#[test]
fn move_without_a_reason_or_a_name_or_confirm_sends_nothing() {
    // Proves: an empty reason, a reason with a control character, a missing host or group, and a
    // declined confirm each report and send no HostGroups.Move.
    // Catches: a move sent without a reason, or one that ignores the operator's no.
    let mut ui = Ui::start();
    hosts_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[false]",
        r#"
await click(tree, "move");
type(tree, "move-reason", "tab\there");
await click(tree, "move");
type(tree, "move-reason", "rebuilt");
await click(tree, "move");
type(tree, "move-host", "store-9");
choose(tree, "move-group", "storage");
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
    hosts_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    ui.reply(&m("HostGroups/Move"), json!({}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[true, true]",
        r#"
type(tree, "move-host", " store-9 ");
choose(tree, "move-group", "storage");
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
    hosts_replies(&mut ui);
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.refuse(
        &m("HostGroups/Move"),
        404,
        "NOT_FOUND",
        "unknown_host_group",
    );
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[true]",
        r#"
type(tree, "move-host", "store-9");
choose(tree, "move-group", "nope");
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
    hosts_replies(&mut ui);
    ui.reply(&m("HostGroups/Put"), json!({"generation": "1"}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        &format!(
            r#"{PICK}
type(tree, "group-name", " web ");
pick(tree, "hostkeeper", true);
pick(tree, "tierstore", true);
pick(tree, "hello", true);
await click(tree, "put-group");
return text(named(last(), "install-order")[0]);
"#
        ),
    ));
    assert_eq!(
        out["out"],
        "Installed in this order: hostkeeper, tierstore, hello."
    );
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
    hosts_replies(&mut ui);
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        &format!(
            r#"{PICK}
await click(tree, "put-group");
type(tree, "group-name", "all");
pick(tree, "tierstore", true);
await click(tree, "put-group");
pick(tree, "hostkeeper", true);
type(tree, "group-reason", "tab\there");
await click(tree, "put-group");
return seen.reports;
"#
        ),
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
    // Proves: Edit loads the group's name and packages (ticked, in the group's order) into the form; Save of the
    // changed list sends the generation List gave, the group's policy as held and the typed
    // reason; taking a package out asks first, and a no sends nothing.
    // Catches: a change that drops the policy, names no generation, or uninstalls a package from
    // the group's hosts without asking.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list_single_with_policy());
    hosts_replies(&mut ui);
    ui.reply(&m("HostGroups/Put"), json!({"generation": "5"}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[false, true]",
        &format!(
            r#"{PICK}
await click(tree, "edit-group");
const loaded = last();
const before = [named(loaded, "group-name")[0].attrs.value, named(loaded, "pick-package").map((b) => b.attrs.checked), text(named(loaded, "install-order")[0])];
pick(loaded, "kbf", false);
pick(loaded, "hello", true);
await click(loaded, "put-group");
type(loaded, "group-reason", " one package fewer ");
await click(loaded, "put-group");
return {{ before, questions: seen.questions }};
"#
        ),
    ));
    assert_eq!(
        out["out"]["before"],
        json!([
            "storage",
            [false, true, true, false],
            "Installed in this order: hostkeeper, kbf."
        ])
    );
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
    hosts_replies(&mut ui);
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
        &format!(
            r#"{PICK}
await click(tree, "edit-group");
const loaded = last();
pick(loaded, "hello", true);
await click(loaded, "put-group");
return null;
"#
        ),
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
fn hosts_in_no_group_are_listed_to_pick_and_the_group_is_chosen_from_the_groups_held() {
    // Proves: the hosts Hosts.List shows in no group are offered, a host in a group is not, a
    // pick fills the Move form's host, the group dropdown lists the groups held but the built-in
    // `all` after a blank, the table marks `all` as built in, and Move sends the pick and the
    // chosen group.
    // Catches: a host approved with no group that cannot be found to move, a host already in a
    // group offered, or a group dropdown that lists anything but the groups held.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list());
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [
            {"group": {"name": "all", "packages": ["hostkeeper"]}, "generation": "1", "hosts": 3},
            {"group": {"name": "zeta"}, "generation": "1"}]}),
    );
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [{"nodeId": "new-1"}, {"nodeId": "web-1", "hostGroup": "web"},
                         {"nodeId": "new-2", "hostGroup": ""}]}),
    );
    ui.reply(&m("HostGroups/Move"), json!({}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[true]",
        r#"
const offered = nodes(tree).filter((n) => n.attrs["name"] === "pick-host").map((n) => text(n));
const groups = choices(tree, "move-group");
const all = rows(tree).find((r) => r[0].startsWith("all"))[0];
await click(tree, "pick-host", 1);
const picked = seen.shown[seen.shown.length - 1];
choose(picked, "move-group", "web");
type(picked, "move-reason", "placed");
await click(picked, "move");
return { offered, groups, all, host: named(picked, "move-host")[0].attrs.value };
"#,
    ));
    assert_eq!(
        out["out"],
        json!({"offered": ["new-1", "new-2"], "groups": ["Choose a group", "storage", "web", "zeta"],
               "all": "all (built in: every host)", "host": "new-2"})
    );
    let moves: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Move"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        moves,
        [json!({"nodeId": "new-2", "hostGroup": "web", "reason": "placed"})]
    );
}

#[test]
fn a_refused_host_list_shows_its_reason_and_the_move_form_stays() {
    // Proves: a refused Hosts.List says so in the page, offers no host to pick, and leaves the
    // Move form to type in.
    // Catches: a Host groups page that fails whole because its host list did.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.refuse(&m("Hosts/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Hosts: UNAVAILABLE store_failed",
            "Move a host",
            "Host group",
        ],
    );
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
    hosts_replies(&mut ui);
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
fn the_form_offers_the_registered_packages_as_checkboxes_and_no_text_field() {
    // Proves: the form's packages are one checkbox for each package Services.List names, in its
    // order, none ticked for a new group, and there is no field to type a package into.
    // Catches: a free-text packages field that takes a name nobody registered, or a picker that
    // offers more or fewer packages than the registry holds.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    hosts_replies(&mut ui);
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        &format!(
            r#"{PICK}
const typed = nodes(tree).filter((n) => n.attrs["name"] === "group-packages").length;
return {{ labels: labels(tree), ticked: named(tree, "pick-package").map((b) => b.attrs.checked), typed }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({"labels": ["hello", "hostkeeper", "kbf", "tierstore"],
               "ticked": [false, false, false, false], "typed": 0})
    );
    assert_shows(&out, &["None chosen."]);
}

#[test]
fn an_empty_registry_links_to_the_packages_page_and_a_refused_list_is_shown() {
    // Proves: with no registered package the form says so and its link opens the Packages page
    // (ctx.navigate); a refused Services.List is shown with its code and reason and the rest of
    // the form still renders.
    // Catches: an empty picker with no way forward, or a Host groups page that fails whole because
    // the package list did.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(&m("Services/List"), json!({}));
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        r#"
await click(tree, "open-packages");
return null;
"#,
    ));
    assert_shows(&out, &["No package is registered yet."]);
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "packages", "params": {}}])
    );

    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), json!({}));
    ui.reply(&m("Hosts/List"), json!({}));
    ui.refuse(&m("Services/List"), 403, "PERMISSION_DENIED", "forbidden");
    let out = ui.run(&page_driver("hostgroups", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Packages: PERMISSION_DENIED forbidden",
            "Create or change a host group",
        ],
    );
}

#[test]
fn a_package_the_group_lists_that_is_not_registered_stays_in_the_picker() {
    // Proves: a package a held group lists that Services.List does not name is still a checkbox,
    // ticked, so saving the group does not drop it.
    // Catches: a Save that silently uninstalls a package from a group's hosts.
    let mut ui = Ui::start();
    ui.reply(&m("HostGroups/List"), storage_list_single_with_policy());
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(
        &m("Services/List"),
        json!({"services": [{"service": "hostkeeper"}]}),
    );
    let out = ui.run(&page_driver(
        "hostgroups",
        "{}",
        "[]",
        &format!(
            r#"{PICK}
await click(tree, "edit-group");
const loaded = last();
return {{ labels: labels(loaded), ticked: named(loaded, "pick-package").map((b) => b.attrs.checked) }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({"labels": ["hostkeeper", "kbf"], "ticked": [true, true]})
    );
}
