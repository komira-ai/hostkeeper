//! The `hkctl` port with a real process (the fake hkctl of tests/bin), and the command line from
//! its arguments to the files it writes.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use hostkeeper_measure::cli;
use hostkeeper_measure::coordinator::{Answer, CallError, Coordinator, Hkctl};
use hostkeeper_testkit::Scratch;
use serde_json::{Value, json};

use crate::support::{self, FakeClock};

const FAKE: &str = env!("CARGO_BIN_EXE_hostkeeper-measure-fake-hkctl");
const ENDPOINT: &str = "https://coordinator.invalid";

/// A directory for the fake hkctl: its CA file (whose directory it answers from) and token.
struct Rig {
    scratch: Scratch,
}

impl Rig {
    fn new(name: &str) -> Self {
        let scratch = support::scratch(name);
        std::fs::write(scratch.path().join("ca.pem"), "not read\n").expect("write the CA");
        std::fs::write(scratch.path().join("token"), "not read\n").expect("write the token");
        Self { scratch }
    }
    fn dir(&self) -> &Path {
        self.scratch.path()
    }
    fn ca(&self) -> PathBuf {
        self.dir().join("ca.pem")
    }
    fn token(&self) -> PathBuf {
        self.dir().join("token")
    }
    /// Every call answers this.
    fn answer(&self, answer: &Value) {
        std::fs::write(self.dir().join("answer.json"), answer.to_string())
            .expect("write the answer");
    }
    /// Call `n` (from 0) answers this.
    fn answer_nth(&self, n: usize, answer: &Value) {
        let dir = self.dir().join("answers");
        std::fs::create_dir_all(&dir).expect("the answers directory");
        std::fs::write(dir.join(format!("{n}.json")), answer.to_string())
            .expect("write the answer");
    }
    /// The calls the fake recorded.
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("a call"))
            .collect()
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn hkctl_is_called_with_the_globals_the_command_and_the_request_file_and_its_answer_read() {
    let rig = Rig::new("hkctl-call");
    let requests = rig.dir().join("requests");
    std::fs::create_dir_all(&requests).expect("the requests directory");
    rig.answer(&json!({"code": 2, "stdout": "{\"x\":1}\n", "stderr": "refused: held_bad\n"}));
    let mut hkctl = Hkctl::new(
        Path::new(FAKE),
        ENDPOINT,
        &rig.ca(),
        &rig.token(),
        &requests,
    );
    let request = json!({"service": "hello"});
    let answer = hkctl
        .call(&strings(&["releases", "set-desired"]), Some(&request))
        .expect("a call");
    assert_eq!(
        answer,
        Answer {
            code: 2,
            stdout: "{\"x\":1}\n".to_owned(),
            stderr: "refused: held_bad\n".to_owned()
        }
    );
    let second = hkctl
        .call(&strings(&["hosts", "list"]), None)
        .expect("a call");
    assert_eq!(second.code, 2);
    let request_file = requests.join("1.json");
    assert_eq!(
        std::fs::read_to_string(&request_file).expect("the request file"),
        "{\n  \"service\": \"hello\"\n}\n"
    );
    let globals = [
        "--endpoint".to_owned(),
        ENDPOINT.to_owned(),
        "--ca-file".to_owned(),
        rig.ca().display().to_string(),
        "--token-file".to_owned(),
        rig.token().display().to_string(),
    ];
    let mut first: Vec<String> = globals.to_vec();
    first.extend(strings(&["releases", "set-desired", "--request"]));
    first.push(request_file.display().to_string());
    let mut then: Vec<String> = globals.to_vec();
    then.extend(strings(&["hosts", "list"]));
    assert_eq!(
        rig.calls(),
        [
            json!({"argv": first, "request": {"service": "hello"}}),
            json!({"argv": then, "request": null}),
        ]
    );
    // The token is never an argument: only the file's path is.
    assert!(
        !rig.calls()
            .iter()
            .any(|c| c.to_string().contains("not read"))
    );
}

#[test]
fn hkctl_that_cannot_run_or_is_killed_or_whose_request_cannot_be_written_is_an_error() {
    let rig = Rig::new("hkctl-errors");
    let nowhere = rig.dir().join("no-such-hkctl");
    let mut hkctl = Hkctl::new(&nowhere, ENDPOINT, &rig.ca(), &rig.token(), rig.dir());
    let err = hkctl
        .call(&strings(&["hosts", "list"]), None)
        .expect_err("no program");
    assert!(matches!(err, CallError::Spawn { .. }));
    assert!(
        err.to_string()
            .starts_with(&format!("running {}: ", nowhere.display())),
        "{err}"
    );
    let missing = rig.dir().join("no-requests");
    let mut hkctl = Hkctl::new(Path::new(FAKE), ENDPOINT, &rig.ca(), &rig.token(), &missing);
    let err = hkctl
        .call(&strings(&["releases", "set-desired"]), Some(&json!({})))
        .expect_err("no requests directory");
    assert!(
        err.to_string().starts_with(&format!(
            "writing the request {}: ",
            missing.join("1.json").display()
        )),
        "{err}"
    );
    rig.answer(&json!({"abort": true}));
    let mut hkctl = Hkctl::new(
        Path::new(FAKE),
        ENDPOINT,
        &rig.ca(),
        &rig.token(),
        rig.dir(),
    );
    let err = hkctl
        .call(&strings(&["hosts", "list"]), None)
        .expect_err("killed");
    assert_eq!(err.to_string(), format!("{FAKE} was ended by a signal"));
}

/// Runs the command line in process with the fake clock and `stdin`.
fn cli(args: &[&str], stdin: &str) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = io::Cursor::new(stdin.as_bytes().to_vec());
    let args = std::iter::once(OsString::from("hostkeeper-measure"))
        .chain(args.iter().map(OsString::from));
    let code = cli::run_with(args, &FakeClock::default(), &mut input, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).expect("text"),
        String::from_utf8(err).expect("text"),
    )
}

fn plan_path(id: &str) -> String {
    support::crate_dir()
        .join("plans")
        .join(format!("{id}.toml"))
        .display()
        .to_string()
}

/// A plan the command-line tests run end to end: one live host, a call with a request, an
/// operator step and a watch of liveness.
const PLAN: &str = r#"
id = "cli"
title = "The command line"
measures = "M."
records = "R."
repetitions = 1
states = ["{live.max} live"]

[hosts]
says = "Every live host."
pick = "live"

[[param]]
name = "svc"
says = "A service."

[[step]]
id = "status"
do = "call"
args = ["releases", "status"]
request = { service = "{svc}" }

[[step]]
id = "ask"
do = "operator"
says = "Say when."

[[step]]
id = "up"
do = "watch"
until = { what = "live" }
poll_secs = 1
timeout_secs = 5

[[measure]]
id = "live"
kind = "count"
says = "hosts seen live"
unit = "hosts"
of = "up/live"
"#;

const HOSTS: &str =
    "{\"hosts\":[{\"nodeId\":\"node-a\",\"live\":true,\"lastHeartbeatUnix\":\"1800000000\"}]}\n";

#[test]
fn help_version_and_usage_errors_exit_as_a_command_line_does() {
    let (code, out, err) = cli(&["--help"], "");
    assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
    assert!(out.contains("Usage: hostkeeper-measure <COMMAND>"), "{out}");
    let (code, out, err) = cli(&["run", "--plan", "p", "--run-id", "r"], "");
    assert_eq!((code, out.as_str()), (ExitCode::from(2), ""));
    assert!(
        err.contains("the following required arguments were not provided"),
        "{err}"
    );
    let status = Command::new(env!("CARGO_BIN_EXE_hostkeeper-measure"))
        .arg("--version")
        .output()
        .expect("run the program");
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "hostkeeper-measure 0.1.0\n"
    );
}

#[test]
fn check_prints_each_plan_and_refuses_a_plan_that_does_not_check() {
    let (code, out, _) = cli(
        &[
            "check",
            "--plan",
            &plan_path("rollout"),
            "--plan",
            &plan_path("drift"),
        ],
        "",
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "rollout: A new hostkeeper version rolled out to the farm\ndrift: A hand-made change on a host, detected and repaired\n"
    );
    let scratch = support::scratch("cli-check");
    let bad = scratch.path().join("bad.toml");
    std::fs::write(&bad, "id = \"x\"\n").expect("write a plan");
    let (code, _, err) = cli(&["check", "--plan", &bad.display().to_string()], "");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!(
            "hostkeeper-measure: {}: the plan is not TOML of the plan's shape",
            bad.display()
        )),
        "{err}"
    );
    let missing = scratch.path().join("missing.toml");
    let (code, _, err) = cli(&["check", "--plan", &missing.display().to_string()], "");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!("hostkeeper-measure: {}: ", missing.display())),
        "{err}"
    );
    let mut broken = Broken;
    let result = cli::execute(
        cli::Command::Check {
            plans: vec![PathBuf::from(plan_path("rollout"))],
        },
        &FakeClock::default(),
        &mut io::Cursor::new(Vec::new()),
        &mut broken,
    );
    assert_eq!(
        result.expect_err("no output").to_string(),
        "writing the output: broken"
    );
}

struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("broken"))
    }
}

/// The live flags of `run` for the rig, and the plan file and run directory under it.
fn live(rig: &Rig, run_dir: &Path, plan: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = strings(&["run", "--plan"]);
    args.push(plan.display().to_string());
    args.extend(strings(&[
        "--run-id",
        "r1",
        "--param",
        "svc=hello",
        "--run-dir",
    ]));
    args.push(run_dir.display().to_string());
    args.extend(strings(&[
        "--hkctl",
        FAKE,
        "--endpoint",
        ENDPOINT,
        "--ca-file",
    ]));
    args.push(rig.ca().display().to_string());
    args.push("--token-file".to_owned());
    args.push(rig.token().display().to_string());
    args.extend(strings(extra));
    args
}

fn run(args: &[String], stdin: &str) -> (ExitCode, String, String) {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cli(&args, stdin)
}

#[test]
fn run_writes_the_run_its_records_its_requests_and_its_names_and_analyze_writes_the_result() {
    let rig = Rig::new("cli-run");
    let plan = rig.dir().join("cli.toml");
    std::fs::write(&plan, PLAN).expect("write the plan");
    let run_dir = rig.dir().join("run");
    rig.answer(&json!({"code": 0, "stdout": HOSTS, "stderr": ""}));
    let (code, out, err) = run(&live(&rig, &run_dir, &plan, &[]), "\n");
    assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
    assert_eq!(
        out,
        format!(
            "[repetition 1] repetition: started\n[repetition 1] status: called\n[repetition 1] ask: Say when.\nPress Enter when it is done.\n[repetition 1] ask: done\n[repetition 1] up: live host-1\n[repetition 1] repetition: ended\n1 of 1 repetitions complete; the records are in {}\n",
            run_dir.join("records.jsonl").display()
        )
    );
    let read = |name: &str| std::fs::read_to_string(run_dir.join(name)).expect("a file of the run");
    assert_eq!(
        read("run.json"),
        "{\n  \"plan\": \"cli\",\n  \"repetitions\": 1,\n  \"run\": \"r1\"\n}\n"
    );
    assert_eq!(read("hosts.tsv"), "node-a\thost-1\n");
    assert_eq!(read("requests/1.json"), "{\n  \"service\": \"hello\"\n}\n");
    let t = support::T0_MS + 60_000;
    assert_eq!(
        read("records.jsonl"),
        format!(
            "{{\"detail\":{{\"hosts\":\"1\"}},\"event\":\"started\",\"rep\":1,\"runner_ms\":{t},\"step\":\"repetition\"}}\n{{\"event\":\"called\",\"rep\":1,\"runner_ms\":{t},\"step\":\"status\"}}\n{{\"event\":\"done\",\"rep\":1,\"runner_ms\":{t},\"step\":\"ask\"}}\n{{\"coordinator_unix\":1800000000,\"event\":\"live\",\"host\":\"host-1\",\"rep\":1,\"runner_ms\":{t},\"step\":\"up\"}}\n{{\"detail\":{{\"complete\":\"true\"}},\"event\":\"ended\",\"rep\":1,\"runner_ms\":{t},\"step\":\"repetition\"}}\n"
        )
    );
    // A run directory that holds records is not run again.
    let (code, _, err) = run(&live(&rig, &run_dir, &plan, &[]), "\n");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        format!(
            "hostkeeper-measure: {}: the run directory already holds records\n",
            run_dir.display()
        )
    );

    let out_dir = rig.dir().join("results");
    std::fs::create_dir_all(&out_dir).expect("the results directory");
    let analyze = |plan: &Path, run_dir: &Path| {
        let args = [
            "analyze".to_owned(),
            "--plan".to_owned(),
            plan.display().to_string(),
            "--run-dir".to_owned(),
            run_dir.display().to_string(),
            "--out-dir".to_owned(),
            out_dir.display().to_string(),
        ];
        run(&args, "")
    };
    let (code, out, err) = analyze(&plan, &run_dir);
    assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
    assert_eq!(
        out,
        format!(
            "wrote {}\nwrote {}\n",
            out_dir.join("cli-r1.md").display(),
            out_dir.join("cli-r1.tsv").display()
        )
    );
    let md = std::fs::read_to_string(out_dir.join("cli-r1.md")).expect("the section");
    assert!(md.contains("- 1 live\n"), "{md}");
    let tsv = std::fs::read_to_string(out_dir.join("cli-r1.tsv")).expect("the result file");
    assert!(
        tsv.contains(
            "\nraw\tlive-rep-1\t1\thosts\tplan cli, run r1, repetition 1: hosts seen live\t\n"
        ),
        "{tsv}"
    );

    // Another plan's records, and broken run files, are refused.
    let (code, _, err) = analyze(Path::new(&plan_path("rollout")), &run_dir);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "hostkeeper-measure: the run is of plan \"cli\", not \"rollout\"\n"
    );
    std::fs::write(run_dir.join("records.jsonl"), "{}\n").expect("break the records");
    let (code, _, err) = analyze(&plan, &run_dir);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!(
            "hostkeeper-measure: {}: records line 1: ",
            run_dir.join("records.jsonl").display()
        )),
        "{err}"
    );
    std::fs::write(run_dir.join("run.json"), "[]").expect("break the run file");
    let (code, _, err) = analyze(&plan, &run_dir);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!(
            "hostkeeper-measure: {}: ",
            run_dir.join("run.json").display()
        )),
        "{err}"
    );
    let (code, _, err) = analyze(&plan, &rig.dir().join("no-run"));
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.starts_with("hostkeeper-measure: "), "{err}");
}

#[test]
fn analyze_into_a_directory_that_does_not_exist_fails() {
    let rig = Rig::new("cli-analyze-out");
    let plan = rig.dir().join("cli.toml");
    std::fs::write(&plan, PLAN).expect("write the plan");
    let run_dir = rig.dir().join("run");
    rig.answer(&json!({"code": 0, "stdout": HOSTS, "stderr": ""}));
    let (code, _, _) = run(&live(&rig, &run_dir, &plan, &[]), "\n");
    assert_eq!(code, ExitCode::SUCCESS);
    let nowhere = rig.dir().join("nowhere");
    let args = [
        "analyze".to_owned(),
        "--plan".to_owned(),
        plan.display().to_string(),
        "--run-dir".to_owned(),
        run_dir.display().to_string(),
        "--out-dir".to_owned(),
        nowhere.display().to_string(),
    ];
    let (code, _, err) = run(&args, "");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!(
            "hostkeeper-measure: {}: ",
            nowhere.join("cli-r1.md").display()
        )),
        "{err}"
    );
}

#[test]
fn a_run_that_stops_keeps_its_records_and_names_and_says_why() {
    let rig = Rig::new("cli-stop");
    let plan = rig.dir().join("cli.toml");
    std::fs::write(&plan, PLAN).expect("write the plan");
    let run_dir = rig.dir().join("run");
    rig.answer(&json!({"code": 0, "stdout": HOSTS, "stderr": ""}));
    // The operator never answers: standard input is empty.
    let (code, out, err) = run(&live(&rig, &run_dir, &plan, &[]), "");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "hostkeeper-measure: standard input ended while waiting for the operator\n"
    );
    assert!(out.ends_with("Press Enter when it is done.\n"), "{out}");
    let records = std::fs::read_to_string(run_dir.join("records.jsonl")).expect("the records");
    assert_eq!(records.lines().count(), 2);
    assert_eq!(
        std::fs::read_to_string(run_dir.join("hosts.tsv")).expect("the names"),
        ""
    );

    // A call that fails ends the repetition, and the run says so. The first run made two calls
    // (the hosts, the status); the status of this one is the fourth.
    let run_dir = rig.dir().join("run-2");
    rig.answer_nth(
        3,
        &json!({"code": 2, "stdout": "", "stderr": "refused: unknown_service\n"}),
    );
    let (code, out, _) = run(&live(&rig, &run_dir, &plan, &[]), "\n");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        out.contains("\n0 of 1 repetitions complete; the records are in "),
        "{out}"
    );
    assert!(
        out.ends_with("repetition 1 ended early: status failed\n"),
        "{out}"
    );
}

#[test]
fn run_refuses_a_bad_run_id_bad_parameters_and_a_run_directory_it_cannot_make() {
    let rig = Rig::new("cli-refuse");
    let plan = rig.dir().join("cli.toml");
    std::fs::write(&plan, PLAN).expect("write the plan");
    let run_dir = rig.dir().join("run");
    let mut args = live(&rig, &run_dir, &plan, &[]);
    let id = args.iter().position(|a| a == "r1").expect("the run id");
    args[id] = "R1".to_owned();
    let (code, _, err) = run(&args, "\n");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "hostkeeper-measure: the run id \"R1\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter\n"
    );
    let (code, _, err) = run(&live(&rig, &run_dir, &plan, &["--param", "nothing"]), "\n");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "hostkeeper-measure: --param \"nothing\" is not name=value or name@repetition=value\n"
    );
    // A records file that cannot be opened: a link to a directory that does not exist.
    let linked = rig.dir().join("linked");
    std::fs::create_dir_all(&linked).expect("a run directory");
    let records = linked.join("records.jsonl");
    std::os::unix::fs::symlink(rig.dir().join("nowhere/records.jsonl"), &records).expect("a link");
    let (code, _, err) = run(&live(&rig, &linked, &plan, &[]), "\n");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!("hostkeeper-measure: {}: ", records.display())),
        "{err}"
    );
    let file = rig.dir().join("a-file");
    std::fs::write(&file, "").expect("write a file");
    let (code, _, err) = run(&live(&rig, &file, &plan, &[]), "\n");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.starts_with(&format!(
            "hostkeeper-measure: {}: ",
            file.join("requests").display()
        )),
        "{err}"
    );
}

#[test]
fn run_with_dry_run_prints_the_steps_and_writes_nothing() {
    let rig = Rig::new("cli-dry");
    let plan = rig.dir().join("cli.toml");
    std::fs::write(&plan, PLAN).expect("write the plan");
    let args = [
        "run",
        "--plan",
        &plan.display().to_string(),
        "--run-id",
        "r1",
        "--param",
        "svc=hello",
        "--dry-run",
    ]
    .map(str::to_owned);
    let (code, out, err) = run(&args, "");
    assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
    assert_eq!(
        out,
        "plan cli: The command line\nhosts: Every live host.\nrepetitions: 1\nevery call is `hkctl --endpoint <endpoint> --ca-file <ca file> --token-file <token file> <command>`\nrepetition 1:\n  start: hkctl hosts list (the live hosts)\n  status: call `hkctl releases status --request <file: {\"service\":\"hello\"}>`\n  ask: ask the operator: Say when.\n  up: watch every 1 s, at most 5 s (a timeout ends the repetition): every host is live\ndry run: nothing was called or written and the operator was asked nothing; the parameter files were read to check them\n"
    );
    assert!(rig.calls().is_empty());
    // With every live flag given too, --dry-run still runs nothing.
    let run_dir = rig.dir().join("run");
    let (code, _, _) = run(&live(&rig, &run_dir, &plan, &["--dry-run"]), "");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(!run_dir.exists());
    assert!(rig.calls().is_empty());
}
