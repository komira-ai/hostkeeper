//! `hk xcode add`: the summary of the Macs that ends it (design section 2.5): which Macs will
//! install the release and when, from a fake gateway that answers `Hosts.List` and
//! `HostGroups.List` in pages, `Maintenance.List`, `Maintenance.Get` and `Maintenance.Schedule`
//! as the test says, in front of the fake store.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::add_rig::{Add, FLEET, Store, archive_bytes, puts, sha256};
use crate::rig::{Ran, Reply, Request, Server, TOKEN};

const HOSTS: &str = FLEET[0];
const GROUPS: &str = FLEET[1];
const RUNS: &str = FLEET[2];
const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";

const MAC: &str = "PLATFORM_DARWIN_ARM64";
const XCODE: &str = "MAINTENANCE_KIND_XCODE";

/// The line a store that already holds the release makes the run say first.
const HELD: &str = "already in the store: Xcode 27.2 (27B52)\n";

/// The line `hk maintenance schedule ...` that stores the run of `group`.
fn command(group: &str) -> String {
    format!(
        "    hk maintenance schedule --host-group {group} --kinds xcode --reason \"Xcode 27.2\" \
         --confirm true\n"
    )
}

/// What the fake gateway answers; a path in `broken` answers that reply in place of its own.
#[derive(Clone, Default)]
struct Fleet {
    /// The pages of `Hosts.List`.
    hosts: Vec<Value>,
    /// The pages of `HostGroups.List`.
    groups: Vec<Value>,
    /// The open runs `Maintenance.List` names.
    runs: Vec<Value>,
    /// The answer of `Maintenance.Get`, by run id.
    turns: BTreeMap<String, Value>,
    /// The answer of `Maintenance.Schedule`, by group: its status and body.
    schedule: BTreeMap<String, (u16, String)>,
    broken: Option<(&'static str, Reply)>,
}

/// A host that is a Mac with a maintenance report listing `xcodes`: `(version, build, selected,
/// foreign)`.
fn mac(id: &str, group: &str, xcodes: &[(&str, &str, bool, bool)]) -> Value {
    let xcodes: Vec<Value> = xcodes
        .iter()
        .map(|(version, build, selected, foreign)| {
            let mut one = json!({
                "version": version,
                "build": build,
                "path": format!("/Applications/Xcode-{version}.app"),
            });
            if *selected {
                one["selected"] = json!(true);
            }
            if *foreign {
                one["foreign"] = json!(true);
            }
            one
        })
        .collect();
    let mut host = json!({
        "nodeId": id,
        "heartbeat": {"platform": MAC},
        "maintenance": {"xcodes": xcodes},
    });
    if !group.is_empty() {
        host["hostGroup"] = json!(group);
    }
    host
}

/// A group whose Macs keep `keep` Xcodes, in batches of `batch`, with `default` selected.
fn group(name: &str, keep: u32, batch: Option<u32>, default: Option<&str>) -> Value {
    let mut maintenance = json!({"xcodeKeep": keep});
    if let Some(batch) = batch {
        maintenance["batch"] = json!(batch);
    }
    if let Some(default) = default {
        maintenance["xcodeDefault"] = json!(default);
    }
    json!({"group": {"name": name, "policy": {"maintenance": maintenance}}, "generation": "1"})
}

/// A run, as `Maintenance.List` and `Maintenance.Get` name it.
fn run(id: &str, state: &str) -> Value {
    json!({"id": id, "state": format!("MAINTENANCE_STATE_{state}")})
}

/// A host's turn of a run.
fn turn(id: &str, step: &str, kinds: &[&str]) -> Value {
    json!({"hostId": id, "step": format!("MAINTENANCE_HOST_STEP_{step}"), "kinds": kinds})
}

/// A preview of a run that would take `taken` and skip `skipped` `(host, MAINTENANCE_SKIP_*)`.
fn preview(taken: &[&str], skipped: &[(&str, &str)], batch: Option<u32>) -> (u16, String) {
    let mut hosts: Vec<Value> = taken
        .iter()
        .map(|id| turn(id, "WAITING", &[XCODE]))
        .collect();
    for (id, why) in skipped {
        let mut skipped = turn(id, "SKIPPED", &[]);
        skipped["skippedBecause"] = json!(why);
        hosts.push(skipped);
    }
    let mut run = json!({"kinds": [XCODE]});
    if let Some(batch) = batch {
        run["batch"] = json!(batch);
    }
    (200, json!({"run": run, "hosts": hosts}).to_string())
}

/// The refusal the gateway answers.
fn refusal(code: &str, reason: &str) -> Reply {
    Reply::new(
        400,
        format!("{{\"code\":\"{code}\",\"reason\":\"{reason}\"}}"),
    )
}

/// The same refusal as a preview's answer: its status and body.
fn refused(code: &str, reason: &str) -> (u16, String) {
    let reply = refusal(code, reason);
    (reply.status, String::from_utf8(reply.body).expect("text"))
}

impl Fleet {
    /// `pages` as the pages of a list: page `n` names page `n + 1` as the next.
    fn page(pages: &[Value], request: &Request) -> Reply {
        let asked: Value = serde_json::from_slice(&request.body).expect("a JSON body");
        let at = match asked["pageToken"].as_str().unwrap_or_default() {
            "" => 0,
            token => token
                .strip_prefix("page-")
                .and_then(|n| n.parse().ok())
                .expect("a token this fleet gave"),
        };
        let mut page = pages[at].clone();
        if at + 1 < pages.len() {
            page["nextPageToken"] = json!(format!("page-{}", at + 1));
        }
        Reply::new(200, page.to_string())
    }

    fn answer(&self, request: &Request) -> Option<Reply> {
        if let Some((path, reply)) = &self.broken
            && request.target == *path
        {
            return Some(reply.clone());
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        match request.target.as_str() {
            HOSTS => Some(Fleet::page(&self.hosts, request)),
            GROUPS => Some(Fleet::page(&self.groups, request)),
            RUNS => Some(Reply::new(200, json!({"runs": self.runs}).to_string())),
            GET => {
                let id = body["runId"].as_str().expect("a run id");
                Some(Reply::new(200, self.turns[id].to_string()))
            }
            SCHEDULE => {
                let group = body["hostGroup"].as_str().expect("a group");
                let (status, text) = self.schedule[group].clone();
                Some(Reply::new(status, text))
            }
            _ => None,
        }
    }

    /// `run` and its turns, as `Maintenance.List` and `Maintenance.Get` answer them.
    fn with_run(mut self, run: Value, turns: Vec<Value>) -> Fleet {
        let id = run["id"].as_str().expect("an id").to_owned();
        self.turns
            .insert(id, json!({"run": run.clone(), "hosts": turns}));
        self.runs.push(run);
        self
    }

    fn with_preview(mut self, group: &str, answer: (u16, String)) -> Fleet {
        self.schedule.insert(group.to_owned(), answer);
        self
    }

    fn broken(mut self, path: &'static str, reply: Reply) -> Fleet {
        self.broken = Some((path, reply));
        self
    }
}

/// The store of most tests: it holds the archive of the tests as 27.2 (27B52) and, besides, the
/// releases `others` as `(version, build)`.
fn store(others: &[(&str, &str)]) -> Store {
    let mut store = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    for (n, (version, build)) in others.iter().enumerate() {
        store = store.holding_sha(&format!("{n:064x}"), version, build);
    }
    store
}

/// `hk xcode add <archive>` against `store` behind `fleet`.
fn add(name: &str, store: Store, fleet: &Fleet) -> (Add, Server, Ran) {
    let add = Add::new(&format!("summary-{name}"));
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let fleet = fleet.clone();
    let server = store.serve_with(&add.rig, move |request| fleet.answer(request));
    let ran = add.add(&server, &[&path]);
    (add, server, ran)
}

/// The JSON bodies the server saw on `path`.
fn bodies(server: &Server, path: &str) -> Vec<Value> {
    server
        .seen()
        .iter()
        .filter(|seen| seen.target == path)
        .map(|seen| serde_json::from_slice(&seen.body).expect("a JSON body"))
        .collect()
}

/// The fleet of the first test: mac-build's Macs and mac-proof's, in two pages of each list, with
/// a Linux host among them.
fn two_groups() -> Fleet {
    let linux = json!({"nodeId": "build-1", "hostGroup": "linux", "heartbeat": {"platform": "PLATFORM_LINUX_X86_64"}});
    Fleet {
        hosts: vec![
            json!({"hosts": [
                mac("mac-01", "mac-build", &[]),
                mac("mac-02", "mac-build", &[("27.1", "27A10", true, false)]),
                linux,
            ]}),
            json!({"hosts": [mac("mac-03", "mac-build", &[]), mac("mac-04", "mac-proof", &[])]}),
        ],
        groups: vec![
            json!({"hostGroups": [group("mac-build", 2, Some(1), None)]}),
            json!({"hostGroups": [group("mac-proof", 0, None, None)]}),
        ],
        ..Fleet::default()
    }
    .with_preview(
        "mac-build",
        preview(&["mac-01", "mac-02", "mac-03"], &[], None),
    )
}

/// Proves: the summary of design section 1 for a store that already holds the release: after
/// `already in the store:` it says the Macs due it by group with the group's `xcode_keep` and
/// batch, what a run of the group would take, the exact line that stores it, and the Macs left
/// out with the reason; it reads both pages of `Hosts.List` and of `HostGroups.List` (each asked
/// with the page the last gave), ignores a Linux host, asks `Maintenance.List` for the runs that
/// have not ended and `Maintenance.Get` of none (there is none), and asks `Maintenance.Schedule`
/// once, for `mac-build`, with the kind `xcode`, the reason `Xcode 27.2` and no `confirm`.
#[test]
fn the_macs_due_the_release_are_listed_by_group_with_the_line_that_stores_the_run() {
    let (_add, server, ran) = add("example", store(&[("27.1", "27A10")]), &two_groups());
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 3 Macs:\n  mac-build (xcode_keep 2, batch 1): mac-01, \
             mac-02, mac-03\n    no run takes it yet; a run would take mac-01, mac-02, mac-03 \
             (batch 1)\n{}  left out: mac-04 (group mac-proof keeps no Xcode)\n",
            command("mac-build")
        )
    );
    assert_eq!(
        bodies(&server, HOSTS),
        [
            json!({"limit": 256, "pageToken": ""}),
            json!({"limit": 256, "pageToken": "page-1"})
        ]
    );
    assert_eq!(
        bodies(&server, GROUPS),
        [
            json!({"limit": 256, "pageToken": ""}),
            json!({"limit": 256, "pageToken": "page-1"})
        ]
    );
    assert_eq!(
        bodies(&server, RUNS),
        [json!({"limit": 256, "states": [
            "MAINTENANCE_STATE_SCHEDULED", "MAINTENANCE_STATE_RUNNING", "MAINTENANCE_STATE_HALTED"
        ]})]
    );
    assert!(bodies(&server, GET).is_empty());
    let schedule: Vec<_> = server
        .seen()
        .into_iter()
        .filter(|seen| seen.target == SCHEDULE)
        .collect();
    assert_eq!(schedule.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&schedule[0].body).expect("JSON"),
        json!({"kinds": ["MAINTENANCE_KIND_XCODE"], "hostGroup": "mac-build", "reason": "Xcode 27.2"})
    );
    assert!(
        !schedule[0].text().contains("confirm"),
        "Schedule was asked to store the run: {}",
        schedule[0].text()
    );
}

/// Proves: a Mac whose newest missing release is a newer one is `due: 27.3, then 27.2 in the same
/// turn` (the Mac's own due item names 27.3); a Mac that has 27.3 and lacks 27.2 is plainly due;
/// a Mac that has both installed and selected 27.3 is `installed`, and a group's default is
/// `latest` and its batch 1 when the policy names neither.
#[test]
fn a_mac_whose_due_item_names_a_newer_release_is_due_that_one_then_this_in_the_same_turn() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g", &[("27.1", "27A10", true, false)]),
            mac("mac-02", "g", &[("27.3", "27C10", true, false)]),
            mac("mac-03", "g", &[("27.3", "27C10", true, false), ("27.2", "27B52", false, false)]),
        ]})],
        groups: vec![json!({"hostGroups": [{"group": {"name": "g", "policy": {"maintenance": {"xcodeKeep": 2}}}}]})],
        ..Fleet::default()
    }
    .with_preview("g", preview(&["mac-01", "mac-02"], &[], None));
    let (_add, _server, ran) = add(
        "behind",
        store(&[("27.3", "27C10"), ("27.1", "27A10")]),
        &fleet,
    );
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 2 Macs:\n  g (xcode_keep 2, batch 1): mac-01, \
             mac-02\n    mac-01: due: 27.3, then 27.2 in the same turn\n    no run takes it yet; \
             a run would take mac-01, mac-02 (batch 1)\n{}  installed: mac-03\n",
            command("g")
        )
    );
}

/// Proves: a Mac that has the release installed is due again, `installed, not selected: due to be
/// selected`, only when the release is the group's default and `xcode-select` names another; a
/// Mac with it selected is `installed`, and so is a Mac of a group whose default is another
/// version (pinned with `xcode_default`) whether or not that one is selected, or a version the
/// store does not keep; a `foreign` Xcode that is selected does not count as the release selected.
#[test]
fn a_mac_with_it_installed_is_due_to_select_it_only_while_it_is_the_groups_default() {
    let both = |selected: &str| {
        [
            ("27.2", "27B52", selected == "27.2", false),
            ("26.4", "26E10", selected == "26.4", false),
        ]
    };
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "latest", &both("26.4")),
            mac("mac-02", "latest", &both("27.2")),
            mac("mac-03", "pinned", &both("26.4")),
            mac("mac-04", "pinned", &both("27.2")),
            mac("mac-05", "gone", &both("27.2")),
            mac(
                "mac-06",
                "latest",
                &[("27.2", "27B52", false, false), ("27.2", "27B52", true, true)],
            ),
        ]})],
        groups: vec![json!({"hostGroups": [
            group("latest", 2, None, Some("latest")),
            group("pinned", 2, None, Some("26.4")),
            group("gone", 2, None, Some("25.0")),
        ]})],
        ..Fleet::default()
    }
    .with_preview("latest", preview(&["mac-01", "mac-06"], &[], None));
    let (_add, _server, ran) = add("select", store(&[("26.4", "26E10")]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 2 Macs:\n  latest (xcode_keep 2, batch 1): mac-01, \
             mac-06\n    mac-01: installed, not selected: due to be selected\n    mac-06: \
             installed, not selected: due to be selected\n    no run takes it yet; a run would \
             take mac-01, mac-06 (batch 1)\n{}  installed: mac-02, mac-03, mac-04, mac-05\n",
            command("latest")
        )
    );
}

/// Proves: only an Xcode in its own directory with this build counts as installed: a `foreign`
/// Xcode of the same version and build, the same version of another build, and a report that
/// lists no Xcode at all each leave the Mac due.
#[test]
fn an_xcode_installed_elsewhere_or_of_another_build_does_not_count() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g", &[("27.2", "27B52", true, true)]),
            mac("mac-02", "g", &[("27.2", "27B51", true, false)]),
            mac("mac-03", "g", &[]),
        ]})],
        groups: vec![json!({"hostGroups": [group("g", 1, None, None)]})],
        ..Fleet::default()
    }
    .with_preview("g", preview(&["mac-01", "mac-02", "mac-03"], &[], None));
    let (_add, _server, ran) = add("installed", store(&[]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 3 Macs:\n  g (xcode_keep 1, batch 1): mac-01, \
             mac-02, mac-03\n    no run takes it yet; a run would take mac-01, mac-02, mac-03 \
             (batch 1)\n{}",
            command("g")
        )
    );
}

/// Proves: a Mac in an open run whose turn has not started and whose kinds include `xcode` is
/// `in run <id>, window <start> to <end>` (UTC) for a run not yet in its window, with the window's
/// numbers as strings or numbers, and `in run <id>, now` for one that is running; `Maintenance.Get`
/// is asked of each open run; a Mac whose turn in the newest run is over is taken by no run; no
/// schedule is asked for a group whose Macs are all in a run, and one is for a group with a Mac
/// that no run takes.
#[test]
fn a_mac_in_a_run_that_takes_xcode_says_the_run_and_its_window() {
    let mut window = run("r-win", "SCHEDULED");
    window["windowStartUnix"] = json!("1790000000");
    window["windowEndUnix"] = json!(1_790_009_000);
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g", &[]),
            mac("mac-02", "g", &[]),
            mac("mac-03", "h", &[]),
        ]})],
        groups: vec![json!({"hostGroups": [group("g", 1, None, None), group("h", 1, None, None)]})],
        ..Fleet::default()
    }
    .with_run(
        run("r-now", "RUNNING"),
        vec![
            turn("mac-02", "WAITING", &["MAINTENANCE_KIND_MACOS", XCODE]),
            turn("mac-03", "DONE", &[XCODE]),
        ],
    )
    .with_run(
        window,
        vec![
            turn("mac-01", "WAITING", &[XCODE]),
            turn("mac-03", "NOT_REACHED", &[XCODE]),
        ],
    )
    .with_preview("h", preview(&["mac-03"], &[], None));
    let (_add, server, ran) = add("when", store(&[]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 3 Macs:\n  g (xcode_keep 1, batch 1): mac-01, \
             mac-02\n    mac-01: in run r-win, window 2026-09-21 14:13 UTC to 2026-09-21 16:43 \
             UTC\n    mac-02: in run r-now, now\n  h (xcode_keep 1, batch 1): mac-03\n    no run \
             takes it yet; a run would take mac-03 (batch 1)\n{}",
            command("h")
        )
    );
    assert_eq!(
        bodies(&server, GET),
        [json!({"runId": "r-now"}), json!({"runId": "r-win"})]
    );
    let asked: Vec<Value> = bodies(&server, SCHEDULE);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["hostGroup"], "h");
}

/// Proves: a window with no end says `to no end`.
#[test]
fn a_window_with_no_end_says_so() {
    let mut window = run("r-win", "SCHEDULED");
    window["windowStartUnix"] = json!("1790000000");
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [mac("mac-01", "g", &[])]})],
        groups: vec![json!({"hostGroups": [group("g", 1, None, None)]})],
        ..Fleet::default()
    }
    .with_run(window, vec![turn("mac-01", "WAITING", &[XCODE])]);
    let (_add, _server, ran) = add("no-end", store(&[]), &fleet);
    assert!(
        ran.stdout
            .contains("    mac-01: in run r-win, window 2026-09-21 14:13 UTC to no end\n"),
        "{ran:?}"
    );
}

/// Proves: a Mac in an open run that does not take `xcode` says `in run <id>, which does not take
/// Xcode: schedule after it ends`; a Mac already in its turn says the same of its turn; a Mac
/// whose turn was deferred is offered again and takes the release; a Mac of a group the run is
/// halted in waits for the halted host; and none of these has a schedule line, while the group of
/// a Mac whose turn is done has one.
#[test]
fn a_mac_in_a_run_that_does_not_take_xcode_has_no_schedule_line() {
    let mut halted = run("r-halt", "HALTED");
    halted["haltedGroups"] = json!(["i"]);
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g", &[]),
            mac("mac-02", "g", &[]),
            mac("mac-03", "g", &[]),
            mac("mac-04", "h", &[]),
            mac("mac-05", "i", &[]),
        ]})],
        groups: vec![json!({"hostGroups": [
            group("g", 1, None, None), group("h", 1, None, None), group("i", 1, None, None)
        ]})],
        ..Fleet::default()
    }
    .with_run(
        run("r-old", "RUNNING"),
        vec![
            turn("mac-01", "WAITING", &["MAINTENANCE_KIND_MACOS"]),
            turn("mac-02", "STAGING", &[XCODE]),
            turn("mac-03", "DEFERRED", &[XCODE]),
            turn("mac-04", "DONE", &[XCODE]),
        ],
    )
    .with_run(halted, vec![turn("mac-05", "WAITING", &[XCODE])])
    .with_preview("h", preview(&["mac-04"], &[], None));
    let (_add, server, ran) = add("no-xcode", store(&[]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 5 Macs:\n  g (xcode_keep 1, batch 1): mac-01, mac-02, \
             mac-03\n    mac-01: in run r-old, which does not take Xcode: schedule after it \
             ends\n    mac-02: in run r-old, whose turn has already started: schedule after it \
             ends\n    mac-03: in run r-old, now\n  h (xcode_keep 1, batch 1): mac-04\n    no run \
             takes it yet; a run would take mac-04 (batch 1)\n{}  i (xcode_keep 1, batch 1): \
             mac-05\n    mac-05: in run r-halt, which is halted in group i: put the halted host \
             back or cancel the run, then schedule\n",
            command("h")
        )
    );
    let asked: Vec<Value> = bodies(&server, SCHEDULE);
    assert_eq!(asked.len(), 1, "{asked:?}");
}

/// Proves: the hosts a preview skips are printed with the coordinator's reason, as the token of
/// the skip; the batch is the run's own when the preview names one and the group's when it does
/// not.
#[test]
fn skipped_hosts_are_printed_with_the_coordinators_reason_and_the_batch_of_the_run() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g", &[]), mac("mac-02", "g", &[]), mac("mac-03", "g", &[]),
            mac("mac-04", "h", &[]),
        ]})],
        groups: vec![
            json!({"hostGroups": [group("g", 1, Some(2), None), group("h", 1, None, None)]}),
        ],
        ..Fleet::default()
    }
    .with_preview(
        "g",
        preview(
            &["mac-01"],
            &[
                ("mac-02", "MAINTENANCE_SKIP_NO_UPDATE_ACCOUNT"),
                ("mac-03", "MAINTENANCE_SKIP_FILEVAULT_NOT_READY"),
            ],
            None,
        ),
    )
    .with_preview("h", preview(&["mac-04"], &[], Some(3)));
    let (_add, _server, ran) = add("skipped", store(&[]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 4 Macs:\n  g (xcode_keep 1, batch 2): mac-01, mac-02, \
             mac-03\n    no run takes it yet; a run would take mac-01 (batch 2)\n    skipped: \
             mac-02 (no_update_account), mac-03 (filevault_not_ready)\n{}  h (xcode_keep 1, \
             batch 1): mac-04\n    no run takes it yet; a run would take mac-04 (batch 3)\n{}",
            command("g"),
            command("h")
        )
    );
}

/// Proves: a refusal of the whole run is printed with the coordinator's reason and code and exits
/// 0 without a word on stderr about it; for `last_host` the line that stores the run carries
/// `--take-last-host true` and says why, and for another refusal there is no line to store it.
#[test]
fn a_refused_run_is_printed_with_the_coordinators_reason_and_last_host_names_its_flag() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [mac("mac-01", "g", &[]), mac("mac-02", "h", &[])]})],
        groups: vec![json!({"hostGroups": [group("g", 1, None, None), group("h", 1, None, None)]})],
        ..Fleet::default()
    }
    .with_preview("g", refused("FAILED_PRECONDITION", "last_host"))
    .with_preview("h", refused("FAILED_PRECONDITION", "host_in_maintenance"));
    let (_add, _server, ran) = add("refused", store(&[]), &fleet);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is now due on 2 Macs:\n  g (xcode_keep 1, batch 1): mac-01\n    no \
             run takes it yet; the coordinator would refuse a run: last_host \
             (FAILED_PRECONDITION)\n    a run that takes every host of g needs --take-last-host \
             true:\n{}  h (xcode_keep 1, batch 1): mac-02\n    no run takes it yet; the \
             coordinator would refuse a run: host_in_maintenance (FAILED_PRECONDITION)\n",
            command("g").trim_end().to_owned() + " --take-last-host true\n"
        )
    );
    assert!(!ran.stderr.contains("coordinator answered"), "{ran:?}");
    assert!(!ran.stderr.contains("summary unavailable"), "{ran:?}");
}

/// Proves: a Mac of a group with `xcode_keep` 0, of a group that keeps only releases newer than
/// this one (the boundary: with `xcode_keep` 1 and a newer release held, this one is left out),
/// of a group the coordinator does not list, in no group, and one with no maintenance report are
/// each left out with their reason, Macs of one reason on one line; with none due the first line
/// says so and nothing is scheduled.
#[test]
fn macs_that_will_not_install_it_are_left_out_with_the_reason() {
    let report_less =
        json!({"nodeId": "mac-05", "hostGroup": "g1", "heartbeat": {"platform": MAC}});
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "g0", &[]),
            mac("mac-02", "g1", &[]),
            mac("mac-03", "", &[]),
            mac("mac-04", "zz", &[]),
            report_less,
            mac("mac-06", "g0", &[]),
        ]})],
        groups: vec![
            json!({"hostGroups": [group("g0", 0, None, None), group("g1", 1, None, None)]}),
        ],
        ..Fleet::default()
    };
    let (_add, server, ran) = add("left-out", store(&[("27.3", "27C10")]), &fleet);
    assert_eq!(
        ran.stdout,
        format!(
            "{HELD}Xcode 27.2 is due on no Mac:\n  left out: mac-01, mac-06 (group g0 keeps no \
             Xcode)\n  left out: mac-02 (group g1 keeps only newer releases (27.3))\n  left out: \
             mac-04 (group zz keeps no Xcode)\n  left out: mac-03 (it is in no host group)\n  \
             left out: mac-05 (the coordinator keeps no maintenance report of it)\n"
        )
    );
    assert!(bodies(&server, SCHEDULE).is_empty());
}

/// Proves: with the release newest of the group's `xcode_keep` 2 releases the Mac is due, and with
/// `xcode_keep` 2 and two newer releases held it is left out (the keep count is the boundary).
#[test]
fn the_group_keeps_the_newest_releases_and_this_one_only_while_it_is_among_them() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [mac("mac-01", "g", &[])]})],
        groups: vec![json!({"hostGroups": [group("g", 2, None, None)]})],
        ..Fleet::default()
    }
    .with_preview("g", preview(&["mac-01"], &[], None));
    let (_add, _server, ran) = add("keeps-one-newer", store(&[("27.3", "27C10")]), &fleet);
    assert!(
        ran.stdout.contains("Xcode 27.2 is now due on 1 Mac:"),
        "{ran:?}"
    );
    let (_add, _server, ran) = add(
        "keeps-two-newer",
        store(&[("27.3", "27C10"), ("27.10", "27D1")]),
        &fleet,
    );
    assert!(
        ran.stdout
            .contains("left out: mac-01 (group g keeps only newer releases (27.10, 27.3))"),
        "{ran:?}"
    );
}

/// Proves: a release the run has just uploaded counts as held though the listing read before the
/// upload lacks it: after `stored:` Macs of a group keeping 2 are due it beside a newer release
/// they have, and Macs of a group keeping 1 are left out because a newer one is held.
#[test]
fn a_release_just_stored_is_among_the_releases_the_group_keeps() {
    let fleet = Fleet {
        hosts: vec![json!({"hosts": [
            mac("mac-01", "two", &[("27.3", "27C10", true, false)]),
            mac("mac-02", "one", &[("27.3", "27C10", true, false)]),
        ]})],
        groups: vec![
            json!({"hostGroups": [group("two", 2, None, None), group("one", 1, None, None)]}),
        ],
        ..Fleet::default()
    }
    .with_preview("two", preview(&["mac-01"], &[], None));
    let store = Store::default().holding_sha(&format!("{:064x}", 9), "27.3", "27C10");
    let (_add, server, ran) = add("stored", store, &fleet);
    assert_eq!(puts(&server).len(), 1);
    assert_eq!(
        ran.stdout,
        format!(
            "stored: Xcode 27.2 (27B52), sha256 {}, {} bytes\nXcode 27.2 is now due on 1 Mac:\n  \
             two (xcode_keep 2, batch 1): mac-01\n    no run takes it yet; a run would take \
             mac-01 (batch 1)\n{}  left out: mac-02 (group one keeps only newer releases \
             (27.3))\n",
            sha256(&archive_bytes()),
            archive_bytes().len(),
            command("two")
        )
    );
}

/// Proves: with no Mac known stdout holds the answer line alone, stderr says there is none, and
/// the groups and runs are not asked for.
#[test]
fn with_no_mac_stdout_holds_the_answer_alone() {
    let fleet = Fleet {
        hosts: vec![
            json!({"hosts": [{"nodeId": "build-1", "heartbeat": {"platform": "PLATFORM_LINUX_X86_64"}}]}),
        ],
        ..Fleet::default()
    };
    let (_add, server, ran) = add("no-mac", store(&[]), &fleet);
    assert_eq!((ran.code, ran.stdout.as_str()), (0, HELD), "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: the coordinator knows no Mac, so no Mac is due Xcode 27.2\n"),
        "{ran:?}"
    );
    assert!(bodies(&server, GROUPS).is_empty() && bodies(&server, RUNS).is_empty());
}

/// Proves: a call of the summary that fails ends it with nothing printed of it: the run exits 0
/// with `already in the store:` alone on stdout, and stderr says `summary unavailable: <why>; the
/// archive is stored` (the call and what it answered: a refusal's reason and code, an answer that
/// is not the gateway's, one that is not JSON, a number that is none, no answer at all) and the
/// two commands to read the Macs by hand.
#[test]
fn a_call_that_fails_leaves_the_archive_stored_and_says_summary_unavailable() {
    let fleet = || {
        Fleet {
            hosts: vec![json!({"hosts": [mac("mac-01", "g", &[])]})],
            groups: vec![json!({"hostGroups": [group("g", 1, None, None)]})],
            ..Fleet::default()
        }
        .with_run(
            run("r-1", "RUNNING"),
            vec![turn("mac-01", "DONE", &[XCODE])],
        )
    };
    let html = || Reply::new(502, "<html>bad gateway</html>");
    let mut bad_window = run("r-1", "RUNNING");
    bad_window["windowStartUnix"] = json!("soon");
    let cases: Vec<(&str, Fleet, &str)> = vec![
        (
            "Hosts.List",
            fleet().broken(HOSTS, refusal("PERMISSION_DENIED", "forbidden")),
            "Hosts.List: refused: forbidden (PERMISSION_DENIED)",
        ),
        (
            "HostGroups.List",
            fleet().broken(GROUPS, html()),
            "HostGroups.List: the answer (HTTP 502) is not the gateway's",
        ),
        (
            "Maintenance.List",
            fleet().broken(RUNS, Reply::new(200, "not json")),
            "Maintenance.List: the answer is not the coordinator's JSON (",
        ),
        (
            "Maintenance.Get",
            fleet().broken(GET, refusal("NOT_FOUND", "unknown_run")),
            "Maintenance.Get: refused: unknown_run (NOT_FOUND)",
        ),
        (
            "bad number",
            fleet().broken(
                GET,
                Reply::new(200, json!({"run": bad_window, "hosts": []}).to_string()),
            ),
            "Maintenance.Get: the answer is not the coordinator's JSON (",
        ),
        (
            "Maintenance.Schedule",
            fleet().broken(SCHEDULE, html()),
            "Maintenance.Schedule: the answer (HTTP 502) is not the gateway's",
        ),
        (
            "no answer",
            fleet().broken(HOSTS, Reply::hang_up()),
            "Hosts.List: no answer from https://127.0.0.1:",
        ),
    ];
    for (n, (name, fleet, why)) in cases.into_iter().enumerate() {
        let (_add, _server, ran) = add(&format!("broken-{n}"), store(&[]), &fleet);
        assert_eq!(
            (ran.code, ran.stdout.as_str()),
            (0, HELD),
            "{name}: {ran:?}"
        );
        let said: Vec<&str> = ran.stderr.lines().collect();
        let first = said
            .iter()
            .find(|line| line.starts_with("hk: summary unavailable: "))
            .unwrap_or_else(|| panic!("{name}: {ran:?}"));
        assert!(
            first.starts_with(&format!("hk: summary unavailable: {why}")),
            "{name}: {first}"
        );
        assert!(
            first.ends_with("; the archive is stored"),
            "{name}: {first}"
        );
        assert!(
            said.contains(&"hk: to read it by hand: hk hosts list, and hk maintenance list"),
            "{name}: {ran:?}"
        );
    }
}

/// Proves: a failed summary does not undo an upload: a run that stores the archive and then finds
/// `Hosts.List` refused exits 0, has made its one upload and prints `stored:` alone on stdout.
#[test]
fn a_failed_summary_after_an_upload_still_exits_0_with_stored_said() {
    let fleet = Fleet::default().broken(HOSTS, refusal("INTERNAL", "store_failed"));
    let (_add, server, ran) = add("after-upload", Store::default(), &fleet);
    assert_eq!(puts(&server).len(), 1);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stdout
            .starts_with("stored: Xcode 27.2 (27B52), sha256 ")
    );
    assert_eq!(ran.stdout.lines().count(), 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: summary unavailable: Hosts.List: refused: store_failed (INTERNAL); the archive is stored\n"),
        "{ran:?}"
    );
}

/// Proves: a list whose next page token is the one just asked ends the summary
/// (`summary unavailable`) instead of asking for ever.
#[test]
fn a_page_token_that_does_not_change_ends_the_summary() {
    let fleet = Fleet::default().broken(
        HOSTS,
        Reply::new(
            200,
            json!({"hosts": [], "nextPageToken": "same"}).to_string(),
        ),
    );
    let (_add, server, ran) = add("loop", store(&[]), &fleet);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr.contains(
            "hk: summary unavailable: Hosts.List: the page after same is same again; the archive is stored\n"
        ),
        "{ran:?}"
    );
    assert_eq!(bodies(&server, HOSTS).len(), 2);
}

/// Proves: the token is the bearer of every call of the summary and is in none of their bodies,
/// and in no line of stdout or stderr, on a run that prints the whole summary and on one whose
/// summary fails.
#[test]
fn the_token_is_in_no_line_of_the_summary_or_its_failure() {
    let ok = add("token-ok", store(&[("27.1", "27A10")]), &two_groups());
    let failed = add(
        "token-failed",
        store(&[]),
        &Fleet::default().broken(HOSTS, Reply::new(502, "bad gateway")),
    );
    for (_add, server, ran) in [ok, failed] {
        assert_eq!(ran.code, 0, "{ran:?}");
        assert!(!ran.stdout.contains(TOKEN) && !ran.stderr.contains(TOKEN));
        let calls: Vec<_> = server
            .seen()
            .into_iter()
            .filter(|seen| seen.target != crate::add_rig::LIST)
            .collect();
        assert!(!calls.is_empty());
        for seen in calls {
            assert!(!seen.text().contains(TOKEN), "{}", seen.target);
            assert_eq!(
                seen.header("authorization"),
                Some(format!("Bearer {TOKEN}").as_str()),
                "{}",
                seen.target
            );
        }
    }
}
