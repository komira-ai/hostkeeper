//! What is due on hosts, read only: the host page's header and Updates tab, Home's updates notice
//! and the Maintenance page, run in the JavaScript engine against the fake gateway of `support`,
//! with the maintenance report the coordinator answers in `Host.maintenance`.

mod support;

use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_ui_tests::client_gen::functions;
use hostkeeper_ui_tests::{UiFile, call_sites, check_calls};
use prost::Message;
use prost_types::FileDescriptorSet;
use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver, render, shown};

/// Markup in a text the report carries (a due item's label).
const MARKUP: &str = "<img src=x onerror=alert(1)>";

/// A due item of `kind` (`LINUX`, `MACOS`, `XCODE`, `REBOOT`) with the fields given.
fn item(kind: &str, fields: Value) -> Value {
    let mut out = json!({"kind": format!("MAINTENANCE_KIND_{kind}")});
    for (key, value) in fields.as_object().expect("an object") {
        out[key] = value.clone();
    }
    out
}

/// `count` Linux security updates, none of which needs a reboot.
fn security(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            item(
                "LINUX",
                json!({"from": "1.0", "to": "1.1", "label": format!("lib{i}"), "security": true}),
            )
        })
        .collect()
}

/// A Linux kernel update: the new kernel package, and the reboot the newer kernel needs.
fn kernel_due() -> Vec<Value> {
    vec![
        item(
            "LINUX",
            json!({"from": "6.8.0-45-generic", "to": "6.8.0-47-generic", "needsReboot": true,
                   "label": "linux-image-generic", "security": true}),
        ),
        item("REBOOT", json!({"needsReboot": true, "label": "kernel"})),
    ]
}

/// A macOS update that needs a restart.
fn macos_due() -> Vec<Value> {
    vec![item(
        "MACOS",
        json!({"from": "26.1", "to": "26.2", "needsReboot": true, "label": "macOS Tahoe 26.2-25C56"}),
    )]
}

/// A Linux host's report listing `due`, with its last look 600 seconds before the coordinator's
/// answer.
fn linux_report(due: Vec<Value>) -> Value {
    json!({
        "host": {"osName": "ubuntu", "osVersion": "24.04", "runningKernel": "6.8.0-45-generic",
                 "newestKernel": "6.8.0-45-generic", "bootId": "b-1"},
        "due": due,
        "lookedUnix": "1799999400",
    })
}

/// A host of `group` with `report`, `age` seconds since its last look, and `stale` as the
/// coordinator says.
fn host(node_id: &str, group: &str, report: Value, age: &str, stale: bool) -> Value {
    let mut out = json!({"nodeId": node_id, "maintenance": report, "maintenanceLookAgeSecs": age});
    if !group.is_empty() {
        out["hostGroup"] = json!(group);
    }
    if stale {
        out["maintenanceStale"] = json!(true);
    }
    out
}

/// A gateway that answers the host page's three reads once, with `host` for Hosts.Get.
fn host_ui(host: &Value) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    ui
}

/// The driver's view of the host page's header: the due phrases in the host bar, the stale pill,
/// how many notices there are, the "nothing listed" words, and whether the Updates tab is marked.
const HEADER: &str = r#"
const bar = nodes(tree).find((n) => n.attrs["class"] === "hostbar");
const all = (name) => nodes(tree).filter((n) => n.attrs["name"] === name).map(text);
const tab = nodes(tree).find((n) => n.attrs["name"] === "tab-updates");
return {
  items: nodes(bar).filter((n) => n.attrs["name"] === "due-item").map(text),
  stale: all("due-stale"),
  notices: nodes(bar).filter((n) => n.attrs["name"] === "due").length,
  everywhere: all("due").length,
  quiet: nodes(bar).filter((n) => n.attrs["class"] === "quiet").map(text),
  marked: nodes(tab).filter((n) => n.attrs["class"] === "dot warn").length,
};"#;

fn header_of(host: &Value) -> Value {
    let mut ui = host_ui(host);
    let out = ui.run(&page_driver("host", r#"{ nodeId: "h-1" }"#, "[]", HEADER));
    out["out"].clone()
}

#[test]
fn the_host_header_shows_exactly_what_is_due() {
    // Asserts: a Linux host with a new kernel (its package and the reboot it needs) shows one
    // phrase, "kernel update, restart needed", and does not also count the kernel package as a
    // security update; a host with 12 security updates shows "12 security updates"; a Mac with a
    // macOS update that needs a restart shows "macOS 26.1 to 26.2, restart". Each in the header,
    // with no stale pill, and the Updates tab marked.
    let cases = [
        (kernel_due(), json!(["kernel update, restart needed"])),
        (security(12), json!(["12 security updates"])),
        (macos_due(), json!(["macOS 26.1 to 26.2, restart"])),
    ];
    for (due, expected) in cases {
        let out = header_of(&host("h-1", "g", linux_report(due), "600", false));
        assert_eq!(
            out,
            json!({"items": expected, "stale": [], "notices": 1, "everywhere": 1, "quiet": [], "marked": 1}),
            "{expected}"
        );
    }
}

#[test]
fn a_host_with_nothing_due_shows_no_notice() {
    // Asserts: a host whose fresh report lists nothing, and a host with no report (its daemon
    // does not announce maintenance-v1), show no notice in the header and leave the Updates tab
    // unmarked; the tab says nothing is due, or that there is no report, and shows a Linux host's
    // facts with no Mac section.
    let fresh = host("h-1", "g", linux_report(vec![]), "600", false);
    let none = json!({"nodeId": "h-1"});
    for one in [&fresh, &none] {
        assert_eq!(
            header_of(one),
            json!({"items": [], "stale": [], "notices": 0, "everywhere": 0, "quiet": [], "marked": 0}),
            "{one}"
        );
    }
    let mut ui = host_ui(&fresh);
    let out = ui.run(&page_driver("host", r#"{ nodeId: "h-1" }"#, "[]", PANEL));
    assert_eq!(
        out["out"],
        json!({
            "rows": [
                ["Operating system", "ubuntu 24.04"],
                ["Running kernel", "6.8.0-45-generic"],
                ["Newest kernel installed", "6.8.0-45-generic"],
                ["Booted", "-"],
                ["Last look", "2027-01-15 07:50:00 UTC (10 min ago)"],
                ["Listing lines no parser knew", "0"],
            ],
            "said": ["Nothing is due."],
            "mac": 0,
        })
    );
    let mut ui = host_ui(&none);
    let out = ui.run(&page_driver("host", r#"{ nodeId: "h-1" }"#, "[]", PANEL));
    assert_eq!(
        out["out"],
        json!({
            "rows": [],
            "said": ["The host's daemon reports no maintenance: it does not announce maintenance-v1, or the coordinator has read no report of it yet."],
            "mac": 0,
        })
    );
}

/// The driver's view of the Updates tab: the rows of its tables, the words it says when nothing is
/// due or there is no report, and how many Mac sections it has.
const PANEL: &str = r#"
const panel = nodes(tree).find((n) => String(n.attrs["class"]).startsWith("panel")
  && nodes(n).some((x) => ["update-facts", "no-report"].includes(x.attrs["name"])));
return {
  rows: rows(panel),
  said: nodes(panel).filter((n) => ["nothing-due", "no-report"].includes(n.attrs["name"])).map(text),
  mac: nodes(panel).filter((n) => n.attrs["name"] === "mac-facts").length,
};"#;

#[test]
fn a_stale_look_is_shown_stale() {
    // Asserts: a report the coordinator calls stale shows a stale pill with its age in the
    // header, even when it lists nothing ("nothing listed"), and with what it does list; the
    // Updates tab marks the last look stale and says an empty list may hide what is due. A
    // stale look of age 0 (one ahead of the coordinator's clock) is said to be ahead, not "0 s".
    let empty = host("h-1", "g", linux_report(vec![]), "9000", true);
    assert_eq!(
        header_of(&empty),
        json!({"items": [], "stale": ["stale look (2 h ago)"], "notices": 1, "everywhere": 1,
               "quiet": ["nothing listed"], "marked": 1})
    );
    let listed = host("h-1", "g", linux_report(security(1)), "259200", true);
    assert_eq!(
        header_of(&listed),
        json!({"items": ["1 security update"], "stale": ["stale look (3 days ago)"], "notices": 1,
               "everywhere": 1, "quiet": [], "marked": 1})
    );
    let ahead = host("h-1", "g", linux_report(vec![]), "0", true);
    assert_eq!(
        header_of(&ahead)["stale"],
        json!(["stale look (ahead of the coordinator's clock)"])
    );
    let mut ui = host_ui(&empty);
    let out = ui.run(&page_driver("host", r#"{ nodeId: "h-1" }"#, "[]", PANEL));
    assert_eq!(
        out["out"]["rows"][4],
        json!(["Last look", "2027-01-15 07:50:00 UTC (2 h ago, stale)"])
    );
    assert_eq!(
        out["out"]["said"],
        json!([
            "The last look listed nothing, but it is stale: something may be due that it has not seen."
        ])
    );
}

#[test]
fn the_updates_tab_shows_the_facts_each_due_item_the_xcodes_and_the_last_look() {
    // Asserts: a Mac's Updates tab shows what it runs and its build, its serial, when it booted,
    // the last look and how old it is, the lines no parser knew, each due item (a label carrying
    // markup stays text), what a macOS update needs of it and each Xcode installed with the
    // selected one; and it calls nothing beyond the host page's own reads.
    let report = json!({
        "host": {"osName": "macOS", "osVersion": "26.1", "osBuild": "25B78", "hardwareSerial": "C02XYZ",
                 "runningKernel": "25.1.0", "bootId": "b-1", "bootUnix": "1799990000"},
        "xcodes": [
            {"version": "27.1", "build": "27A100", "path": "/Applications/Xcode-27.1.app", "selected": true},
            {"version": "26.4", "build": "26E200", "path": "/Applications/Xcode-26.4.app"},
        ],
        "mac": {"filevault": "FILE_VAULT_STATE_ON", "authrestartSupported": true, "dataFreeBytes": "107374182400",
                "updateAccount": {"present": true, "admin": true, "secureToken": true},
                "commandLineTools": {"receipt": true, "files": true, "version": "26.1.0.0.1.1761"},
                "mdmConflicts": ["Deferrals"]},
        "due": [item("MACOS", json!({"from": "26.1", "to": "26.2", "needsReboot": true, "label": MARKUP}))],
        "dueLeftOut": 3,
        "lookedUnix": "1799999040",
        "unparsedLines": 2,
    });
    let mac = host("h-1", "mac-build", report, "960", false);
    let mut ui = host_ui(&mac);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        &format!(
            "{}\nconst more = nodes(panel).filter((n) => n.attrs[\"class\"] === \"note\").map(text);\nreturn {{ rows: rows(panel), more }};",
            PANEL
                .split("return {")
                .next()
                .expect("the panel's lookup")
        ),
    ));
    assert_eq!(
        out["out"],
        json!({
            "rows": [
                ["Operating system", "macOS 26.1 (25B78)"],
                ["Running kernel", "25.1.0"],
                ["Hardware serial", "C02XYZ"],
                ["Booted", "2027-01-15 05:13:20 UTC"],
                ["Last look", "2027-01-15 07:44:00 UTC (16 min ago)"],
                ["Listing lines no parser knew", "2"],
                ["Kind", "From", "To", "Restart", "Security", "Label"],
                ["macos", "26.1", "26.2", "yes", "no", MARKUP],
                ["FileVault", "on"],
                ["Restart without unlock (authrestart)", "yes"],
                ["Free on the data volume", "100.0 GiB"],
                ["Update account", "present, administrator: yes, Secure Token: yes"],
                ["Command Line Tools", "receipt: yes, files: yes, version: 26.1.0.0.1.1761"],
                ["Device-management conflicts", "Deferrals"],
                ["Version", "Build", "Path", "Selected"],
                ["27.1", "27A100", "/Applications/Xcode-27.1.app", "yes"],
                ["26.4", "26E200", "/Applications/Xcode-26.4.app", "no"],
            ],
            "more": ["3 more items due that the report leaves out."],
        })
    );
    assert!(
        !ui.methods()
            .iter()
            .any(|method| method.contains("Maintenance") || method.contains("Xcode")),
        "{:?}",
        ui.methods()
    );
}

#[test]
fn due_phrases_cover_every_kind_and_the_items_left_out() {
    // Asserts each phrase and its order: no report and an empty one say nothing; one security
    // update is singular; updates from no security origin are "other updates"; a reboot that is
    // not for a kernel is "restart needed" and comes first; a kernel reboot stands for every
    // other reboot; a macOS update without a restart or a `from`, and a major upgrade; an Xcode;
    // a kind the page does not know names its label; the items left out end the list. And the
    // age of a look, at each unit's bounds, and absent or not a number.
    let reboot = |label: &str| item("REBOOT", json!({"needsReboot": true, "label": label}));
    let other = |label: &str| item("LINUX", json!({"from": "1", "to": "2", "label": label}));
    let mixed = [security(2), vec![other("a"), reboot("reboot-required")]].concat();
    let mac_and_xcode = [macos_due(), vec![item("XCODE", json!({"to": "27.2"}))]].concat();
    let reports = json!([
        null,
        {},
        {"due": security(1)},
        {"due": [other("a"), other("b"), other("c")]},
        {"due": mixed},
        {"due": [reboot("reboot-required"), reboot("kernel")]},
        {"due": [item("MACOS", json!({"to": "26.2", "label": "macOS 26.2"})),
                 item("MACOS", json!({"from": "26.2", "to": "27.0", "needsReboot": true}))]},
        {"due": [item("XCODE", json!({"to": "27.2"}))]},
        {"due": [item("MACOS", json!({})), item("XCODE", json!({}))]},
        {"due": [{"label": "firmware"}]},
        {"due": security(12), "dueLeftOut": 4},
        {"due": mac_and_xcode},
    ]);
    let ages = json!([
        null, "0", "59", "60", "3599", "3600", "86399", "86400", "172800", "-1", "soon"
    ]);
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ ageWords, duePhrases }} from "./updates.js";
export async function main() {{
  return {{ phrases: {reports}.map((r) => duePhrases(r === null ? undefined : r)), ages: {ages}.map(ageWords) }};
}}"#
    ));
    assert_eq!(
        out["phrases"],
        json!([
            [],
            [],
            ["1 security update"],
            ["3 other updates"],
            ["restart needed", "2 security updates", "1 other update"],
            ["kernel update, restart needed"],
            ["macOS 26.2", "macOS 26.2 to 27.0, restart"],
            ["Xcode 27.2 available"],
            ["macOS update", "Xcode update available"],
            ["update firmware"],
            ["12 security updates", "and 4 more"],
            ["macOS 26.1 to 26.2, restart", "Xcode 27.2 available"],
        ])
    );
    assert_eq!(
        out["ages"],
        json!([
            "not looked yet",
            "0 s ago",
            "59 s ago",
            "1 min ago",
            "59 min ago",
            "1 h ago",
            "23 h ago",
            "1 day ago",
            "2 days ago",
            "not looked yet",
            "not looked yet",
        ])
    );
}

/// Six hosts of two groups and none, over two pages of Hosts.List: in `linux-build` one with a
/// kernel update, one with 12 security updates and one with nothing due; in `mac-build` one with a
/// macOS update and one whose stale look lists nothing; and one in no group with no report.
fn fleet_pages() -> (Value, Value) {
    let first = json!({"hosts": [
        {"nodeId": "u-1"},
        host("s-1", "linux-build", linux_report(security(12)), "3600", false),
        host("m-2", "mac-build", linux_report(vec![]), "9000", true),
    ], "nextPageToken": "p2"});
    let second = json!({"hosts": [
        host("a-1", "linux-build", linux_report(vec![]), "59", false),
        host("m-1", "mac-build", linux_report(macos_due()), "86400", false),
        host("k-1", "linux-build", linux_report(kernel_due()), "600", false),
    ]});
    (first, second)
}

/// The driver's view of the Maintenance page in `tree`: the fleet's count, and each group's title,
/// count and rows.
const MAINTENANCE: &str = r#"
const view = (t) => ({
  fleet: text(named(t, "fleet-due")[0]),
  groups: named(t, "maintenance-group").map((card) => ({
    title: text(card.children[0]), note: text(card.children[1]), rows: rows(card).slice(1),
  })),
  warnings: nodes(t).filter((n) => n.attrs["class"] === "warning").map(text),
});"#;

#[test]
fn the_maintenance_page_groups_hosts_and_lists_the_due_ones() {
    // Asserts: the page reads every page of Hosts.List and nothing else; it counts the hosts with
    // something due and their groups; it shows the groups in name order with the hosts in no group
    // last, each group's due hosts first, each host with what is due and its last look (stale ones
    // marked), a host with no report saying so; a host's name opens its page.
    let (first, second) = fleet_pages();
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), first);
    ui.reply(&m("Hosts/List"), second);
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        &format!("{MAINTENANCE}\nconst before = view(tree);\nawait click(tree, \"open-host\", 0);\nreturn {{ before, navigations: seen.navigations }};"),
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/List"), json!({})),
            (m("Hosts/List"), json!({"pageToken": "p2"})),
        ]
    );
    assert_eq!(
        out["out"]["before"],
        json!({
            "fleet": "Updates due on 3 hosts in 2 groups",
            "groups": [
                {"title": "linux-build", "note": "2 of 3 hosts with updates due", "rows": [
                    ["k-1", "kernel update, restart needed", "10 min ago"],
                    ["s-1", "12 security updates", "1 h ago"],
                    ["a-1", "nothing due", "59 s ago"],
                ]},
                {"title": "mac-build", "note": "1 of 2 hosts with updates due", "rows": [
                    ["m-1", "macOS 26.1 to 26.2, restart", "1 day ago"],
                    ["m-2", "nothing listed (stale)", "2 h ago, stale"],
                ]},
                {"title": "Unassigned", "note": "0 of 1 host with updates due", "rows": [
                    ["u-1", "no report", "not reported"],
                ]},
            ],
            "warnings": [],
        })
    );
    assert_eq!(
        out["out"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "k-1"}}])
    );
}

#[test]
fn the_maintenance_page_reads_again_every_minute_and_keeps_the_last_answer_on_a_failure() {
    // Asserts: the page asks the frame to repeat its read every 60 seconds; a repeat shows the new
    // answer (a host whose update was applied has nothing due, and the fleet count says so); a
    // repeat that fails keeps the last answer and says the read failed; and a page whose first
    // read fails shows the refusal and repeats nothing.
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [host("k-1", "linux-build", linux_report(kernel_due()), "600", false)]}),
    );
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [host("k-1", "linux-build", linux_report(vec![]), "30", false)]}),
    );
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        &format!(
            r#"{MAINTENANCE}
const seconds = seen.repeats.map((r) => r.seconds);
await seen.repeats[0].run();
const after = view(seen.shown[seen.shown.length - 1]);
await seen.repeats[0].run();
const failed = view(seen.shown[seen.shown.length - 1]);
return {{ seconds, before: view(tree).fleet, after, failed }};"#
        ),
    ));
    assert_eq!(out["out"]["seconds"], json!([60]));
    assert_eq!(out["out"]["before"], "Updates due on 1 host in 1 group");
    let applied = json!({
        "fleet": "No host reports anything due.",
        "groups": [{"title": "linux-build", "note": "0 of 1 host with updates due",
                    "rows": [["k-1", "nothing due", "30 s ago"]]}],
        "warnings": [],
    });
    assert_eq!(out["out"]["after"], applied);
    let mut failed = applied.clone();
    failed["warnings"] = json!([
        "The hosts could not be read again (UNIMPLEMENTED unimplemented): this is the last answer."
    ]);
    assert_eq!(out["out"]["failed"], failed);

    let mut ui = Ui::start();
    let out = render(&mut ui, "maintenance", "{}");
    assert!(
        shown(&out).contains("UNIMPLEMENTED unimplemented"),
        "{}",
        shown(&out)
    );
    assert_eq!(ui.methods(), [m("Hosts/List")]);
    assert_eq!(out["seen"]["repeats"], json!([]));
}

/// Home's replies, with `alerts` for Alerts.List.
fn home_ui(alerts: Value) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({ "alerts": alerts }));
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    ui
}

/// The driver's view of Home: the updates notice's lines, the alerts strip's text, and where the
/// notice's button goes.
const HOME: &str = r#"
const notices = nodes(tree).filter((n) => n.attrs["name"] === "updates-due");
const strip = nodes(tree).find((n) => String(n.attrs["class"]).startsWith("alert-strip") && n.attrs["name"] === undefined);
const groups = notices.length === 0 ? [] : named(tree, "updates-due-group").map((li) => text(li).trim());
if (notices.length > 0) {
  await click(tree, "open-maintenance");
}
return { notices: notices.length, groups, alerts: text(strip).replace(/\s+/g, " ").trim(), navigations: seen.navigations };"#;

#[test]
fn home_shows_updates_due_as_a_notice_that_links_the_maintenance_page() {
    // Asserts: each `updates_due` alert is a line of the updates notice, in the coordinator's
    // words, and not an alert of the alerts strip, whose count leaves it out; the notice's button
    // opens the Maintenance page; with no such alert there is no notice.
    let due = |group: &str, message: &str| {
        json!({"kind": "ALERT_KIND_UPDATES_DUE", "severity": "ALERT_SEVERITY_WARNING", "hostGroup": group,
               "message": message, "firstObservedUnix": "1800000000"})
    };
    let halt = json!({"kind": "ALERT_KIND_HALT", "severity": "ALERT_SEVERITY_CRITICAL", "hostId": "web-2",
                      "service": "api", "message": "api is halted", "firstObservedUnix": "1800000000"});
    let mut ui = home_ui(json!([
        due("linux-build", "Updates due on 2 hosts of linux-build"),
        halt.clone(),
        due("mac-build", "Updates due on 1 host of mac-build"),
    ]));
    let out = ui.run(&page_driver("home", "{}", "[]", HOME));
    assert_eq!(
        out["out"],
        json!({
            "notices": 1,
            "groups": ["Updates due on 2 hosts of linux-build", "Updates due on 1 host of mac-build"],
            "alerts": "Alerts (1) halt critical api is halted since 2027-01-15 08:00:00 UTC",
            "navigations": [{"name": "maintenance", "params": {}}],
        })
    );

    let mut ui = home_ui(json!([halt]));
    let out = ui.run(&page_driver("home", "{}", "[]", HOME));
    assert_eq!(out["out"]["notices"], 0);
    assert_eq!(out["out"]["navigations"], json!([]));
}

#[test]
fn every_call_of_the_new_modules_is_a_descriptor_method() {
    // Asserts: the Maintenance page calls Hosts.List and nothing else, through the generated
    // client, and that method is one of the protobuf descriptors; the module of the due words
    // calls nothing.
    let page = UiFile::named("pages/maintenance.js").expect("the Maintenance page is held");
    let sites = call_sites(page.path, page.text).unwrap_or_else(|e| panic!("{e}"));
    let methods: Vec<String> = sites
        .iter()
        .map(|site| {
            functions(COMMANDS)
                .into_iter()
                .find(|f| f.call() == site.function)
                .unwrap_or_else(|| panic!("api.{} is not a function of the client", site.function))
                .method
        })
        .collect();
    assert_eq!(methods, ["hostkeeper.operator.v1.Hosts/List"]);
    let descriptors = FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("hostkeeper-proto's descriptors decode");
    assert_eq!(check_calls(&methods, &descriptors), Ok(()));
    let words = UiFile::named("updates.js").expect("the due words are held");
    assert_eq!(call_sites(words.path, words.text), Ok(vec![]));
}
