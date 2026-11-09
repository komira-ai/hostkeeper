//! The runner's failures and boundaries against the simulated fleet: what ends a repetition, what
//! stops a run before any change, what a poll that fails leaves, and what the records never hold.

use std::cell::RefCell;
use std::io::{self, BufRead, Read, Write};
use std::rc::Rc;

use hostkeeper_measure::coordinator::Answer;
use hostkeeper_measure::dry::dry_run;
use hostkeeper_measure::record::Record;
use hostkeeper_measure::runner::{self, Io, Names, RunError};

use crate::plans::BASE;
use crate::support::{self, FakeClock, Fleet, Operator, SimHost, T0_MS, refused};

fn fleet(hosts: Vec<SimHost>) -> Rc<RefCell<Fleet>> {
    let fleet = Fleet::new(Rc::new(FakeClock::default()), hosts);
    let fleet = Rc::new(RefCell::new(fleet));
    {
        let mut f = fleet.borrow_mut();
        f.world.first_order_ms = 1_000;
        f.world.update_ms = 3_000;
    }
    fleet
}

fn one_hello() -> Rc<RefCell<Fleet>> {
    fleet(vec![SimHost::new("node-a", 0, &[("hello", "v1")])])
}

/// A record as one line: repetition, step, event, host and detail.
fn line(r: &Record) -> String {
    let detail: Vec<String> = r.detail.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!(
        "{} {} {} {} {}",
        r.rep,
        r.step,
        r.event,
        r.host.as_deref().unwrap_or("-"),
        detail.join(",")
    )
    .trim_end()
    .to_owned()
}

fn lines(records: &[Record]) -> Vec<String> {
    records.iter().map(line).collect()
}

fn base_params() -> hostkeeper_measure::params::Params {
    support::params(&["version@1=v2", "version@2=v3"])
}

#[test]
fn a_failed_call_ends_its_repetition_and_the_next_repetition_runs() {
    let fleet = one_hello();
    fleet.borrow_mut().once.push((
        "releases set-desired".to_owned(),
        refused(2, "below_min_version"),
    ));
    let plan = support::plan(BASE);
    let ran = support::run(
        &plan,
        &base_params(),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    let summary = ran.result.expect("the run ends");
    assert_eq!(summary.complete, 1);
    assert_eq!(summary.ended, vec![(1, "set failed".to_owned())]);
    assert_eq!(
        lines(&ran.records),
        [
            "1 repetition started - hosts=1",
            "1 set call-failed - code=2,refused=below_min_version",
            "1 repetition ended - complete=false,why=set failed",
            "2 repetition started - hosts=1",
            "2 set called - audit_seq=1",
            "2 wait ordered host-1",
            "2 wait updated host-1",
            "2 repetition ended - complete=true",
        ]
    );
    let called = &ran.records[4];
    assert_eq!(called.coordinator_unix, Some((T0_MS + 60_000) / 1000));
    assert_eq!(called.runner_ms, T0_MS + 60_000);
    // The set's slots are filled per repetition.
    let sets: Vec<Vec<String>> = fleet
        .borrow()
        .calls
        .iter()
        .filter(|c| c[0] == "releases")
        .cloned()
        .collect();
    assert_eq!(
        sets,
        [
            ["releases", "set-desired", "--x", "v2"]
                .map(str::to_owned)
                .to_vec(),
            ["releases", "set-desired", "--x", "v3"]
                .map(str::to_owned)
                .to_vec(),
        ]
    );
    assert_eq!(
        ran.console,
        "[repetition 1] repetition: started\n[repetition 1] set: call-failed\n[repetition 1] repetition: ended\n[repetition 2] repetition: started\n[repetition 2] set: called\n[repetition 2] wait: ordered host-1\n[repetition 2] wait: updated host-1\n[repetition 2] repetition: ended\n"
    );
}

#[test]
fn a_watch_that_times_out_ends_its_repetition() {
    let fleet = one_hello();
    fleet.borrow_mut().world.update_ms = 60_000;
    let plan = support::plan(BASE);
    let ran = support::run(
        &plan,
        &base_params(),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    let summary = ran.result.expect("the run ends");
    assert_eq!(summary.complete, 0);
    assert_eq!(
        summary.ended,
        vec![
            (1, "wait timed out".to_owned()),
            (2, "wait timed out".to_owned())
        ]
    );
    assert_eq!(
        lines(&ran.records),
        [
            "1 repetition started - hosts=1",
            "1 set called - audit_seq=1",
            "1 wait ordered host-1",
            "1 wait timed-out -",
            "1 repetition ended - complete=false,why=wait timed out",
            "2 repetition started - hosts=1",
            "2 set called - audit_seq=2",
            "2 wait ordered host-1",
            "2 wait timed-out -",
            "2 repetition ended - complete=false,why=wait timed out",
        ]
    );
    // The timeout is at the plan's 10 s, on the fake clock: polls at 0 to 10 s.
    let timed_out = &ran.records[3];
    assert_eq!(timed_out.runner_ms, T0_MS + 70_000);
}

/// The coordinator is down from 61 s to 62 s, while it orders the host (61 s): the poll at 61 s
/// fails, the restarted coordinator lists the host again only at its verdict (64 s), so the
/// order is never seen and is missing from the records rather than made up.
#[test]
fn a_poll_that_fails_is_recorded_and_the_watch_goes_on() {
    let fleet = one_hello();
    fleet.borrow_mut().world.down.push((61_000, 62_000));
    let plan = support::plan(&BASE.replace("repetitions = 2", "repetitions = 1"));
    let ran = support::run(
        &plan,
        &support::params(&["version@1=v2"]),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    ran.result.expect("the run ends");
    assert_eq!(
        lines(&ran.records),
        [
            "1 repetition started - hosts=1",
            "1 set called - audit_seq=1",
            "1 wait call-failed - code=1",
            "1 wait updated host-1",
            "1 repetition ended - complete=true",
        ]
    );
}

#[test]
fn hosts_not_as_many_as_the_plan_says_stop_the_run_before_any_change() {
    let fleet = fleet(vec![
        SimHost::new("node-a", 0, &[("hello", "v1")]),
        SimHost::new("node-b", 0, &[("hello", "v1")]),
    ]);
    let plan = support::plan(BASE);
    let ran = support::run(
        &plan,
        &base_params(),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    let err = ran.result.expect_err("two hosts for a plan of one");
    assert_eq!(
        err.to_string(),
        "the plan's hosts: exactly 1, but the coordinator lists 2"
    );
    assert!(ran.records.is_empty());
    let calls: Vec<String> = fleet.borrow().calls.iter().map(|c| c.join(" ")).collect();
    assert_eq!(calls, ["hosts list --limit 256"]);

    let fleet = fleet_of_none();
    let plan = support::plan(&BASE.replace("exactly = 1\n", ""));
    let err = support::run(
        &plan,
        &base_params(),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    )
    .result
    .expect_err("no host of hello");
    assert_eq!(
        err.to_string(),
        "the plan's hosts: at least one, but the coordinator lists 0"
    );
}

fn fleet_of_none() -> Rc<RefCell<Fleet>> {
    fleet(vec![SimHost::new("node-a", 0, &[("hostkeeper", "v1")])])
}

#[test]
fn an_answer_the_run_needs_and_does_not_get_stops_the_run() {
    let plan = support::plan(BASE);
    let cases = [
        (
            "hosts list",
            refused(3, "unauthorized"),
            "`hkctl hosts list` failed (exit 3, refused: unauthorized), and the run needs its answer",
        ),
        (
            "hosts list",
            Answer {
                code: 1,
                stdout: String::new(),
                stderr: "curl: (7)\n".to_owned(),
            },
            "`hkctl hosts list` failed (exit 1), and the run needs its answer",
        ),
        (
            "audit list --newest",
            refused(3, "forbidden"),
            "`hkctl audit list --newest true --limit 1` failed (exit 3, refused: forbidden), and the run needs its answer",
        ),
        (
            "hosts list",
            Answer {
                code: 0,
                stdout: "not json".to_owned(),
                stderr: String::new(),
            },
            "`hkctl hosts list --limit 256` answered what is not the gateway's JSON: expected ident at line 1 column 2",
        ),
    ];
    for (prefix, answer, want) in cases {
        let fleet = one_hello();
        fleet.borrow_mut().once.push((prefix.to_owned(), answer));
        let ran = support::run(
            &plan,
            &base_params(),
            &fleet,
            &mut Operator::new(Rc::clone(&fleet)),
        );
        assert_eq!(ran.result.expect_err(prefix).to_string(), want);
    }
}

#[test]
fn a_call_whose_audit_entry_is_not_found_is_stamped_by_the_runner_alone() {
    let fleet = one_hello();
    let plan = support::plan(
        &BASE
            .replace("audit = \"set_desired\"", "audit = \"never_written\"")
            .replace("repetitions = 2", "repetitions = 1"),
    );
    let ran = support::run(
        &plan,
        &support::params(&["version@1=v2"]),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    ran.result.expect("the run ends");
    assert_eq!(line(&ran.records[1]), "1 set called - audit=not found");
    assert_eq!(ran.records[1].coordinator_unix, None);
}

/// A plan with one live host, an operator step and a call naming a host from the audit.
const LIVE: &str = r#"
id = "live"
title = "Live"
measures = "M."
records = "R."
repetitions = 1
states = ["{n.max}"]

[hosts]
says = "Every live host."
pick = "live"

[[step]]
id = "ask"
do = "operator"
says = "Do it in repetition {rep}."

[[step]]
id = "seen"
do = "watch"
until = { what = "audit", actions = ["cert_renewed"] }
poll_secs = 1
timeout_secs = 5

[[step]]
id = "get"
do = "call"
args = ["hosts", "get", "--node-id", "{host}"]

[[measure]]
id = "n"
kind = "count"
says = "calls"
unit = "calls"
of = "get/called"
"#;

#[test]
fn a_step_naming_a_host_the_audit_did_not_give_stops_the_run() {
    let fleet = one_hello();
    let plan = support::plan(LIVE);
    let mut operator = Operator::new(Rc::clone(&fleet)).then(|f, now| {
        f.world.audit_at(now, "cert_renewed", None, "coordinator");
    });
    let ran = support::run(&plan, &support::params(&[]), &fleet, &mut operator);
    assert_eq!(
        ran.result.expect_err("no host").to_string(),
        "step \"get\" names {host}, and no host is known yet"
    );
    assert!(
        ran.console
            .contains("[repetition 1] ask: Do it in repetition 1.\nPress Enter when it is done.\n")
    );

    let fleet = one_hello();
    let mut operator = Operator::new(Rc::clone(&fleet)).then(|f, now| {
        f.world
            .audit_at(now, "cert_renewed", Some("node-a"), "coordinator");
    });
    let ran = support::run(&plan, &support::params(&[]), &fleet, &mut operator);
    ran.result.expect("the host is known");
    assert_eq!(
        lines(&ran.records),
        [
            "1 repetition started - hosts=1",
            "1 ask done -",
            "1 seen seen host-1 action=cert_renewed",
            "1 get called -",
            "1 repetition ended - complete=true",
        ]
    );
    let calls: Vec<String> = fleet.borrow().calls.iter().map(|c| c.join(" ")).collect();
    assert_eq!(
        calls.last().map(String::as_str),
        Some("hosts get --node-id node-a")
    );
}

#[test]
fn an_audit_watch_reads_on_past_entries_of_other_actions_and_through_failed_polls() {
    let fleet = one_hello();
    let plan = support::plan(LIVE);
    let mut operator = Operator::new(Rc::clone(&fleet)).then(|f, now| {
        f.world
            .audit_at(now, "set_labels", Some("node-a"), "operator");
        f.once.push((
            "audit list --since-seq".to_owned(),
            refused(1, "unavailable"),
        ));
        f.at(
            now + 2_000,
            Box::new(move |w| {
                w.audit_at(now + 2_000, "cert_renewed", Some("node-a"), "coordinator")
            }),
        );
    });
    let ran = support::run(&plan, &support::params(&[]), &fleet, &mut operator);
    ran.result.expect("the run ends");
    assert_eq!(
        lines(&ran.records)[2..4],
        [
            "1 seen call-failed - code=1,refused=unavailable",
            "1 seen seen host-1 action=cert_renewed",
        ]
    );
    let since: Vec<String> = fleet
        .borrow()
        .calls
        .iter()
        .filter(|c| {
            c.starts_with(&[
                "audit".to_owned(),
                "list".to_owned(),
                "--since-seq".to_owned(),
            ])
        })
        .map(|c| c[3].clone())
        .collect();
    // From the repetition's start (no entry), then past the entry of another action.
    assert_eq!(since, ["0", "0", "1"]);
}

#[test]
fn standard_input_that_ends_or_fails_stops_the_run() {
    let fleet = one_hello();
    let plan = support::plan(LIVE);
    let ran = support::run(
        &plan,
        &support::params(&[]),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    assert_eq!(
        ran.result.expect_err("no operator").to_string(),
        "standard input ended while waiting for the operator"
    );
    let err = run_with(&plan, &mut Broken, &mut Vec::new(), &mut |_| Ok(()));
    assert_eq!(
        err.expect_err("stdin fails").to_string(),
        "reading standard input: broken"
    );
}

#[test]
fn a_console_or_a_record_that_cannot_be_written_stops_the_run() {
    let plan = support::plan(LIVE);
    let mut lines = io::Cursor::new(b"\n".to_vec());
    let err = run_with(&plan, &mut lines, &mut Broken, &mut |_| Ok(()));
    assert_eq!(
        err.expect_err("no console").to_string(),
        "writing to the console: broken"
    );
    let mut lines = io::Cursor::new(b"\n".to_vec());
    let err = run_with(&plan, &mut lines, &mut Vec::new(), &mut |_| {
        Err(io::Error::other("full"))
    });
    assert_eq!(
        err.expect_err("no records").to_string(),
        "writing a record: full"
    );
}

/// A reader and writer that always fail.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
}

impl BufRead for Broken {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::other("broken"))
    }
    fn consume(&mut self, _: usize) {}
}

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("broken"))
    }
}

/// Runs `plan` on a fleet of one live host with the given operator, console and records.
fn run_with(
    plan: &hostkeeper_measure::plan::Plan,
    operator: &mut dyn BufRead,
    console: &mut dyn Write,
    records: &mut dyn FnMut(&Record) -> io::Result<()>,
) -> Result<runner::Summary, RunError> {
    let fleet = one_hello();
    let clock = Rc::clone(&fleet.borrow().clock);
    let mut coordinator = support::Shared(Rc::clone(&fleet));
    runner::run(
        plan,
        &support::params(&[]),
        Io {
            coordinator: &mut coordinator,
            clock: &*clock,
            operator,
            console,
            records,
            names: &mut Names::default(),
        },
    )
}

#[test]
fn a_parameter_file_that_cannot_be_read_stops_the_run_before_any_call() {
    let fleet = one_hello();
    let plan = support::shipped("rollout");
    let missing = support::crate_dir().join("tests/no-such-index.json");
    let mut items = Vec::new();
    for k in 1..=3 {
        items.push(format!("version@{k}=v{}", k + 1));
        items.push(format!("index@{k}={}", missing.display()));
    }
    let params = hostkeeper_measure::params::Params::parse(&items).expect("parameters");
    let ran = support::run(
        &plan,
        &params,
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    let err = ran.result.expect_err("no index");
    assert!(
        err.to_string().starts_with(&format!(
            "parameter \"index\": reading {}: ",
            missing.display()
        )),
        "{err}"
    );
    assert!(fleet.borrow().calls.is_empty());
    let err = dry_run(&plan, &params, &mut Vec::new()).expect_err("no index");
    assert!(err.to_string().starts_with("parameter \"index\""), "{err}");
}

#[test]
fn parameters_that_do_not_fit_the_plan_stop_the_run_and_the_dry_run() {
    let fleet = one_hello();
    let plan = support::plan(BASE);
    let params = support::params(&["version@1=v2"]);
    let ran = support::run(
        &plan,
        &params,
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    assert_eq!(
        ran.result.expect_err("no version 2").to_string(),
        "no value of \"version\" for repetition 2"
    );
    assert!(fleet.borrow().calls.is_empty());
    assert_eq!(
        dry_run(&plan, &params, &mut Vec::new())
            .expect_err("no version 2")
            .to_string(),
        "no value of \"version\" for repetition 2"
    );
}

#[test]
fn the_records_and_the_console_name_hosts_only_as_host_n_and_the_names_file_maps_them() {
    let fleet = fleet(vec![
        SimHost::new("node-zeta", 0, &[("hostkeeper", "v1")]),
        SimHost::new("node-alpha", 4_000, &[("hostkeeper", "v1")]),
        SimHost::new("node-mid", 8_000, &[("hostkeeper", "v1")]),
    ]);
    let plan = support::plan(
        &BASE
            .replace("exactly = 1\n", "")
            .replace("\"hello\"", "\"hostkeeper\"")
            .replace("timeout_secs = 10", "timeout_secs = 300"),
    );
    let ran = support::run(
        &plan,
        &base_params(),
        &fleet,
        &mut Operator::new(Rc::clone(&fleet)),
    );
    ran.result.expect("the run ends");
    // Hosts are numbered in the order the run first saw them: the update order, by node id.
    assert_eq!(
        ran.names.tsv(),
        "node-alpha\thost-1\nnode-mid\thost-2\nnode-zeta\thost-3\n"
    );
    let text: String = ran.records.iter().map(Record::line).collect::<String>() + &ran.console;
    for node in ["node-zeta", "node-alpha", "node-mid"] {
        assert!(
            !text.contains(node),
            "{node} reached the records or the console"
        );
    }
    let mut names = Names::default();
    assert_eq!(names.name("x"), "host-1");
    assert_eq!(names.name("y"), "host-2");
    assert_eq!(names.name("x"), "host-1");
}

#[test]
fn a_restart_is_seen_by_a_host_no_longer_listed_when_no_poll_fails() {
    let fleet = fleet(vec![
        SimHost::new("node-a", 0, &[("hostkeeper", "v1")]),
        SimHost::new("node-b", 5_000, &[("hostkeeper", "v1")]),
    ]);
    let plan = support::shipped("coordinator-restart");
    let plan = hostkeeper_measure::plan::Plan {
        repetitions: 1,
        ..plan
    };
    // Down between two polls: no poll fails, but the restarted coordinator lists a host only
    // after its next heartbeat.
    let mut operator = Operator::new(Rc::clone(&fleet))
        .then(|f, now| f.world.down.push((now + 1_200, now + 1_800)));
    let ran = support::run(&plan, &support::params(&[]), &fleet, &mut operator);
    ran.result.expect("the run ends");
    let got = lines(&ran.records);
    assert!(!got.iter().any(|l| l.contains("call-failed")), "{got:#?}");
    assert!(
        got.contains(&"1 back restart-seen -".to_owned()),
        "{got:#?}"
    );
    assert!(got.contains(&"1 back back host-1".to_owned()), "{got:#?}");
    assert_eq!(
        got.last().map(String::as_str),
        Some("1 repetition ended - complete=true")
    );
}

#[test]
fn a_new_mac_is_one_not_listed_or_listed_with_another_enrollment_and_a_failed_listing_is_recorded()
{
    let fleet = one_hello();
    {
        let mut f = fleet.borrow_mut();
        f.world.devices = vec![
            ("d-1".to_owned(), Some(1_700_000_000)),
            ("d-2".to_owned(), None),
            ("d-3".to_owned(), Some(1_700_000_100)),
        ];
    }
    let plan = support::shipped("enrollment-mac");
    let mut operator = Operator::new(Rc::clone(&fleet)).then(|f, now| {
        f.once
            .push(("mdm list-devices".to_owned(), refused(1, "unavailable")));
        // d-3 enrolls anew: the same device, a newer enrollment.
        f.at(
            now + 10_000,
            Box::new(|w| w.devices[2].1 = Some(1_800_000_070)),
        );
    });
    let ran = support::run(&plan, &support::params(&[]), &fleet, &mut operator);
    let summary = ran.result.expect("the run ends");
    assert_eq!(
        summary.ended,
        vec![(1, "install-sent timed out".to_owned())]
    );
    assert_eq!(
        lines(&ran.records)[..4],
        [
            "1 repetition started - hosts=0",
            "1 erase done -",
            "1 mdm call-failed - code=1,refused=unavailable",
            "1 mdm enrolled -",
        ]
    );
    assert_eq!(ran.records[3].coordinator_unix, Some(1_800_000_070));
}

#[test]
fn the_dry_run_prints_every_step_of_every_repetition_and_calls_nothing() {
    let scratch = support::scratch("dry-run");
    let index = scratch.path().join("index.json");
    std::fs::write(&index, "{}\n").expect("write the index");
    let plan = support::shipped("update-rollback");
    let mut items = vec!["previous=v1".to_owned()];
    for k in 1..=5 {
        items.push(format!("version@{k}=v{}", k + 1));
        items.push(format!("index@{k}={}", index.display()));
    }
    let params = hostkeeper_measure::params::Params::parse(&items).expect("parameters");
    let mut out = Vec::new();
    dry_run(&plan, &params, &mut out).expect("the dry run");
    let out = String::from_utf8(out).expect("text");
    let first = format!(
        "plan update-rollback: An update and a rollback on one host\nhosts: The one host hello is assigned to (label one host demo=yes and no other): host-1 in the records.\nrepetitions: 5\nevery call is `hkctl --endpoint <endpoint> --ca-file <ca file> --token-file <token file> <command>`\nrepetition 1:\n  start: hkctl hosts list (the hosts of hello), then hkctl audit list --newest true --limit 1\n  update: call `hkctl releases set-desired --request <file: {{\"desired\":{{\"version\":\"v2\"}},\"releaseIndex\":\"<the text of {}>\",\"releaseIndexDigest\":\"ca3d163bab055381827226140568f3bef7eaac187cebd76878e0b63e9e442356\",\"service\":\"hello\"}}>`; stamped by its audit entry set_desired\n  updated: watch every 1 s, at most 900 s (a timeout ends the repetition): every host lists hello v2 installed by a succeeded update, healthy\n  rollback: call `hkctl releases force --service hello --version v1 --reason 'measurement: roll back to the previous version' --request <file: {{\"hosts\":{{\"nodeIds\":[\"<host>\"]}}}}>`; stamped by its audit entry force\n  rolled-back: watch every 1 s, at most 900 s (a timeout ends the repetition): every host lists hello v1 installed by a succeeded update, healthy\nrepetition 2:\n",
        index.display()
    );
    assert!(out.starts_with(&first), "{out}");
    assert!(
        out.ends_with("dry run: nothing was called or written and the operator was asked nothing; the parameter files were read to check them\n"),
        "{out}"
    );
    assert_eq!(out.matches("\nrepetition ").count(), 5);

    // Every other kind of step and pick, in words.
    let cases = [
        (
            "coordinator-restart",
            "  start: hkctl hosts list (the live hosts)\n  steady: watch every 5 s, at most 120 s (a timeout ends the repetition): every host is live\n  restart: ask the operator: Press Enter, then restart the coordinator on its Postgres store within two minutes (docker compose --profile postgres restart coordinator, as docs/deploy/coordinator/README.md says).\n  back: watch every 1 s, at most 900 s (a timeout ends the repetition): the coordinator goes away (a failed call or a host not live) and every host is live again with a newer heartbeat\n",
        ),
        (
            "enrollment-mac",
            "  start: no hosts: the steps find them, then hkctl audit list --newest true --limit 1\n  erase: ask the operator: Erase the Mac and let it enroll through Automated Device Enrollment; press Enter when the erase starts.\n  mdm: watch every 5 s, at most 7200 s (a timeout ends the repetition): a Mac enrolls with the MDM gate\n  install-sent: watch every 2 s, at most 1800 s (a timeout ends the repetition): an audit entry mdm_install_sent\n",
        ),
        (
            "enrollment",
            "  approved: watch every 2 s, at most 3600 s (a timeout ends the repetition): an audit entry enroll_approve or enrollment.auto_approved\n  issued: watch every 2 s, at most 900 s (a timeout ends the repetition): an audit entry enroll_issued\n  serving: watch every 2 s, at most 1800 s (a timeout ends the repetition): the new host has a first heartbeat and a first serving stamp\n",
        ),
    ];
    for (id, want) in cases {
        let mut out = Vec::new();
        dry_run(&support::shipped(id), &support::params(&[]), &mut out).expect("the dry run");
        let out = String::from_utf8(out).expect("text");
        assert!(out.contains(want), "{id}:\n{out}");
    }
    let mut out = Vec::new();
    let drift = support::shipped("drift");
    let mut items = vec!["change=a change".to_owned()];
    for k in 1..=3 {
        items.push(format!("version@{k}=v{}", k + 1));
        items.push(format!("index@{k}={}", index.display()));
    }
    let params = hostkeeper_measure::params::Params::parse(&items).expect("parameters");
    dry_run(&drift, &params, &mut out).expect("the dry run");
    let out = String::from_utf8(out).expect("text");
    assert!(
        out.contains("  change: ask the operator: On the host <host>, make the hand-made change: a change.\n  detected: watch every 2 s, at most 600 s (a timeout goes on): every host reports hello HEALTH_UNHEALTHY\n"),
        "{out}"
    );
    // An empty argument, and one with a quote, are quoted as a shell needs them.
    let mut out = Vec::new();
    dry_run(
        &support::plan(BASE),
        &support::params(&["version@1=", "version@2=it's"]),
        &mut out,
    )
    .expect("the dry run");
    let out = String::from_utf8(out).expect("text");
    assert!(
        out.contains("call `hkctl releases set-desired --x ''"),
        "{out}"
    );
    assert!(
        out.contains("call `hkctl releases set-desired --x 'it'\\''s'"),
        "{out}"
    );
    let mut out = Vec::new();
    let mut items = Vec::new();
    for k in 1..=3 {
        items.push(format!("version@{k}=v{}", k + 1));
        items.push(format!("index@{k}={}", index.display()));
    }
    let params3 = hostkeeper_measure::params::Params::parse(&items).expect("parameters");
    dry_run(&support::shipped("rollout"), &params3, &mut out).expect("the dry run");
    let out = String::from_utf8(out).expect("text");
    assert!(
        out.contains("  rollout: watch every 2 s, at most 7200 s (a timeout ends the repetition): every host lists hostkeeper v2 installed by a succeeded update\n"),
        "{out}"
    );
    let mut sink = Broken;
    assert_eq!(
        dry_run(&drift, &params, &mut sink)
            .expect_err("no console")
            .to_string(),
        "writing to the console: broken"
    );
}

#[test]
fn a_succeeded_update_is_not_updated_until_healthy_for_a_plan_that_asks_healthy() {
    let fleet = one_hello();
    let plan = support::plan(
        &BASE
            .replace(
                "version = \"{version}\" }",
                "version = \"{version}\", healthy = true }",
            )
            .replace("repetitions = 2", "repetitions = 1"),
    );
    // The verdict comes at 64.5 s; hello is unhealthy from just after it until 67 s.
    fleet.borrow_mut().world.update_ms = 3_500;
    let sick = |health: &'static str| {
        Box::new(move |w: &mut support::World| {
            w.host("node-a")
                .services
                .get_mut("hello")
                .expect("hello")
                .health = health.to_owned();
        }) as support::Change
    };
    fleet.borrow_mut().at(64_600, sick("HEALTH_UNHEALTHY"));
    fleet.borrow_mut().at(67_000, sick("HEALTH_HEALTHY"));
    let mut operator = Operator::new(Rc::clone(&fleet));
    let ran = support::run(
        &plan,
        &support::params(&["version@1=v2"]),
        &fleet,
        &mut operator,
    );
    ran.result.expect("the run ends");
    assert_eq!(
        lines(&ran.records),
        [
            "1 repetition started - hosts=1",
            "1 set called - audit_seq=1",
            "1 wait ordered host-1",
            "1 wait updated host-1",
            "1 repetition ended - complete=true",
        ]
    );
    assert_eq!(ran.records[3].runner_ms, T0_MS + 67_000);
}
