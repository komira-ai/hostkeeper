//! The wait of release.yml's job `coordinator` and its two error steps, run for real: each test
//! reads the committed job, runs the wait step's `run:` with `bash -eo pipefail` under its `env`,
//! reads the step's output file the way the runner does, chooses the error steps to run by their
//! committed `if:` (evaluated by [`super::expr`]), and runs those with their `env`, reading the
//! annotation each prints. Nothing is substituted in a command.
//!
//! `curl` on the `PATH` is the test program `tests/bin/gh_fixture.rs`: it answers the version
//! route from a scratch file per try, or not at all, retries as curl does with the committed
//! retry flags (and fails as ubuntu-24.04's curl 8.5 does when it cannot retry), and never
//! waits, so a test does not take the 30 minutes of the committed retries.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::expr::{Context, holds};
use super::lint::entry;
use super::testdir::TestDir;
use super::{workflows, yaml};

const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");

/// The release the job waits for.
const VERSION: &str = "0.1.230";

/// The coordinator's private listener, as the repository variable gives it.
const URL: &str = "https://coordinator.example.com:8444";

/// The request the wait makes, as the fake records it.
const ASKED: &str = "GET /v1/version?at_least=0.1.230";

/// The key of the fake's answer file for [`ASKED`]: every character but a letter or a digit
/// made `_`.
fn answer_key() -> String {
    ASKED
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// What the coordinator answers, or that it does not answer.
enum Coordinator {
    Answers(u16, &'static str),
    Silent,
}

/// The tries of the committed wait: one request and its 180 retries.
const TRIES: usize = 181;

/// What a failed or passed wait left: how many times it asked, the wait's exit status, its
/// outputs, and the annotation lines of the error steps that ran, in order.
#[derive(Debug, PartialEq, Eq)]
struct Ran {
    tries: usize,
    exit: Option<i32>,
    outputs: BTreeMap<String, String>,
    annotations: Vec<String>,
}

/// The text of `key` of `step`.
fn text_of<'a>(step: &'a yaml::Node, key: &str) -> Option<&'a str> {
    entry(step, key).and_then(yaml::Node::as_str)
}

/// The value of an `env:` value of the job's steps: one of the two expressions they use.
fn evaluate(value: &str, outputs: &BTreeMap<String, String>) -> String {
    match value {
        "${{ needs.targets.outputs.version }}" => VERSION.to_owned(),
        "${{ vars.HOSTKEEPER_COORDINATOR_URL }}" => URL.to_owned(),
        "${{ steps.wait.outputs.status }}" => outputs.get("status").cloned().unwrap_or_default(),
        other => panic!("the test does not know the value `{other}`"),
    }
}

/// The `(key, value)` text pairs of the step's `env`.
fn env_of(step: &yaml::Node) -> Vec<(&str, &str)> {
    entry(step, "env")
        .map_or(&[][..], super::lint::entries)
        .iter()
        .map(|(k, v)| (k.as_str().expect("a key"), v.as_str().expect("a text")))
        .collect()
}

/// Runs `script` with bash in `root`, with the fake on the `PATH`, `env` and the output file
/// `output`: its exit status and stdout.
fn bash(root: &Path, script: &str, env: &[(&str, String)], output: &Path) -> (Option<i32>, String) {
    let mut command = Command::new("bash");
    command
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", script])
        .current_dir(root)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("GITHUB_OUTPUT", output)
        .env("RUNNER_TEMP", root.join("temp"));
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().expect("run bash");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The answer file's text of `coordinator`; `000` is no answer.
fn answer_text(coordinator: &Coordinator) -> String {
    match coordinator {
        Coordinator::Answers(status, body) => format!("{status}\n{body}"),
        Coordinator::Silent => "000".to_owned(),
    }
}

/// Runs the committed wait against `coordinator`, the same answer to every try, then the
/// error steps whose committed conditions hold.
fn wait(name: &str, coordinator: &Coordinator) -> Ran {
    wait_after(name, &[], coordinator)
}

/// Runs the committed wait against a coordinator that answers the tries in `first` in order,
/// and every later try as `then`, then the error steps whose committed conditions hold.
fn wait_after(name: &str, first: &[&Coordinator], then: &Coordinator) -> Ran {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coordinator-wait-run");
    let root = TestDir::under(&base, name);
    for dir in ["bin/answers", "temp"] {
        std::fs::create_dir_all(root.join(dir)).expect("make a scratch directory");
    }
    std::fs::hard_link(GH, root.join("bin/curl")).expect("install the fake curl");
    std::fs::write(root.join("temp/coordinator-ca.pem"), "the CA").expect("the CA");
    std::fs::write(
        root.join("bin/answers").join(answer_key()),
        answer_text(then),
    )
    .expect("the coordinator's answer");
    for (at, coordinator) in first.iter().enumerate() {
        std::fs::write(
            root.join("bin/answers")
                .join(format!("{}-{}", answer_key(), at + 1)),
            answer_text(coordinator),
        )
        .expect("the coordinator's answer to one try");
    }

    let text = workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists");
    let doc = yaml::load(&text).expect("release.yml parses");
    let job = entry(entry(&doc, "jobs").expect("jobs"), "coordinator").expect("the job");
    let Some(yaml::Value::Seq(steps)) = entry(job, "steps").map(|s| &s.value) else {
        panic!("the job coordinator has no steps");
    };
    let at = steps
        .iter()
        .position(|s| text_of(s, "id") == Some("wait"))
        .expect("the wait step");

    let none = BTreeMap::new();
    let wait_step = &steps[at];
    let env: Vec<(&str, String)> = env_of(wait_step)
        .into_iter()
        .map(|(k, v)| (k, evaluate(v, &none)))
        .collect();
    let output = root.join("temp/output-wait");
    let script = text_of(wait_step, "run").expect("the wait runs");
    let (exit, _) = bash(&root, script, &env, &output);
    let outputs: BTreeMap<String, String> = std::fs::read_to_string(&output)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let calls = std::fs::read_to_string(root.join("bin/answers/calls")).expect("the fake's calls");
    assert!(
        calls.lines().all(|l| l == ASKED),
        "every try asks the version route for this release: {calls}"
    );

    let mut names = BTreeMap::from([(
        "steps.wait.outcome".to_owned(),
        if exit == Some(0) {
            "success"
        } else {
            "failure"
        }
        .to_owned(),
    )]);
    if let Some(status) = outputs.get("status") {
        names.insert("steps.wait.outputs.status".to_owned(), status.clone());
    }
    let context = Context {
        names,
        cancelled: false,
        failed: exit != Some(0),
    };
    let mut annotations = Vec::new();
    for step in &steps[at + 1..] {
        let condition = text_of(step, "if").expect("an error step has a condition");
        if !holds(condition, &context).expect("the condition evaluates") {
            continue;
        }
        let env: Vec<(&str, String)> = env_of(step)
            .into_iter()
            .map(|(k, v)| (k, evaluate(v, &outputs)))
            .collect();
        let script = text_of(step, "run").expect("an error step runs");
        let (code, printed) = bash(&root, script, &env, &root.join("temp/output-error"));
        assert_eq!(code, Some(0), "the error step itself succeeds");
        annotations.extend(printed.lines().map(str::to_owned));
    }
    Ran {
        tries: calls.lines().count(),
        exit,
        outputs,
        annotations,
    }
}

/// The annotation of the error step after a 409 or a 404 with `status`.
fn no_updater(status: &str) -> String {
    format!(
        "::error::No coordinator updater reported this release ({VERSION}) (is the timer installed?). For 30 minutes the coordinator answered that it runs an older release (its last answer was HTTP {status}; 404 is a coordinator from before the version route). Install the timer of hostkeeper-coordinator-update on the coordinator's machine, or read its journal there, as docs/deploy/coordinator-update.md says. With the control-plane deploy on (CP_DEPLOY_ENDPOINT), the job cp-deploy reported this release deployed, so check that HOSTKEEPER_COORDINATOR_URL names the coordinator it deploys. The hosts' desired version is not set."
    )
}

/// The annotation of the error step after any other last status.
fn no_answer(status: &str) -> String {
    format!(
        "::error::The coordinator did not say whether it runs this release ({VERSION}) within 30 minutes; its last answer was HTTP {status} (000 is no answer). Check the tailnet join and the repository variables HOSTKEEPER_COORDINATOR_URL and HOSTKEEPER_COORDINATOR_CA. The hosts' desired version is not set."
    )
}

fn outputs(status: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("status".to_owned(), status.to_owned())])
}

#[test]
fn a_coordinator_that_runs_an_older_release_fails_the_wait_saying_no_updater_reported_it() {
    // Catches: a wait that passes on a 409, gives up before its last try, writes no status (or
    // the body) into its outputs, or whose failure says the coordinator did not answer, or says
    // both, or says nothing; and an annotation without the release or the status.
    assert_eq!(
        wait("older", &older()),
        Ran {
            tries: TRIES,
            exit: Some(22),
            outputs: outputs("409"),
            annotations: vec![no_updater("409")],
        }
    );
}

#[test]
fn a_coordinator_from_before_the_version_route_fails_the_wait_saying_no_updater_reported_it() {
    // Catches: a 404 (the coordinator older than the route) taken for a pass or for no answer.
    assert_eq!(
        wait("before-route", &Coordinator::Answers(404, "not found")),
        Ran {
            tries: TRIES,
            exit: Some(22),
            outputs: outputs("404"),
            annotations: vec![no_updater("404")],
        }
    );
}

#[test]
fn a_coordinator_that_does_not_answer_fails_the_wait_saying_so_with_status_000() {
    // Catches: no answer (the tailnet, the CA or the URL) called a missing updater, a status
    // other than 000, or no annotation; and a proxy's 502 called a missing updater.
    assert_eq!(
        wait("silent", &Coordinator::Silent),
        Ran {
            tries: TRIES,
            exit: Some(7),
            outputs: outputs("000"),
            annotations: vec![no_answer("000")],
        }
    );
    assert_eq!(
        wait("proxy", &Coordinator::Answers(502, "bad gateway")),
        Ran {
            tries: TRIES,
            exit: Some(22),
            outputs: outputs("502"),
            annotations: vec![no_answer("502")],
        }
    );
}

#[test]
fn a_coordinator_that_runs_the_release_passes_the_wait_with_no_annotation() {
    // Catches: a wait that fails on a 200, or an error step that runs when the wait passed.
    assert_eq!(
        wait("runs", &runs()),
        Ran {
            tries: 1,
            exit: Some(0),
            outputs: outputs("200"),
            annotations: vec![],
        }
    );
}

/// The coordinator's answer while it runs an older release.
fn older() -> Coordinator {
    Coordinator::Answers(
        409,
        r#"{"error":"coordinator_older","version":"0.1.229","commit":"0123456789ab"}"#,
    )
}

/// The coordinator's answer once it runs the release.
fn runs() -> Coordinator {
    Coordinator::Answers(200, r#"{"version":"0.1.230","commit":"0123456789ab"}"#)
}

/// The outcome of a wait that passed after `tries` tries.
fn passed_after(tries: usize) -> Ran {
    Ran {
        tries,
        exit: Some(0),
        outputs: outputs("200"),
        annotations: vec![],
    }
}

#[test]
fn a_coordinator_that_answers_older_until_the_deploy_lands_passes_the_wait() {
    // Catches: the wait failing on the coordinator's first 409 (the old coordinator, while the
    // control-plane deploy or the updater replaces it), as `--fail-with-body` made curl 8.5 do,
    // ending with exit 23, no status and the "did not answer" annotation with an empty status.
    let older = older();
    assert_eq!(
        wait_after("older-then-runs", &[&older, &older], &runs()),
        passed_after(3)
    );
}

#[test]
fn a_coordinator_that_restarts_during_the_deploy_passes_the_wait() {
    // Catches: no answer (the old task stopped, the new one not yet listening) or a proxy's 502
    // between the 409s and the 200 ending the wait.
    let older = older();
    let proxy = Coordinator::Answers(502, "bad gateway");
    assert_eq!(
        wait_after(
            "restarts",
            &[&older, &Coordinator::Silent, &proxy, &older],
            &runs()
        ),
        passed_after(5)
    );
}

#[test]
fn the_wait_passes_on_its_last_try_and_not_after_it() {
    // Catches: a wait with fewer tries than the committed 181, or one that ignores the answer of
    // its last try; and one that asks once more after its last.
    let older = older();
    let before_last: Vec<&Coordinator> = vec![&older; TRIES - 1];
    assert_eq!(
        wait_after("last-try", &before_last, &runs()),
        passed_after(TRIES)
    );
    let all: Vec<&Coordinator> = vec![&older; TRIES];
    assert_eq!(
        wait_after("after-last-try", &all, &runs()),
        Ran {
            tries: TRIES,
            exit: Some(22),
            outputs: outputs("409"),
            annotations: vec![no_updater("409")],
        }
    );
}
