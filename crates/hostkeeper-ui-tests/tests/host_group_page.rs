//! The Host page's "Host group" form, run in the JavaScript engine against the fake gateway of
//! `support`: the dropdown of the groups held but `all` with the host's own selected, the move
//! into another group, the move into none by the blank, and the refusals.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

const NODE: &str = r#"{ nodeId: "web-2" }"#;

fn groups_reply() -> Value {
    json!({"hostGroups": [
        {"group": {"name": "all", "packages": ["hostkeeper"]}, "generation": "1"},
        {"group": {"name": "storage"}, "generation": "2"},
    ], "nextPageToken": "storage"})
}

fn second_groups_reply() -> Value {
    json!({"hostGroups": [{"group": {"name": "web"}, "generation": "1"}]})
}

/// Scripts the page of `web-2`, in group `group` (none when empty), and the two pages of groups.
fn start(group: &str) -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web", "live": true, "hostGroup": group}}),
    );
    ui.reply(&m("HostGroups/List"), groups_reply());
    ui.reply(&m("HostGroups/List"), second_groups_reply());
    ui
}

fn moves(ui: &Ui) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| method == &m("HostGroups/Move"))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn the_host_page_offers_every_group_but_all_with_the_hosts_own_selected() {
    // Proves: the Host group dropdown lists a blank (no group) and every group of every page of
    // HostGroups.List except the built-in `all`, selects the host's own group, and selects the
    // blank for a host in `all` alone.
    // Catches: `all` offered as a choice, a list that stops at its first page, or a dropdown that
    // does not show where the host is.
    let mut ui = start("web");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return { choices: choices(tree, "host-group"), chosen: chosen(tree, "host-group") };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"choices": ["No group (all alone)", "storage", "web"], "chosen": "web"})
    );
    assert_shows(&out, &["Host group", "Move"]);
    let mut ui = start("all");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return { choices: choices(tree, "host-group"), chosen: chosen(tree, "host-group") };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"choices": ["No group (all alone)", "storage", "web"], "chosen": ""})
    );
}

#[test]
fn choosing_another_group_calls_move_after_a_confirm_and_refreshes() {
    // Proves: choosing `storage` and pressing Move asks first, sends HostGroups.Move with the
    // node id, the group and the trimmed reason (a default when none is typed), reports the group
    // the host left, and refreshes once; a declined confirm sends nothing.
    // Catches: a move sent without the yes, a body with a wrong field, or no refresh.
    let mut ui = start("web");
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[false, true, true]",
        r#"
choose(tree, "host-group", "storage");
await click(tree, "move-host");
const declined = seen.refreshes;
type(tree, "move-reason", " rebalancing ");
await click(tree, "move-host");
const once = seen.refreshes;
type(tree, "move-reason", "");
await click(tree, "move-host");
return { declined, once };
"#,
    ));
    assert_eq!(
        moves(&ui),
        [
            json!({"nodeId": "web-2", "hostGroup": "storage", "reason": "rebalancing"}),
            json!({"nodeId": "web-2", "hostGroup": "storage", "reason": "moved in the console"}),
        ]
    );
    assert_eq!(out["out"], json!({"declined": 0, "once": 1}));
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Move web-2 to host group storage? It is ordered that group's packages at its next heartbeat.",
            "Move web-2 to host group storage? It is ordered that group's packages at its next heartbeat.",
            "Move web-2 to host group storage? It is ordered that group's packages at its next heartbeat."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not moved.",
            "Moved web-2 from web to storage.",
            "Moved web-2 from web to storage."
        ])
    );
}

#[test]
fn choosing_the_blank_moves_the_host_to_no_group() {
    // Proves: the blank choice sends HostGroups.Move with an empty hostGroup after a question
    // that says the host then runs the packages of all alone, and reports "no group".
    // Catches: a blank that is refused on the page or sent as a group named "".
    let mut ui = start("web");
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[true]",
        r#"
choose(tree, "host-group", "");
await click(tree, "move-host");
return null;
"#,
    ));
    assert_eq!(
        moves(&ui),
        [json!({"nodeId": "web-2", "hostGroup": "", "reason": "moved in the console"})]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Remove web-2 from host group web? It then runs the packages of all alone, and what web lists and all does not is uninstalled."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Moved web-2 from web to no group."])
    );
}

#[test]
fn a_move_to_where_the_host_is_or_with_a_bad_reason_sends_nothing() {
    // Proves: pressing Move with the host's own group selected says so (in a group, and in none),
    // and a reason with a control character is reported; none of them sends a Move.
    // Catches: a move that the coordinator would refuse `same_host_group`, or a bad reason sent.
    let mut ui = start("web");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"
await click(tree, "move-host");
choose(tree, "host-group", "storage");
type(tree, "move-reason", "tab\there");
await click(tree, "move-host");
return seen.reports;
"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            "web-2 is already in host group web.",
            "A reason has no control character (a tab or a line break, for example)."
        ])
    );
    assert_eq!(moves(&ui), Vec::<Value>::new());
    let mut ui = start("");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"
await click(tree, "move-host");
return seen.reports;
"#,
    ));
    assert_eq!(out["out"], json!(["web-2 is in no host group."]));
    assert_eq!(moves(&ui), Vec::<Value>::new());
}

#[test]
fn a_refused_move_is_reported_without_a_refresh_and_a_refused_list_leaves_the_page() {
    // Proves: a refused HostGroups.Move shows its code and reason and does not refresh; a refused
    // HostGroups.List is shown inline, offers only the blank and the host's own group, and the
    // rest of the page renders.
    // Catches: a refusal swallowed, a refresh after a move that did not happen, or a Host page
    // that fails whole because the group list did.
    let mut ui = start("web");
    ui.refuse(
        &m("HostGroups/Move"),
        404,
        "NOT_FOUND",
        "unknown_host_group",
    );
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[true]",
        r#"
choose(tree, "host-group", "storage");
await click(tree, "move-host");
return null;
"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!(["NOT_FOUND unknown_host_group"])
    );
    assert_eq!(out["seen"]["refreshes"], json!(0));

    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web", "hostGroup": "web"}}),
    );
    ui.refuse(&m("HostGroups/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return choices(tree, "host-group");"#,
    ));
    assert_eq!(out["out"], json!(["No group (all alone)", "web"]));
    assert_shows(
        &out,
        &[
            "Host groups: UNAVAILABLE store_failed",
            "Host web-2",
            "Retire",
        ],
    );
}
