//! The Host page's "Host group" form, run in the JavaScript engine against the fake gateway of
//! `support`: the dropdown of the groups held, `all` among them as an ordinary name, with the host's own selected, the move
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
fn the_host_page_offers_every_group_with_the_hosts_own_selected_and_unassigned_for_none() {
    // Proves: the Host group dropdown lists a blank (no group) and every group of every page of
    // HostGroups.List (a group named `all` is an ordinary choice, and a host in it has it
    // selected), selects the host's own group, and selects the blank, labelled Unassigned, for a
    // host in no group.
    // Catches: a group name still filtered out or still read as every host's group, a list that
    // stops at its first page, or a dropdown that does not show where the host is.
    let mut ui = start("web");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return { choices: choices(tree, "host-group"), chosen: chosen(tree, "host-group") };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"choices": ["Unassigned", "all", "storage", "web"], "chosen": "web"})
    );
    assert_shows(&out, &["Host group", "Move"]);
    let mut ui = start("");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return { choices: choices(tree, "host-group"), chosen: chosen(tree, "host-group") };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"choices": ["Unassigned", "all", "storage", "web"], "chosen": ""})
    );
}

#[test]
fn choosing_another_group_calls_move_after_a_confirm_and_refreshes() {
    // Proves: choosing `storage` and pressing Move asks first, sends HostGroups.Move with the
    // node id, the group and the default reason (the form has no reason field), reports the group
    // the host left, and refreshes once; a declined confirm sends nothing.
    // Catches: a move sent without the yes, a body with a wrong field, or no refresh.
    let mut ui = start("web");
    ui.reply(&m("HostGroups/Move"), json!({"fromHostGroup": "web"}));
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[false, true]",
        r#"
choose(tree, "host-group", "storage");
await click(tree, "move-host");
const declined = seen.refreshes;
await click(tree, "move-host");
const once = seen.refreshes;
return { declined, once, fields: nodes(tree).filter((n) => ["move-reason", "reason"].includes((n.attrs || {})["name"])).length };
"#,
    ));
    assert_eq!(
        moves(&ui),
        [json!({"nodeId": "web-2", "hostGroup": "storage", "reason": "moved in the console"}),]
    );
    assert_eq!(out["out"], json!({"declined": 0, "once": 1, "fields": 0}));
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Move web-2 to host group storage? It is ordered that group's packages at its next heartbeat.",
            "Move web-2 to host group storage? It is ordered that group's packages at its next heartbeat."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Not moved.", "Moved web-2 from web to storage."])
    );
}

#[test]
fn choosing_the_blank_moves_the_host_to_no_group() {
    // Proves: the blank choice sends HostGroups.Move with an empty hostGroup after a question
    // that says the host is then Unassigned and ordered no package, and reports "Unassigned".
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
            "Remove web-2 from host group web? It is then Unassigned: it is ordered no package and keeps what it runs."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Moved web-2 from web to Unassigned."])
    );
}

#[test]
fn a_move_to_where_the_host_already_is_sends_nothing() {
    // Proves: pressing Move with the host's own group selected says so (in a group, and in none)
    // and sends no Move.
    // Catches: a move that the coordinator would refuse `same_host_group`.
    let mut ui = start("web");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"
await click(tree, "move-host");
return seen.reports;
"#,
    ));
    assert_eq!(out["out"], json!(["web-2 is already in host group web."]));
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
    assert_eq!(out["out"], json!(["web-2 is Unassigned."]));
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
    assert_eq!(out["out"], json!(["Unassigned", "web"]));
    assert_shows(
        &out,
        &[
            "Host groups: UNAVAILABLE store_failed",
            "Host web-2",
            "Retire",
        ],
    );
}

#[test]
fn a_host_in_no_group_is_shown_unassigned_and_one_in_a_group_named_all_is_shown_in_it() {
    // Proves: the header's Group line says "Unassigned" for a host with no group, names the group
    // for a host in one (a group named `all` included: it has no special meaning), and the Host
    // group panel says a host is in one group or Unassigned and is then ordered no package.
    // Catches: "no group" wording, a group named all hidden as if it were no group, or the old
    // text that every host is in all.
    let mut ui = start("");
    let out = ui.run(&page_driver("host", NODE, "[]", "return null;"));
    assert_shows(
        &out,
        &[
            "Group",
            "Unassigned",
            "A host is in one host group or is Unassigned.",
            "an Unassigned host is ordered no package and keeps what it runs.",
        ],
    );
    let text = shown_text(&out);
    assert!(!text.contains("Every host is in all"), "{text}");
    assert!(!text.contains("no group"), "{text}");

    let mut ui = start("all");
    let out = ui.run(&page_driver(
        "host",
        NODE,
        "[]",
        r#"return chosen(tree, "host-group");"#,
    ));
    assert_eq!(
        out["out"],
        json!("all"),
        "a host in all is in the group all"
    );
}

fn shown_text(out: &Value) -> String {
    support::pages::shown(out).to_owned()
}
