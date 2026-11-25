//! The Xcode page (`ui/pages/xcode.js`) in the JavaScript engine against the fake gateway of
//! `support`: the waiting release, the uploads and the store with Remove and its reason, the reads
//! and their failures, and `ui/calls.json`. The Add Xcode panel is in `xcode_add_panel.rs`, the
//! Mac table in `xcode_macs.rs`.

mod support;

use hostkeeper_ui_tests::{UiFile, listed_calls};
use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m};
use support::xcode::*;

#[test]
fn the_waiting_release_is_said_with_the_macos_it_needs_and_offers_add_xcode() {
    // Asserts: with release 27.3 (27C21) waiting, the page says "Xcode 27.3 (27C21) is out and not
    // in the store; it needs macOS 27.1" and has the Add Xcode button; the first read is
    // Xcode.List, Hosts.List, HostGroups.List and Maintenance.List (the last 10 runs), and the page
    // reads again every 60 seconds.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[]",
        "return { repeats: seen.repeats.map((r) => r.seconds), add: has(tree, \"add-xcode\"), panel: has(tree, \"add-panel\") };",
    );
    assert!(
        shown(&out).contains("Xcode 27.3 (27C21) is out and not in the store; it needs macOS 27.1"),
        "{}",
        shown(&out)
    );
    assert_eq!(
        out["out"],
        json!({"repeats": [60], "add": true, "panel": false})
    );
    assert_eq!(
        bodies(&ui),
        [
            (m("Xcode/List"), json!({})),
            (m("Hosts/List"), json!({})),
            (m("HostGroups/List"), json!({})),
            (m("Maintenance/List"), json!({"limit": 10})),
        ]
    );
}

#[test]
fn a_release_that_names_no_macos_is_said_without_one() {
    // Asserts: a waiting release whose list names no oldest macOS is said without "it needs".
    let mut ui = fleet(
        store(
            vec![],
            Some(json!({"version": "27.3", "build": "27C21"})),
            vec![],
        ),
        vec![],
        vec![],
        vec![],
    );
    let out = drive(&mut ui, "[]", "return null;");
    assert!(shown(&out).contains("Xcode 27.3 (27C21) is out and not in the store"));
    assert!(!shown(&out).contains("it needs"), "{}", shown(&out));
}

#[test]
fn with_nothing_waiting_the_page_says_the_store_is_current_and_still_offers_add_xcode() {
    // Asserts: with no release waiting the page says the store holds the newest release and still
    // has the Add Xcode button (its panel shows the bare command), and an empty store and no Macs
    // are said in words.
    let mut ui = fleet(store(vec![], None, vec![]), vec![], vec![], vec![]);
    let out = drive(&mut ui, "[]", "return { add: has(tree, \"add-xcode\") };");
    assert_eq!(out["out"], json!({"add": true}));
    for words in [
        "No release waits: the store holds the newest release.",
        "The store holds no Xcode.",
        "No Mac is enrolled.",
    ] {
        assert!(shown(&out).contains(words), "{words}: {}", shown(&out));
    }
}

#[test]
fn a_coordinator_without_a_store_is_said_and_offers_no_add_xcode() {
    // Asserts: Xcode.List answering `configured` false (no `--xcode-dir`) is said, and the page
    // has no Add Xcode button even when a release waits.
    let mut ui = fleet(json!({"waiting": waiting()}), vec![], vec![], vec![]);
    let out = drive(&mut ui, "[]", "return { add: has(tree, \"add-xcode\") };");
    assert_eq!(out["out"], json!({"add": false}));
    assert!(
        shown(&out)
            .contains("This coordinator has no Xcode store: it was started without --xcode-dir.")
    );
}

#[test]
fn uploads_are_listed_with_who_started_them_and_how_far() {
    // Asserts: each partial upload is listed, without the panel, as "Xcode 27.3 (27C21), started
    // by ana: 7.31 of 11.92 GB, last write 2 minutes ago"; an upload of no writer says "-"; a
    // store with none lists no Uploads section.
    let mut ui = fleet(
        store(
            archives(false),
            None,
            vec![upload("7310000000", "ana"), upload("0", "")],
        ),
        vec![],
        vec![],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[]",
        "return nodes(tree).filter((n) => n.attrs[\"name\"] === \"upload\").map(t);",
    );
    assert_eq!(
        out["out"],
        json!([
            "Xcode 27.3 (27C21), started by ana: 7.31 of 11.92 GB, last write 2 minutes ago",
            "Xcode 27.3 (27C21), started by -: 0.00 of 11.92 GB, last write 2 minutes ago",
        ])
    );
    let mut none = fleet(store(archives(false), None, vec![]), vec![], vec![], vec![]);
    let out = drive(&mut none, "[]", "return has(tree, \"xcode-uploads\");");
    assert_eq!(out["out"], false);
}

#[test]
fn the_words_for_a_time_ago_and_a_size() {
    // Asserts: the page's words for an instant: "just now", seconds, minutes, hours and days with
    // the singular for one, and "never" for none; and for a size, decimal gigabytes with two places.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { agoWords, gigabytes } from "./xcodeplan.js";
export async function main() {
  const now = 1000000;
  return {
    ago: [0, 1, 59, 60, 119, 3599, 3600, 7200, 86400, 172800].map((s) => agoWords(now, now - s)),
    future: agoWords(now, now + 5),
    never: [agoWords(now, 0), agoWords(now, "x")],
    sizes: [gigabytes("11920000000"), gigabytes(0), gigabytes(undefined)],
  };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "ago": ["just now", "1 second ago", "59 seconds ago", "1 minute ago", "1 minute ago", "59 minutes ago",
                    "1 hour ago", "2 hours ago", "1 day ago", "2 days ago"],
            "future": "just now",
            "never": ["never", "never"],
            "sizes": ["11.92 GB", "0.00 GB", "0.00 GB"],
        })
    );
}

#[test]
fn the_store_lists_each_archive_with_its_size_uploader_time_and_the_macs_that_have_it() {
    // Asserts: each archive's row shows version, build, size in GB, who uploaded it, when (UTC),
    // and how many Macs have it installed in its own directory with its build (a foreign copy and
    // another build do not count), and the keep and the largest upload are said.
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![
            mac(
                "m-1",
                "g",
                &[
                    ("27.2", "27B52", true, false),
                    ("27.1", "27A10", false, false),
                ],
                &[],
            ),
            mac("m-2", "g", &[("27.2", "27B52", true, true)], &[]),
            mac(
                "m-3",
                "g",
                &[
                    ("27.2", "27B99", true, false),
                    ("27.1", "27A10", false, false),
                ],
                &[],
            ),
        ],
        vec![group("g", 2, None)],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"return named(tree, "archive").map((row) => row.children.slice(0, 6).map(t));"#,
    );
    assert_eq!(
        out["out"],
        json!([
            [
                "27.2",
                "27B52",
                "11.92 GB",
                "ana",
                "2027-01-15 05:13:20 UTC",
                "1 Mac"
            ],
            [
                "27.1",
                "27A10",
                "11.92 GB",
                "ana",
                "2027-01-15 05:13:20 UTC",
                "2 Macs"
            ],
        ])
    );
    assert!(
        shown(&out).contains("The store keeps the newest 2 releases, and any a Mac reports installed. An upload may be at most 21.47 GB."),
        "{}",
        shown(&out)
    );
}

#[test]
fn remove_says_which_macs_still_have_it_due_asks_once_and_sends_the_reason() {
    // Asserts: the row of 27.2 says "Still due on m-1." (the Mac that lacks it) before Remove;
    // Remove asks one question that names that Mac; with the reason "  superseded " typed, it
    // calls Xcode.Remove with the SHA-256 and the trimmed reason "superseded", reads Xcode.List
    // again, says "Removed Xcode 27.2 (27B52)." and the archive is gone from the page.
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![
            mac("m-1", "g", &[("27.1", "27A10", true, false)], &[]),
            mac(
                "m-2",
                "g",
                &[
                    ("27.2", "27B52", true, false),
                    ("27.1", "27A10", false, false),
                ],
                &[],
            ),
        ],
        vec![group("g", 2, None)],
        vec![],
    );
    ui.reply(&m("Xcode/Remove"), json!({"entry": {}}));
    ui.reply(
        &m("Xcode/List"),
        store(vec![archive(B, "27.1", "27A10")], None, vec![]),
    );
    let out = drive(
        &mut ui,
        "[true]",
        r#"
const still = named(tree, "still-due").map(t);
type(tree, "remove-reason", "  superseded ", 0);
await click(tree, "remove-archive", 0);
return { still, left: named(cur(), "archive").map((row) => t(row.children[0])) };"#,
    );
    assert_eq!(
        out["out"]["still"],
        json!(["Still due on m-1.", "Due on no Mac."])
    );
    assert_eq!(out["out"]["left"], json!(["27.1"]));
    assert_eq!(
        out["seen"]["questions"],
        json!(["Remove Xcode 27.2 (27B52) from the store? It is still due on m-1."])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Removed Xcode 27.2 (27B52)."])
    );
    let sent: Vec<(String, Value)> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Xcode/Remove"))
        .collect();
    assert_eq!(
        sent,
        [(
            m("Xcode/Remove"),
            json!({"sha256": A, "reason": "superseded"})
        )]
    );
}

#[test]
fn remove_of_an_archive_due_on_no_mac_says_so_in_its_question() {
    // Asserts: removing 27.1, which no Mac has due, asks "Remove Xcode 27.1 (27A10) from the
    // store? No Mac has it due."
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![mac(
            "m-2",
            "g",
            &[
                ("27.2", "27B52", true, false),
                ("27.1", "27A10", false, false),
            ],
            &[],
        )],
        vec![group("g", 2, None)],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[false]",
        r#"
type(tree, "remove-reason", "old", 1);
await click(tree, "remove-archive", 1);
return null;"#,
    );
    assert_eq!(
        out["seen"]["questions"],
        json!(["Remove Xcode 27.1 (27A10) from the store? No Mac has it due."])
    );
}

#[test]
fn remove_without_a_reason_or_with_a_bad_one_or_declined_sends_nothing() {
    // Asserts: Remove with no reason says "Give a reason for the removal." and asks nothing; a
    // reason with a control character says why it is refused and asks nothing; a reason typed and
    // the question declined calls nothing; none of the three calls Xcode.Remove.
    let mut ui = plain();
    let out = drive(
        &mut ui,
        "[false]",
        r#"
await click(tree, "remove-archive", 0);
type(tree, "remove-reason", "a\tb", 0);
await click(tree, "remove-archive", 0);
type(tree, "remove-reason", "fine", 0);
await click(tree, "remove-archive", 0);
return null;"#,
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason for the removal.",
            "A reason has no control character (a tab or a line break, for example)."
        ])
    );
    assert_eq!(
        out["seen"]["questions"]
            .as_array()
            .expect("questions")
            .len(),
        1
    );
    assert!(
        bodies(&ui)
            .iter()
            .all(|(method, _)| *method != m("Xcode/Remove"))
    );
}

#[test]
fn a_refused_remove_is_reported_with_its_code_and_token() {
    // Asserts: Xcode.Remove refused (`FAILED_PRECONDITION partial_locked`) reports exactly that
    // and leaves the archive listed.
    let mut ui = plain();
    ui.refuse(
        &m("Xcode/Remove"),
        400,
        "FAILED_PRECONDITION",
        "partial_locked",
    );
    let out = drive(
        &mut ui,
        "[true]",
        r#"
type(tree, "remove-reason", "gone", 0);
await click(tree, "remove-archive", 0);
return named(cur(), "archive").length;"#,
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["FAILED_PRECONDITION partial_locked"])
    );
    assert_eq!(out["out"], 2);
}

#[test]
fn a_read_that_fails_at_the_start_is_the_page_and_one_that_fails_again_keeps_the_last_answer() {
    // Asserts: Xcode.List refused when the page opens shows the refusal (code and token) and no
    // tables; a later read that fails says "The page could not be read again (UNAVAILABLE down):
    // this is the last answer." and keeps the Macs and the archives; the next read that works
    // takes the warning away and shows what changed.
    let mut ui = Ui::start();
    ui.refuse(&m("Xcode/List"), 403, "PERMISSION_DENIED", "forbidden");
    let out = drive(&mut ui, "[]", "return null;");
    assert!(
        shown(&out).contains("PERMISSION_DENIED forbidden"),
        "{}",
        shown(&out)
    );
    assert!(!shown(&out).contains("The Macs"));

    let mut ui = plain();
    ui.refuse(&m("Hosts/List"), 503, "UNAVAILABLE", "down");
    ui.reply(&m("Xcode/List"), store(archives(false), None, vec![]));
    ui.reply(&m("Xcode/List"), store(archives(false), None, vec![]));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [mac("m-9", "mac-build", &[], &[])]}),
    );
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [group("mac-build", 2, None)]}),
    );
    ui.reply(&m("Maintenance/List"), json!({}));
    ui.reply(&m("Maintenance/List"), json!({}));
    let out = drive(
        &mut ui,
        "[]",
        r#"
await seen.repeats[0].run();
const failed = { warning: t(nodes(cur()).find((n) => n.attrs["role"] === "alert")), macs: macTable(cur()).map(({ name }) => name), archives: named(cur(), "archive").length };
await seen.repeats[0].run();
const mended = { alerts: nodes(cur()).filter((n) => n.attrs["role"] === "alert").length, macs: macTable(cur()).map(({ name }) => name), waiting: t(named(cur(), "xcode-waiting")[0]).startsWith("No release waits") };
return { failed, mended };"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "failed": {
                "warning": "The page could not be read again (UNAVAILABLE down): this is the last answer.",
                "macs": ["m-1", "m-2", "m-3"],
                "archives": 2,
            },
            "mended": {"alerts": 0, "macs": ["m-9"], "waiting": true},
        })
    );
}

#[test]
fn the_hosts_and_the_groups_are_read_to_the_last_page() {
    // Asserts: Hosts.List and HostGroups.List are read page after page (the second request names
    // the first reply's token), so a Mac on the second page, and a group on the second page, count.
    let ui = Ui::start();
    ui.reply(&m("Xcode/List"), store(archives(false), None, vec![]));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [mac("m-1", "g1", &[], &[])], "nextPageToken": "t1"}),
    );
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [mac("m-2", "g2", &[], &[])]}),
    );
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [group("g1", 2, None)], "nextPageToken": "u1"}),
    );
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [group("g2", 0, None)]}),
    );
    ui.reply(&m("Maintenance/List"), json!({}));
    let mut ui = ui;
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map(({ name, keeps }) => [name, keeps]);",
    );
    assert_eq!(
        out["out"],
        json!([
            ["m-1", "keep 2, default latest"],
            ["m-2", "keep 0, default latest"]
        ])
    );
    let sent = bodies(&ui);
    let hosts: Vec<&Value> = sent
        .iter()
        .filter(|(method, _)| *method == m("Hosts/List"))
        .map(|(_, body)| body)
        .collect();
    let groups: Vec<&Value> = sent
        .iter()
        .filter(|(method, _)| *method == m("HostGroups/List"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(hosts, [&json!({}), &json!({"pageToken": "t1"})]);
    assert_eq!(groups, [&json!({}), &json!({"pageToken": "u1"})]);
}

#[test]
fn the_calls_json_lists_the_pages_calls_for_the_page() {
    // Asserts: `ui/calls.json` lists the page "xcode" for xcode.list, xcode.remove, hosts.list,
    // hostGroups.list, maintenance.list and maintenance.get, and for no other call.
    let file = UiFile::named("calls.json").expect("calls.json is held");
    let listed = listed_calls(file.text).expect("calls.json parses");
    let mut for_page: Vec<&str> = listed
        .iter()
        .filter(|call| call.pages.iter().any(|page| page == "xcode"))
        .map(|call| call.function.as_str())
        .collect();
    for_page.sort_unstable();
    assert_eq!(
        for_page,
        [
            "hostGroups.list",
            "hosts.list",
            "maintenance.get",
            "maintenance.list",
            "xcode.list",
            "xcode.remove"
        ]
    );
}
