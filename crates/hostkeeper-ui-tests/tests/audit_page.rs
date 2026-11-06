//! The Audit page, run in the JavaScript engine against the fake gateway of `support`: the
//! filters and the page it asks `Audit.List` for, and the rows it shows.

mod support;

use serde_json::json;
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

#[test]
fn audit_page_passes_its_filters_to_the_call() {
    // Catches: a filter dropped from the request, an empty filter sent, and paging that does not
    // go on from the last entry.
    let mut ui = Ui::start();
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [{"seq": "6", "by": "ACTOR_OPERATOR", "action": "hold", "service": "api",
                            "nodeId": "web-1", "reason": "kernel patch"}],
               "nextSeq": "6"}),
    );
    ui.reply(&m("Audit/List"), json!({}));
    let out = ui.run(&page_driver(
        "audit",
        r#"{ actor: "operator", action: "hold", service: "api", nodeId: "web-1", sinceSeq: "5" }"#,
        "[]",
        r#"
await click(tree, "next-page");
await click(tree, "first-page");
type(tree, "service", " ");
type(tree, "action", "unhold");
choose(tree, "actor", "ci");
await click(tree, "show");
const { ctx: other } = recorder(ctx.api, {}, []);
return text(await render(other));
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Audit/List"),
                json!({"sinceSeq": "5", "limit": 100, "actor": "operator", "action": "hold",
                                      "service": "api", "nodeId": "web-1"})
            ),
            (m("Audit/List"), json!({"sinceSeq": "0", "limit": 100})),
        ]
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "audit", "params": {"actor": "operator", "action": "hold", "service": "api", "nodeId": "web-1", "sinceSeq": "6"}},
            {"name": "audit", "params": {"actor": "operator", "action": "hold", "service": "api", "nodeId": "web-1"}},
            {"name": "audit", "params": {"actor": "ci", "action": "unhold", "nodeId": "web-1"}},
        ])
    );
    assert_shows(
        &out,
        &["6", "operator", "hold", "api", "web-1", "kernel patch"],
    );
    assert!(
        out["out"]
            .as_str()
            .expect("the second page")
            .contains("No entry.")
    );
}

#[test]
fn audit_rows_show_time_and_who_and_the_who_filter_and_newest_are_passed() {
    // Catches: a row without the entry's time or the name it was made under, a time that is not
    // the entry's, the Who filter left out of the request, and the newest page not asked for or
    // asked for again from itself.
    let mut ui = Ui::start();
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [{"seq": "7", "atUnix": "1800000000", "by": "ACTOR_OPERATOR",
                            "byName": "alice", "action": "hold"}],
               "nextSeq": "7"}),
    );
    ui.reply(&m("Audit/List"), json!({}));
    let out = ui.run(&page_driver(
        "audit",
        r#"{ byName: "alice", sinceSeq: "5" }"#,
        "[]",
        r#"
const rows = nodes(tree).filter((n) => n.tag === "tr").map((r) => r.children.map(text).join("|"));
await click(tree, "newest-page");
type(tree, "by-name", " bob ");
await click(tree, "show");
const { ctx: other } = recorder(ctx.api, { newest: "1" }, []);
const newest = await render(other);
const names = nodes(newest).map((n) => n.attrs["name"]);
return { rows, again: names.includes("newest-page"), first: names.includes("first-page") };
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (
                m("Audit/List"),
                json!({"sinceSeq": "5", "limit": 100, "byName": "alice"})
            ),
            (
                m("Audit/List"),
                json!({"sinceSeq": "0", "limit": 100, "newest": true})
            ),
        ]
    );
    assert_eq!(
        out["out"]["rows"],
        json!([
            "Seq|Time|By|Who|Action|Service|Host|Reason",
            "7|2027-01-15 08:00:00 UTC|operator|alice|hold|-|-|-",
        ])
    );
    assert_eq!(out["out"]["again"], json!(false));
    assert_eq!(out["out"]["first"], json!(true));
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "audit", "params": {"byName": "alice", "newest": "1"}},
            {"name": "audit", "params": {"byName": "bob"}},
        ])
    );
}
