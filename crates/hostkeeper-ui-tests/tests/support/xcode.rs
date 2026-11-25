//! What the tests of the Xcode page share: the store, hosts and groups its fake gateway answers,
//! the driver, and the helpers of the page's view (`t`, `cur`, `macTable`, `has`).

use serde_json::{Value, json};

use super::Ui;
use super::pages::{m, page_driver};

pub const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
/// The clock of the page contexts of `support` (`NOW` of `kit.js`).
pub const NOW: u64 = 1_800_000_000;

pub fn archive(sha: &str, version: &str, build: &str) -> Value {
    json!({"sha256": sha, "version": version, "build": build, "sizeBytes": "11920000000",
           "uploadedBy": "ana", "uploadedAtUnix": "1799990000"})
}

/// The archives the store of most tests holds: 27.2 and 27.1 (and 27.0 when `three`).
pub fn archives(three: bool) -> Vec<Value> {
    let mut out = vec![archive(A, "27.2", "27B52"), archive(B, "27.1", "27A10")];
    if three {
        out.push(archive(C, "27.0", "27A1"));
    }
    out
}

pub fn waiting() -> Value {
    json!({"version": "27.3", "build": "27C21", "requiresMacos": "27.1"})
}

/// An Xcode.List answer: configured, with the `archives` and the release that waits.
pub fn store(archives: Vec<Value>, waiting: Option<Value>, uploads: Vec<Value>) -> Value {
    let mut out =
        json!({"configured": true, "archives": archives, "keep": 2, "maxBytes": "21474836480"});
    if let Some(waiting) = waiting {
        out["waiting"] = waiting;
    }
    if !uploads.is_empty() {
        out["uploads"] = json!(uploads);
    }
    out
}

/// An installed Xcode as a report lists it: `(version, build, selected, foreign)`.
pub type Installed<'a> = (&'a str, &'a str, bool, bool);

/// A Mac of `group` whose report lists `xcodes` and `conflicts`.
pub fn mac(id: &str, group: &str, xcodes: &[Installed], conflicts: &[&str]) -> Value {
    let xcodes: Vec<Value> = xcodes
        .iter()
        .map(|(version, build, selected, foreign)| {
            json!({"version": version, "build": build, "path": format!("/Applications/Xcode-{version}.app"),
                   "selected": selected, "foreign": foreign})
        })
        .collect();
    let mut mac_facts = json!({});
    if !conflicts.is_empty() {
        mac_facts["mdmConflicts"] = json!(conflicts);
    }
    let mut host = json!({"nodeId": id, "heartbeat": {"platform": "PLATFORM_DARWIN_ARM64"},
        "maintenance": {"host": {"osName": "macOS"}, "xcodes": xcodes, "mac": mac_facts,
                        "lookedUnix": "1799999400"}});
    if !group.is_empty() {
        host["hostGroup"] = json!(group);
    }
    host
}

/// A host group with its Xcode policy: `keep` and, when given, `xcode_default`.
pub fn group(name: &str, keep: u32, xcode_default: Option<&str>) -> Value {
    let mut maintenance = json!({"xcodeKeep": keep});
    if let Some(default) = xcode_default {
        maintenance["xcodeDefault"] = json!(default);
    }
    json!({"group": {"name": name, "packages": ["hostkeeper"], "policy": {"maintenance": maintenance}},
           "generation": "1", "hosts": 1})
}

/// A page over one read: `xcode` (an Xcode.List answer), `hosts`, `groups` and `runs`.
pub fn fleet(xcode: Value, hosts: Vec<Value>, groups: Vec<Value>, runs: Vec<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Xcode/List"), xcode);
    ui.reply(&m("Hosts/List"), json!({ "hosts": hosts }));
    ui.reply(&m("HostGroups/List"), json!({ "hostGroups": groups }));
    ui.reply(&m("Maintenance/List"), json!({ "runs": runs }));
    ui
}

/// The helpers every driver of this file has: the text of a node with its blanks run together, the
/// tree the page last showed, and the table of the Macs as `{ name, group, keeps, installed, due,
/// device, run }`.
pub const VIEW: &str = r#"
const t = (n) => text(n).replace(/\s+/g, " ").trim();
const cur = () => seen.shown[seen.shown.length - 1] || tree;
const macTable = (tr) => named(tr, "mac").map((row) => ({
  name: t(named(row, "open-host")[0]),
  group: t(row.children[1]),
  keeps: t(row.children[2]),
  installed: nodes(named(row, "installed")[0]).filter((n) => n.tag === "div").map(t),
  installedText: t(named(row, "installed")[0]),
  due: t(named(row, "due")[0]),
  device: t(named(row, "device-management")[0]),
  run: t(named(row, "run")[0]),
}));
const has = (tr, name) => nodes(tr).some((n) => n.attrs["name"] === name);"#;

pub fn drive(ui: &mut Ui, answers: &str, then: &str) -> Value {
    ui.run(&page_driver(
        "xcode",
        "{}",
        answers,
        &[VIEW, then].join("\n"),
    ))
}

pub fn shown(out: &Value) -> &str {
    out["text"].as_str().expect("the page's text")
}

/// A fleet of two Macs of `mac-build` that have 27.2 and 27.1, and one of `mac-off`, with the store
/// of 27.2 and 27.1 and 27.3 waiting.
pub fn plain() -> Ui {
    fleet(
        store(archives(false), Some(waiting()), vec![]),
        vec![
            mac(
                "m-1",
                "mac-build",
                &[
                    ("27.2", "27B52", true, false),
                    ("27.1", "27A10", false, false),
                ],
                &[],
            ),
            mac(
                "m-2",
                "mac-build",
                &[
                    ("27.2", "27B52", true, false),
                    ("27.1", "27A10", false, false),
                ],
                &[],
            ),
            mac("m-3", "mac-off", &[], &[]),
        ],
        vec![group("mac-build", 2, None), group("mac-off", 0, None)],
        vec![],
    )
}

pub fn upload(received: &str, started_by: &str) -> Value {
    json!({"sha256": D, "version": "27.3", "build": "27C21", "receivedBytes": received,
           "sizeBytes": "11920000000", "startedBy": started_by,
           "lastWriteUnix": (NOW - 120).to_string()})
}
