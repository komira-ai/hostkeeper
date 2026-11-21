//! The runs section of the Maintenance page: each run with its hosts' steps in words, the links to
//! a restarting host's Console or Desktop, Retry, Put back and Cancel, and the reads of the runs,
//! run in the JavaScript engine against the fake gateway of `support`.

mod support;

use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_ui_tests::client_gen::functions;
use hostkeeper_ui_tests::{UiFile, call_sites, check_calls};
use prost::Message;
use prost_types::FileDescriptorSet;
use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// A run `id` in `state` (`RUNNING`, `DONE`, ...) of the host group `g`.
fn run(id: &str, state: &str) -> Value {
    json!({"id": id, "state": format!("MAINTENANCE_STATE_{state}"), "hostGroup": "g"})
}

/// A host's turn in `step` (`HALTED`, `REBOOTING`, ...), in the host group `g`.
fn turn(host: &str, step: &str) -> Value {
    json!({"hostId": host, "hostGroup": "g", "step": format!("MAINTENANCE_HOST_STEP_{step}")})
}

/// `base` with `fields` set over it.
fn with(mut base: Value, fields: Value) -> Value {
    for (key, value) in fields.as_object().expect("an object") {
        base[key] = value.clone();
    }
    base
}

/// A Hosts.List reply of hosts `(id, operating system, has an update due)` in the host group `g`,
/// each with a report.
fn fleet(hosts: &[(&str, &str, bool)]) -> Value {
    let hosts: Vec<Value> = hosts
        .iter()
        .map(|(id, os, due)| {
            let items = if *due {
                json!([{"kind": "MAINTENANCE_KIND_LINUX", "from": "1.0", "to": "1.1", "label": "libx", "security": true}])
            } else {
                json!([])
            };
            json!({"nodeId": id, "hostGroup": "g", "maintenanceLookAgeSecs": "600",
                   "maintenance": {"host": {"osName": os}, "due": items, "lookedUnix": "1799999400"}})
        })
        .collect();
    json!({ "hosts": hosts })
}

/// Scripts the read of the runs: Maintenance.List of `runs`, then Maintenance.Get of each that is
/// not done, with its hosts.
fn script_runs(ui: &Ui, runs: &[(Value, Vec<Value>)]) {
    let listed: Vec<Value> = runs.iter().map(|(run, _)| run.clone()).collect();
    ui.reply(&m("Maintenance/List"), json!({ "runs": listed }));
    for (run, hosts) in runs {
        if run["state"].as_str() != Some("MAINTENANCE_STATE_DONE") {
            ui.reply(&m("Maintenance/Get"), json!({"run": run, "hosts": hosts}));
        }
    }
}

/// The request bodies of the first read of the runs of `runs` (after Hosts.List).
fn read_of(runs: &[&str]) -> Vec<(String, Value)> {
    let mut out = vec![(m("Maintenance/List"), json!({"limit": 10}))];
    for id in runs {
        out.push((m("Maintenance/Get"), json!({ "runId": id })));
    }
    out
}

/// The driver's view of the runs in a tree: for each run its text lines, its host rows, the names
/// of its buttons and how many Reason fields it has.
const VIEW: &str = r#"
const view = (t) => named(t, "maintenance-run").map((run) => ({
  lines: nodes(run).filter((n) => n.tag === "p").map(text),
  rows: rows(run).slice(1),
  buttons: nodes(run).filter((n) => n.tag === "button").map((n) => n.attrs["name"]),
  reasons: nodes(run).filter((n) => n.attrs["name"] === "run-reason").length,
}));
const last = () => seen.shown[seen.shown.length - 1];"#;

fn drive(ui: &mut Ui, answers: &str, then: &str) -> Value {
    ui.run(&page_driver(
        "maintenance",
        "{}",
        answers,
        &[VIEW, then].join("\n"),
    ))
}

/// A halted run `r-1` (two halted hosts, a done one, a waiting one), a cancelled run `r-2` holding
/// a halted host and a not reached one, and a done run `r-0`.
fn halted_runs() -> Vec<(Value, Vec<Value>)> {
    vec![
        (
            run("r-1", "RUNNING"),
            vec![
                with(turn("h-1", "HALTED"), json!({"reason": "apply exited 1"})),
                with(turn("h-2", "HALTED"), json!({"reason": "drain exited 2"})),
                turn("h-3", "DONE"),
                turn("h-4", "WAITING"),
            ],
        ),
        (
            run("r-2", "CANCELLED"),
            vec![
                with(turn("h-5", "HALTED"), json!({"reason": "verify exited 3"})),
                turn("h-6", "NOT_REACHED"),
            ],
        ),
        (run("r-0", "DONE"), vec![]),
    ]
}

/// A gateway at the first read of `halted_runs`.
fn halted_ui() -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[]));
    script_runs(&ui, &halted_runs());
    ui
}

#[test]
fn the_run_view_shows_each_of_the_thirteen_steps_in_words() {
    // Asserts: one run whose hosts are in each of the 13 steps shows each step in words with the
    // time it entered it (a drain says "since"), a halted or deferred host's reason after its step,
    // a skipped host's why, "-" for a host with no time; the run's head says its state, group,
    // window, batch, reason and who scheduled it, and the group it halted in; a done run scheduled
    // on hosts says so, with no window and the group's batch, and holds no row and no button.
    let steps = [
        "WAITING",
        "STAGING",
        "DRAINING",
        "APPLYING",
        "REBOOTING",
        "CHECKING",
        "PUTTING_BACK",
        "DONE",
        "DEFERRED",
        "HALTED",
        "NOT_BACK",
        "NOT_REACHED",
        "SKIPPED",
    ];
    let hosts: Vec<Value> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let mut one = turn(&format!("n{:02}", i + 1), step);
            if (1..12).contains(&i) {
                one["sinceUnix"] = json!((1_799_990_000 + 100 * (i + 1)).to_string());
            }
            match *step {
                "DEFERRED" => one["reason"] = json!("drain deadline passed"),
                "HALTED" => one["reason"] = json!("verify exited 3"),
                "SKIPPED" => one["skippedBecause"] = json!("MAINTENANCE_SKIP_HELD"),
                _ => {}
            }
            one
        })
        .collect();
    let big = with(
        run("r-1", "RUNNING"),
        json!({"hostGroup": "linux-build", "windowStartUnix": "1800000000",
               "windowEndUnix": "1800003600", "batch": 2, "reason": "monthly", "byName": "ops",
               "haltedGroups": ["linux-build"]}),
    );
    let done = with(
        run("r-2", "DONE"),
        json!({"hostGroup": "", "hostIds": ["a", "b"]}),
    );
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/List"),
        fleet(&[("n05", "ubuntu", false), ("n11", "macOS", false)]),
    );
    script_runs(&ui, &[(big, hosts), (done, vec![])]);
    let out = drive(&mut ui, "[]", "return view(tree);");
    let row = |host: &str, step: &str, since: &str, actions: &str| {
        json!([host, "g", step, since, actions])
    };
    assert_eq!(
        out["out"],
        json!([
            {
                "lines": [
                    "Run r-1; running; group linux-build; window 2027-01-15 08:00:00 UTC to 2027-01-15 09:00:00 UTC; batch 2; reason: monthly; by ops",
                    "halted in: linux-build",
                ],
                "rows": [
                    row("n01", "waiting", "-", ""),
                    row("n02", "staging", "2027-01-15 05:16:40 UTC", ""),
                    row("n03", "draining since 2027-01-15 05:18:20 UTC", "2027-01-15 05:18:20 UTC", ""),
                    row("n04", "applying", "2027-01-15 05:20:00 UTC", ""),
                    row("n05", "rebooting", "2027-01-15 05:21:40 UTC", "Console"),
                    row("n06", "checking", "2027-01-15 05:23:20 UTC", ""),
                    row("n07", "putting back", "2027-01-15 05:25:00 UTC", ""),
                    row("n08", "done", "2027-01-15 05:26:40 UTC", ""),
                    row("n09", "deferred: drain deadline passed", "2027-01-15 05:28:20 UTC", ""),
                    row("n10", "halted: verify exited 3", "2027-01-15 05:30:00 UTC", "Retry Put back"),
                    row("n11", "not back", "2027-01-15 05:31:40 UTC", "Desktop"),
                    row("n12", "not reached", "2027-01-15 05:33:20 UTC", ""),
                    row("n13", "skipped: a package hold stands on it", "-", ""),
                ],
                "buttons": ["open-console", "retry-host", "put-back-host", "open-desktop", "cancel-run"],
                "reasons": 1,
            },
            {
                "lines": ["Run r-2; done; hosts a, b; window now to no end; batch the group's"],
                "rows": [],
                "buttons": [],
                "reasons": 0,
            },
        ])
    );
}

#[test]
fn a_restarting_host_links_its_console_or_its_desktop_and_no_other_step_links() {
    // Asserts: a host whose report says it is not macOS has a Console button and one whose report
    // says macOS has a Desktop button, whatever kinds its turn applies; a host with no report has
    // Desktop when its turn applies macOS and Console otherwise; a host not back has the same
    // link as one rebooting; hosts staging, applying, checking and done have no button; Console
    // opens the serial console page, and Desktop the desktop page, each with the host's node id.
    let mut mac_kinds = turn("k-1", "REBOOTING");
    mac_kinds["kinds"] = json!(["MAINTENANCE_KIND_MACOS"]);
    let mut linux_kinds = turn("k-2", "REBOOTING");
    linux_kinds["kinds"] = json!(["MAINTENANCE_KIND_LINUX"]);
    let mut reported_linux = turn("k-3", "REBOOTING");
    reported_linux["kinds"] = json!(["MAINTENANCE_KIND_MACOS"]);
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/List"),
        fleet(&[
            ("l-1", "ubuntu", false),
            ("m-1", "macOS", false),
            ("k-3", "ubuntu", false),
        ]),
    );
    script_runs(
        &ui,
        &[(
            run("r-1", "RUNNING"),
            vec![
                turn("l-1", "REBOOTING"),
                turn("m-1", "REBOOTING"),
                mac_kinds,
                linux_kinds,
                reported_linux,
                turn("m-1", "NOT_BACK"),
                turn("s-1", "STAGING"),
                turn("a-1", "APPLYING"),
                turn("c-1", "CHECKING"),
                turn("d-1", "DONE"),
            ],
        )],
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
const links = named(tree, "run-host").map((tr) => ({
  host: text(tr.children[0]),
  links: nodes(tr).filter((n) => n.tag === "button").map((n) => n.attrs["name"] + ":" + text(n)),
}));
await click(tree, "open-console", 0);
await click(tree, "open-desktop", 0);
return { links, navigations: seen.navigations };"#,
    );
    let link = |host: &str, links: Value| json!({"host": host, "links": links});
    assert_eq!(
        out["out"],
        json!({
            "links": [
                link("l-1", json!(["open-console:Console"])),
                link("m-1", json!(["open-desktop:Desktop"])),
                link("k-1", json!(["open-desktop:Desktop"])),
                link("k-2", json!(["open-console:Console"])),
                link("k-3", json!(["open-console:Console"])),
                link("m-1", json!(["open-desktop:Desktop"])),
                link("s-1", json!([])),
                link("a-1", json!([])),
                link("c-1", json!([])),
                link("d-1", json!([])),
            ],
            "navigations": [
                {"name": "serialconsole", "params": {"nodeId": "l-1"}},
                {"name": "desktop", "params": {"nodeId": "m-1"}},
            ],
        })
    );
}

#[test]
fn retry_and_put_back_are_on_the_halted_hosts_and_an_ended_run_has_no_retry() {
    // Asserts: exactly the halted hosts' rows have Retry and Put back; in a cancelled run the
    // halted host has Put back alone (a retry of an ended run is refused); the run that is not
    // ended has a Reason field and Cancel, the cancelled run with a halted host has the field and
    // no Cancel, the done run has neither; no button anywhere is a pause.
    let mut ui = halted_ui();
    let out = drive(
        &mut ui,
        "[]",
        r#"
const buttons = nodes(tree).filter((n) => n.tag === "button");
return {
  runs: view(tree),
  pause: buttons.filter((n) => /pause/i.test(text(n) + n.attrs["name"])).length,
  retries: named(tree, "retry-host").length,
  putBacks: named(tree, "put-back-host").length,
};"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "runs": [
                {
                    "lines": ["Run r-1; running; group g; window now to no end; batch the group's"],
                    "rows": [
                        ["h-1", "g", "halted: apply exited 1", "-", "Retry Put back"],
                        ["h-2", "g", "halted: drain exited 2", "-", "Retry Put back"],
                        ["h-3", "g", "done", "-", ""],
                        ["h-4", "g", "waiting", "-", ""],
                    ],
                    "buttons": ["retry-host", "put-back-host", "retry-host", "put-back-host", "cancel-run"],
                    "reasons": 1,
                },
                {
                    "lines": ["Run r-2; cancelled; group g; window now to no end; batch the group's"],
                    "rows": [
                        ["h-5", "g", "halted: verify exited 3", "-", "Put back"],
                        ["h-6", "g", "not reached", "-", ""],
                    ],
                    "buttons": ["put-back-host"],
                    "reasons": 1,
                },
                {
                    "lines": ["Run r-0; done; group g; window now to no end; batch the group's"],
                    "rows": [],
                    "buttons": [],
                    "reasons": 0,
                },
            ],
            "pause": 0,
            "retries": 2,
            "putBacks": 3,
        })
    );
    assert_eq!(
        bodies(&ui),
        [vec![(m("Hosts/List"), json!({}))], read_of(&["r-1", "r-2"]),].concat()
    );
}

#[test]
fn retry_needs_the_runs_reason_asks_once_and_sends_run_host_and_reason() {
    // Asserts: Retry of the second halted host with no reason says "Give a reason first." and
    // asks nothing and calls nothing; with the run's reason typed it asks once, and when the
    // operator says no says "Not retried." and calls nothing; when the operator says yes it sends
    // exactly the run id, the host id and the trimmed reason to Maintenance.RetryHost, says the
    // host was retried, and reads the runs again, which the page then shows.
    let mut ui = halted_ui();
    ui.reply(
        &m("Maintenance/RetryHost"),
        json!({"host": turn("h-2", "APPLYING")}),
    );
    let mut again = halted_runs();
    again[0].1[1] = turn("h-2", "APPLYING");
    script_runs(&ui, &again);
    let out = drive(
        &mut ui,
        "[false, true]",
        r#"
await click(tree, "retry-host", 1);
type(tree, "run-reason", "  disk swapped ", 0);
await click(tree, "retry-host", 1);
await click(tree, "retry-host", 1);
return { reports: seen.reports, questions: seen.questions, after: view(last())[0], runs: view(last()).length };"#,
    );
    let question = "Retry h-2 from the step that failed? It is drained again if needed and the update goes on.";
    assert_eq!(
        out["out"],
        json!({
            "reports": ["Give a reason first.", "Not retried.", "Retried h-2 in run r-1."],
            "questions": [question, question],
            "after": {
                "lines": ["Run r-1; running; group g; window now to no end; batch the group's"],
                "rows": [
                    ["h-1", "g", "halted: apply exited 1", "-", "Retry Put back"],
                    ["h-2", "g", "applying", "-", ""],
                    ["h-3", "g", "done", "-", ""],
                    ["h-4", "g", "waiting", "-", ""],
                ],
                "buttons": ["retry-host", "put-back-host", "cancel-run"],
                "reasons": 1,
            },
            "runs": 3,
        })
    );
    assert_eq!(
        bodies(&ui),
        [
            vec![(m("Hosts/List"), json!({}))],
            read_of(&["r-1", "r-2"]),
            vec![(
                m("Maintenance/RetryHost"),
                json!({"runId": "r-1", "hostId": "h-2", "reason": "disk swapped"}),
            )],
            read_of(&["r-1", "r-2"]),
        ]
        .concat()
    );
}

#[test]
fn a_refused_retry_is_shown_as_the_refusal_and_the_runs_are_not_read_again() {
    // Asserts: when Maintenance.RetryHost is refused the page reports the refusal's code and
    // reason as they are, and does not read the runs again.
    let mut ui = halted_ui();
    ui.refuse(
        &m("Maintenance/RetryHost"),
        412,
        "FAILED_PRECONDITION",
        "host_not_halted",
    );
    let out = drive(
        &mut ui,
        "[true]",
        r#"
type(tree, "run-reason", "again", 0);
await click(tree, "retry-host", 0);
return { reports: seen.reports, shown: seen.shown.length };"#,
    );
    assert_eq!(
        out["out"],
        json!({"reports": ["FAILED_PRECONDITION host_not_halted"], "shown": 0})
    );
    assert_eq!(
        bodies(&ui).last(),
        Some(&(
            m("Maintenance/RetryHost"),
            json!({"runId": "r-1", "hostId": "h-1", "reason": "again"})
        ))
    );
}

#[test]
fn put_back_in_an_ended_run_needs_that_runs_reason_asks_once_and_sends_run_host_and_reason() {
    // Asserts: Put back of the halted host of the cancelled run, with a reason typed only in the
    // other run's field, says "Give a reason first." (each run keeps its own reason); with its own
    // run's reason typed, when the operator says no says "Not put back." and calls nothing; when
    // the operator says yes it sends exactly the run id, the host id and the reason to
    // Maintenance.PutBack, says the host was put back, and reads the runs again, in which the
    // cancelled run has no halted host and so no Reason field.
    let mut ui = halted_ui();
    ui.reply(
        &m("Maintenance/PutBack"),
        json!({"host": turn("h-5", "DONE")}),
    );
    script_runs(
        &ui,
        &[(
            run("r-2", "CANCELLED"),
            vec![turn("h-5", "DONE"), turn("h-6", "NOT_REACHED")],
        )],
    );
    let out = drive(
        &mut ui,
        "[false, true]",
        r#"
type(tree, "run-reason", "other run", 0);
await click(tree, "put-back-host", 2);
type(tree, "run-reason", "host replaced", 1);
await click(tree, "put-back-host", 2);
await click(tree, "put-back-host", 2);
return { reports: seen.reports, questions: seen.questions, after: view(last()) };"#,
    );
    let question = "Put h-5 back? Its drained services' resume runs, the update stays as far as it got, and the run goes on.";
    assert_eq!(
        out["out"],
        json!({
            "reports": ["Give a reason first.", "Not put back.", "Put h-5 back in run r-2."],
            "questions": [question, question],
            "after": [{
                "lines": ["Run r-2; cancelled; group g; window now to no end; batch the group's"],
                "rows": [
                    ["h-5", "g", "done", "-", ""],
                    ["h-6", "g", "not reached", "-", ""],
                ],
                "buttons": [],
                "reasons": 0,
            }],
        })
    );
    assert_eq!(
        bodies(&ui),
        [
            vec![(m("Hosts/List"), json!({}))],
            read_of(&["r-1", "r-2"]),
            vec![(
                m("Maintenance/PutBack"),
                json!({"runId": "r-2", "hostId": "h-5", "reason": "host replaced"}),
            )],
            read_of(&["r-2"]),
        ]
        .concat()
    );
}

#[test]
fn cancel_is_on_the_runs_not_ended_and_sends_the_run_and_the_reason_after_one_question() {
    // Asserts: Cancel is on the scheduled, running and halted runs and not on the done, cancelled
    // or window ended ones; each run's state is said in words; Cancel with no reason says "Give a
    // reason first." and one with a reason that has a line break says the reason's rule, each
    // asking nothing and calling nothing; with a good reason it asks once, and when the operator
    // says no says "Not cancelled." and calls nothing; when the operator says yes it sends exactly
    // the run id and the reason to Maintenance.Cancel, says the run was cancelled, and reads the
    // runs again; no button is a pause.
    let states = [
        ("r-1", "RUNNING", vec![turn("h-1", "APPLYING")]),
        (
            "r-2",
            "CANCELLED",
            vec![with(turn("h-2", "HALTED"), json!({"reason": "x"}))],
        ),
        ("r-3", "DONE", vec![]),
        ("r-4", "WINDOW_ENDED", vec![turn("h-4", "NOT_REACHED")]),
        ("r-5", "SCHEDULED", vec![turn("h-5", "WAITING")]),
        ("r-6", "HALTED", vec![]),
    ];
    let runs: Vec<(Value, Vec<Value>)> = states
        .into_iter()
        .map(|(id, state, hosts)| (run(id, state), hosts))
        .collect();
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[]));
    script_runs(&ui, &runs);
    ui.reply(
        &m("Maintenance/Cancel"),
        json!({"run": run("r-5", "CANCELLED")}),
    );
    script_runs(
        &ui,
        &[(run("r-5", "CANCELLED"), vec![turn("h-5", "NOT_REACHED")])],
    );
    let out = drive(
        &mut ui,
        "[false, true]",
        r#"
const first = view(tree);
const pause = nodes(tree).filter((n) => n.tag === "button" && /pause/i.test(text(n) + n.attrs["name"])).length;
await click(tree, "cancel-run", 1);
type(tree, "run-reason", "a\nb", 2);
await click(tree, "cancel-run", 1);
type(tree, "run-reason", "freeze", 2);
await click(tree, "cancel-run", 1);
await click(tree, "cancel-run", 1);
return {
  states: first.map((r) => r.lines[0].split("; ")[1]),
  cancels: first.map((r) => r.buttons.includes("cancel-run")),
  fields: first.map((r) => r.reasons),
  pause,
  reports: seen.reports,
  questions: seen.questions,
  after: view(last()),
};"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "states": ["running", "cancelled", "done", "window ended", "scheduled", "halted"],
            "cancels": [true, false, false, false, true, true],
            "fields": [1, 1, 0, 0, 1, 1],
            "pause": 0,
            "reports": [
                "Give a reason first.",
                "A reason has no control character (a tab or a line break, for example).",
                "Not cancelled.",
                "Cancelled run r-5.",
            ],
            "questions": [
                "Cancel run r-5? No new host starts; a host in its turn finishes.",
                "Cancel run r-5? No new host starts; a host in its turn finishes.",
            ],
            "after": [{
                "lines": ["Run r-5; cancelled; group g; window now to no end; batch the group's"],
                "rows": [["h-5", "g", "not reached", "-", ""]],
                "buttons": [],
                "reasons": 0,
            }],
        })
    );
    assert_eq!(
        bodies(&ui),
        [
            vec![(m("Hosts/List"), json!({}))],
            read_of(&["r-1", "r-2", "r-4", "r-5", "r-6"]),
            vec![(
                m("Maintenance/Cancel"),
                json!({"runId": "r-5", "reason": "freeze"}),
            )],
            read_of(&["r-5"]),
        ]
        .concat()
    );
}

#[test]
fn the_runs_are_read_after_the_hosts_and_a_done_run_is_not_read_for_its_hosts() {
    // Asserts: the page reads Hosts.List, then the last 10 runs with Maintenance.List, then
    // Maintenance.Get of each run that is not done, in list order and no other; a run list that is
    // empty says "No maintenance run."; a Hosts.List that fails shows its refusal with no call of
    // the runs.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[]));
    script_runs(
        &ui,
        &[
            (run("r-d", "DONE"), vec![]),
            (run("r-1", "RUNNING"), vec![turn("h-1", "APPLYING")]),
            (run("r-2", "HALTED"), vec![]),
        ],
    );
    drive(&mut ui, "[]", "return null;");
    assert_eq!(
        bodies(&ui),
        [vec![(m("Hosts/List"), json!({}))], read_of(&["r-1", "r-2"]),].concat()
    );
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[]));
    script_runs(&ui, &[]);
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const card = named(tree, "maintenance-runs")[0];
return { quiet: nodes(card).filter((n) => n.attrs["class"] === "quiet").map(text), runs: nodes(card).filter((n) => n.attrs["name"] === "maintenance-run").length };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"quiet": ["No maintenance run."], "runs": 0})
    );
    assert_eq!(
        bodies(&ui),
        [vec![(m("Hosts/List"), json!({}))], read_of(&[])].concat()
    );

    let mut ui = Ui::start();
    ui.refuse(&m("Hosts/List"), 503, "UNAVAILABLE", "store_failed");
    ui.run(&page_driver("maintenance", "{}", "[]", "return null;"));
    assert_eq!(ui.methods(), [m("Hosts/List")]);
}

#[test]
fn a_failed_run_list_leaves_the_due_list_and_says_the_runs_could_not_be_read() {
    // Asserts: when Maintenance.List is refused the page still shows the fleet's count and the
    // group's due host with its Update button, says "The runs could not be read (UNAVAILABLE
    // store_failed)." as an alert, shows no run and reads no run's hosts.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[("k-1", "ubuntu", true)]));
    ui.refuse(&m("Maintenance/List"), 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        r#"
const failure = named(tree, "runs-failure")[0];
return {
  failure: text(failure),
  role: failure.attrs["role"],
  runs: nodes(tree).filter((n) => n.attrs["name"] === "maintenance-run").length,
  fleet: text(named(tree, "fleet-due")[0]),
  rows: rows(named(tree, "maintenance-group")[0]).slice(1),
};"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "failure": "The runs could not be read (UNAVAILABLE store_failed).",
            "role": "alert",
            "runs": 0,
            "fleet": "Updates due on 1 host in 1 group",
            "rows": [["k-1", "1 security update", "10 min ago", "Update"]],
        })
    );
    assert_eq!(
        bodies(&ui),
        [vec![(m("Hosts/List"), json!({}))], read_of(&[])].concat()
    );
}

#[test]
fn a_failed_get_says_that_runs_hosts_could_not_be_read_and_the_other_run_is_shown() {
    // Asserts: when Maintenance.Get of one run is refused that run shows its head and "Its hosts
    // could not be read: NOT_FOUND no_such_run" as an alert with no host rows (and still its
    // Cancel), and the next run's hosts are read and shown.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[]));
    let first = run("r-1", "RUNNING");
    let second = run("r-2", "RUNNING");
    ui.reply(
        &m("Maintenance/List"),
        json!({"runs": [first.clone(), second.clone()]}),
    );
    ui.refuse(&m("Maintenance/Get"), 404, "NOT_FOUND", "no_such_run");
    ui.reply(
        &m("Maintenance/Get"),
        json!({"run": second, "hosts": [turn("h-2", "WAITING")]}),
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
const alerts = named(tree, "maintenance-run").map((run) => nodes(run).filter((n) => n.attrs["role"] === "alert").map(text));
return { runs: view(tree), alerts };"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "runs": [
                {
                    "lines": [
                        "Run r-1; running; group g; window now to no end; batch the group's",
                        "Its hosts could not be read: NOT_FOUND no_such_run",
                    ],
                    "rows": [],
                    "buttons": ["cancel-run"],
                    "reasons": 1,
                },
                {
                    "lines": ["Run r-2; running; group g; window now to no end; batch the group's"],
                    "rows": [["h-2", "g", "waiting", "-", ""]],
                    "buttons": ["cancel-run"],
                    "reasons": 1,
                },
            ],
            "alerts": [["Its hosts could not be read: NOT_FOUND no_such_run"], []],
        })
    );
}

#[test]
fn the_runs_are_read_again_every_ten_seconds_and_show_the_new_step() {
    // Asserts: the page asks the frame to repeat its hosts read every 60 seconds and, after it,
    // its runs read every 10; running the second repeat sends Maintenance.List and Maintenance.Get
    // again (and no Hosts.List) and shows the new step, with the Console button a host that now
    // reboots has; a repeat whose Maintenance.List is refused says the runs could not be read.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/List"), fleet(&[("h-1", "ubuntu", false)]));
    script_runs(
        &ui,
        &[(run("r-1", "RUNNING"), vec![turn("h-1", "APPLYING")])],
    );
    script_runs(
        &ui,
        &[(run("r-1", "RUNNING"), vec![turn("h-1", "REBOOTING")])],
    );
    ui.refuse(&m("Maintenance/List"), 503, "UNAVAILABLE", "store_failed");
    let out = drive(
        &mut ui,
        "[]",
        r#"
const seconds = seen.repeats.map((r) => r.seconds);
const before = view(tree)[0].rows;
await seen.repeats[1].run();
const after = view(last())[0].rows;
await seen.repeats[1].run();
return { seconds, before, after, failed: text(named(last(), "runs-failure")[0]) };"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "seconds": [60, 10],
            "before": [["h-1", "g", "applying", "-", ""]],
            "after": [["h-1", "g", "rebooting", "-", "Console"]],
            "failed": "The runs could not be read (UNAVAILABLE store_failed).",
        })
    );
    assert_eq!(
        bodies(&ui),
        [
            vec![(m("Hosts/List"), json!({}))],
            read_of(&["r-1"]),
            read_of(&["r-1"]),
            read_of(&[]),
        ]
        .concat()
    );
}

/// The Maintenance methods the calls of the module `path` under `ui/` make, in the order written,
/// each as the descriptor method it is; each of the module's calls is a descriptor method.
fn maintenance_methods(path: &str) -> Vec<String> {
    let file = UiFile::named(path).unwrap_or_else(|| panic!("{path} is held"));
    let sites = call_sites(file.path, file.text).unwrap_or_else(|e| panic!("{e}"));
    let all = functions(COMMANDS);
    let methods: Vec<String> = sites
        .iter()
        .filter(|site| site.function.starts_with("maintenance."))
        .map(|site| {
            all.iter()
                .find(|f| f.call() == site.function)
                .unwrap_or_else(|| panic!("api.{} is not a function of the client", site.function))
                .method
                .clone()
        })
        .collect();
    let descriptors = FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("hostkeeper-proto's descriptors decode");
    assert_eq!(check_calls(&methods, &descriptors), Ok(()), "{path}");
    methods
}

#[test]
fn the_maintenance_calls_of_the_pages_are_descriptor_methods_and_the_new_modules_call_nothing() {
    // Asserts: the Maintenance page calls Maintenance.List, Get, Schedule, Cancel, RetryHost and
    // PutBack, the Home page and the host page call Maintenance.Schedule (for their Update button
    // and Schedule form), each a method of the protobuf descriptors through the generated client;
    // the modules of the Schedule form and of the runs section call nothing. (`calls.json` listing
    // them is held by `calls_json_is_derived_from_the_pages` in `client.rs`.)
    let op = "hostkeeper.operator.v1.Maintenance";
    assert_eq!(
        maintenance_methods("pages/maintenance.js"),
        ["List", "Get", "Schedule", "Cancel", "RetryHost", "PutBack"]
            .map(|name| format!("{op}/{name}"))
    );
    assert_eq!(
        maintenance_methods("pages/home.js"),
        [format!("{op}/Schedule")]
    );
    assert_eq!(
        maintenance_methods("pages/host.js"),
        [format!("{op}/Schedule")]
    );
    for module in ["schedule.js", "runs.js"] {
        let file = UiFile::named(module).expect("the module is held");
        assert_eq!(call_sites(file.path, file.text), Ok(vec![]), "{module}");
    }
}
