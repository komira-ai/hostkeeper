//! The job `no-downgrade` of the repository's `release.yml` ([`super::no_downgrade`]) and the
//! terms of `set-desired`'s `if:` that read it.
//!
//! The job's committed steps are run with `bash -eo pipefail`, as GitHub runs a `run:` step, with
//! `gh` on the `PATH` the test program `tests/bin/gh_fixture.rs` (it answers `gh release list`
//! from a scratch file with the runner's own jq, so no test reaches GitHub). A step's `if:` is
//! evaluated by [`super::expr`], and an expression the test does not know fails the test. What is
//! asserted is what decides whether the set runs and what the log says: the job's outputs and the
//! exact warning it prints. Then the set's `if:` is evaluated against each attempt of a run, the
//! re-run of an older release's jobs after a newer one was set among them; and the lint is shown
//! to refuse each defect put into the job.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::desired::{SET_DESIRED_IF, job};
use super::expr::{Context, holds};
use super::lint::{entries, entry, lint};
use super::no_downgrade::{GUARD_IF, JOB, OUTPUTS, SET_TERMS, STEPS, guard_needs};
use super::release_files::REPO;
use super::testdir::TestDir;
use super::{with_in, workflows, yaml};

const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");

fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// How a run of the job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Every step passed: the job's outputs, and what the steps printed.
    Passed(BTreeMap<String, String>, String),
    /// The step of this name failed.
    Failed(String),
}

/// The value of `value` with each `${{ expr }}` resolved: the run's `version` from targets, the
/// token, the attempt `1`, and the outputs of the steps so far.
fn evaluate(version: &str, steps: &BTreeMap<String, String>, value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find("${{ ") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let end = after.find(" }}").expect("an expression ends with ` }}`");
        let expr = &after[..end];
        let resolved = match expr {
            "needs.targets.outputs.version" => version.to_owned(),
            "github.token" => "fixture-token".to_owned(),
            "github.run_attempt" => "1".to_owned(),
            _ if expr.starts_with("steps.") => steps.get(expr).cloned().unwrap_or_default(),
            _ => panic!("the test does not know the expression `{expr}`"),
        };
        out.push_str(&resolved);
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    out
}

/// Runs the job of the committed `release.yml` for the run's release `version` while `tags`
/// are the repository's published releases.
fn run(version: &str, tags: &[&str]) -> Ended {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-downgrade-run");
    let root = TestDir::under(&base, "run");
    let releases = root.join("bin/releases").join(REPO);
    std::fs::create_dir_all(&releases).expect("make the releases directory");
    std::fs::create_dir_all(root.join("temp")).expect("make the temp directory");
    std::fs::hard_link(GH, root.join("bin/gh")).expect("install the fake gh");
    let list: Vec<serde_json::Value> = tags
        .iter()
        .map(|t| serde_json::json!({ "tagName": t }))
        .collect();
    std::fs::write(
        releases.join("release-list.json"),
        serde_json::to_vec(&list).expect("list"),
    )
    .expect("write the release list");
    let doc = yaml::load(&release_yml()).expect("release.yml parses");
    let job = job(&doc, JOB).expect("release.yml has the job no-downgrade");
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(yaml::Value::Seq(items)) => items,
        _ => panic!("the job has no steps"),
    };
    let path = format!("{}:/usr/bin:/bin", root.join("bin").display());
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let mut printed = String::new();
    for (at, step) in steps.iter().enumerate() {
        let text_of = |key: &str| {
            entry(step, key)
                .and_then(yaml::Node::as_str)
                .map(str::to_owned)
        };
        let name = text_of("name").expect("each step is named");
        if let Some(condition) = text_of("if") {
            let context = Context {
                names: outputs.clone(),
                cancelled: false,
                failed: false,
            };
            if !holds(&condition, &context).expect("the test reads the condition") {
                continue;
            }
        }
        let output_file = root.join("temp").join(format!("output-{at}"));
        let mut command = Command::new("bash");
        command
            .args(["--noprofile", "--norc", "-eo", "pipefail", "-c"])
            .arg(text_of("run").expect("every step runs a command"))
            .env_clear()
            .env("PATH", &path)
            .env("GITHUB_OUTPUT", &output_file)
            .env("GITHUB_REPOSITORY", REPO);
        for (k, v) in entry(step, "env").map_or(&[][..], entries) {
            let (k, v) = (k.as_str().expect("env key"), v.as_str().expect("env value"));
            command.env(k, evaluate(version, &outputs, v));
        }
        let out = command.output().expect("run bash");
        if !out.status.success() {
            return Ended::Failed(name);
        }
        printed.push_str(&String::from_utf8(out.stdout).expect("text"));
        let id = text_of("id");
        let written = std::fs::read_to_string(&output_file).unwrap_or_default();
        for (k, v) in written.lines().filter_map(|l| l.split_once('=')) {
            let id = id.as_deref().expect("a step that writes outputs has an id");
            outputs.insert(format!("steps.{id}.outputs.{k}"), v.to_owned());
        }
    }
    let job_outputs = OUTPUTS
        .iter()
        .map(|(k, v)| ((*k).to_owned(), evaluate(version, &outputs, v)))
        .collect();
    Ended::Passed(job_outputs, printed)
}

fn passed(set: bool, newest: &str, printed: &str) -> Ended {
    Ended::Passed(
        BTreeMap::from([
            ("attempt".to_owned(), "1".to_owned()),
            ("newest".to_owned(), newest.to_owned()),
            ("set".to_owned(), set.to_string()),
        ]),
        printed.to_owned(),
    )
}

/// The warning the job prints for the run's `version` while `newest` is published.
fn warning(version: &str, newest: &str) -> String {
    format!(
        "::warning::Not setting hostkeeper {version} as the desired version on the coordinator: {newest} is published, a newer release, and setting {version} would move every host back. This is a run of an older release (a re-run of its jobs, or a late run); the newest release's own run sets the fleet. A move back is an operator's step on the coordinator, never this workflow's.\n"
    )
}

#[test]
fn a_run_of_an_older_release_is_not_set_and_says_why() {
    // Catches: the re-run of v0.1.250's failed jobs after v0.1.251 was set, which set 0.1.250
    // again and moved every host back, with nothing in the log.
    assert_eq!(
        run("0.1.250", &["v0.1.251", "v0.1.250", "v0.1.249"]),
        passed(false, "0.1.251", &warning("0.1.250", "0.1.251"))
    );
}

#[test]
fn the_newest_release_is_set_and_nothing_is_said() {
    // Catches: a check that refuses the run's own release, the newest, or warns while it sets.
    assert_eq!(
        run("0.1.251", &["v0.1.251", "v0.1.250"]),
        passed(true, "0.1.251", "")
    );
}

#[test]
fn versions_compare_by_number_and_other_tags_are_passed_over() {
    // Catches: versions compared as text (0.1.99 above 0.1.100), and a tag that is not a
    // release's v<X.Y.Z> (another product's, a pre-release) read as a newer release.
    assert_eq!(
        run(
            "0.1.100",
            &["v0.1.99", "v0.1.100", "v9.9.9-rc1", "hello-v9.9.9"]
        ),
        passed(true, "0.1.100", "")
    );
    assert_eq!(
        run("0.1.99", &["v0.1.99", "v0.1.100"]),
        passed(false, "0.1.100", &warning("0.1.99", "0.1.100"))
    );
    assert_eq!(
        run("0.2.0", &["v0.2.0", "v0.1.300", "v1.0.0"]),
        passed(false, "1.0.0", &warning("0.2.0", "1.0.0"))
    );
}

#[test]
fn no_published_release_fails_the_job() {
    // Catches: an empty or unreadable list read as "nothing newer", which would let the set go on
    // with nothing checked.
    assert_eq!(
        run("0.1.251", &[]),
        Ended::Failed("Whether this run's release is still the newest published".to_owned())
    );
}

/// What the set's `if:` reads in attempt `attempt` of a run whose channel writes landed and whose
/// check of no-downgrade gave `set` in the attempt `checked`.
fn set_context(set: &str, checked: &str, attempt: &str) -> Context {
    let names = [
        ("needs.plan.outputs.release", "true"),
        ("github.ref", "refs/heads/main"),
        ("vars.RELEASE_SETS_DESIRED", "true"),
        ("needs.targets.outputs.newest", "true"),
        ("needs.coordinator.result", "success"),
        ("needs.testing-apt-write.outputs.conflict", "false"),
        ("needs.testing-pkg-upload.outputs.enabled", "true"),
        ("needs.no-downgrade.outputs.set", set),
        ("needs.no-downgrade.outputs.attempt", checked),
        ("github.run_attempt", attempt),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    Context {
        names,
        cancelled: false,
        failed: false,
    }
}

#[test]
fn set_desired_runs_only_after_a_check_of_its_own_attempt_said_set() {
    // Catches: the set run on a check of an earlier attempt (a re-run of the set alone, after a
    // newer release was set), on a check that said not to, or with the check skipped.
    for (name, set, checked, attempt, runs) in [
        (
            "checked and set in the first attempt",
            "true",
            "1",
            "1",
            true,
        ),
        ("checked again and set in a re-run", "true", "2", "2", true),
        (
            "the check said a newer release is published",
            "false",
            "2",
            "2",
            false,
        ),
        (
            "the set re-run alone after a check of attempt 1",
            "true",
            "1",
            "2",
            false,
        ),
        ("the check was skipped", "", "", "1", false),
    ] {
        assert_eq!(
            holds(SET_DESIRED_IF, &set_context(set, checked, attempt)),
            Ok(runs),
            "{name}"
        );
    }
}

#[test]
fn the_set_runs_under_the_checks_condition_and_its_answer() {
    // Catches: the check and the set drifting apart, so the check is skipped where the set runs.
    let inner = &GUARD_IF[..GUARD_IF.len() - 3];
    assert_eq!(SET_DESIRED_IF, format!("{inner}{SET_TERMS} }}}}"));
    assert_eq!(
        release_yml()
            .matches(&format!("    if: {GUARD_IF}\n"))
            .count(),
        1
    );
    assert_eq!(lint("release.yml", &release_yml()), Vec::<String>::new());
}

/// Every problem the lint finds in release.yml with the one occurrence of `from` in the jobs
/// no-downgrade and set-desired (from the first's name to the comment of the job after the
/// second) replaced by `to`.
fn problems_with(from: &str, to: &str) -> Vec<String> {
    let text = release_yml();
    let at = text.find("\n  no-downgrade:\n").expect("the job is there");
    let end = text.find("\n  # newest ").expect("the job newest follows");
    let changed = format!(
        "{}{}{}",
        &text[..at],
        with_in(&text[at..end], from, to),
        &text[end..]
    );
    lint("release.yml", &changed)
}

#[test]
fn a_set_that_does_not_wait_for_the_check_of_its_attempt_is_refused() {
    // Catches: the set no longer needing the check, or reading its answer without its attempt.
    let set_if = format!("    if: {SET_DESIRED_IF}\n");
    let want = [
        format!("release.yml: job set-desired needs the exact `if: {SET_DESIRED_IF}`"),
        format!("job set-desired needs the exact `if: {SET_DESIRED_IF}`"),
    ];
    for term in [
        " && needs.no-downgrade.outputs.attempt == github.run_attempt",
        " && needs.no-downgrade.outputs.set == 'true'",
    ] {
        assert_eq!(
            problems_with(&set_if, &set_if.replacen(term, "", 1)),
            want,
            "{term}"
        );
    }
    let needs = "testing-pkg-upload, no-downgrade]\n";
    let mut got = guard_needs();
    got.retain(|j| *j != "no-downgrade");
    assert_eq!(
        problems_with(needs, "testing-pkg-upload]\n"),
        [format!(
            "release.yml: job set-desired needs {got:?}, not {:?}",
            super::desired::caller_needs()
        )]
    );
}

#[test]
fn a_check_that_is_not_made_again_with_the_jobs_it_follows_is_refused() {
    // Catches: the check not needing the coordinator wait (a re-run of that wait, the incident's,
    // would not re-run the check), or run under another condition than the set's.
    let line = "  no-downgrade:\n    needs: [plan, targets, coordinator, ";
    let mut got = guard_needs();
    got.retain(|j| *j != "coordinator");
    assert_eq!(
        problems_with(line, "  no-downgrade:\n    needs: [plan, targets, "),
        [format!(
            "release.yml: job no-downgrade needs {got:?}, not {:?}",
            guard_needs()
        )]
    );
    let guard_if = format!("    if: {GUARD_IF}\n");
    assert_eq!(
        problems_with(
            &guard_if,
            &guard_if.replacen(" && needs.coordinator.result == 'success'", "", 1)
        ),
        [format!(
            "release.yml: job no-downgrade needs the exact `if: {GUARD_IF}`"
        )]
    );
    assert_eq!(
        problems_with(
            "    timeout-minutes: 5\n",
            "    timeout-minutes: 5\n    continue-on-error: true\n"
        ),
        ["release.yml: job no-downgrade has `continue-on-error`: a failure must stop the deploy"]
    );
}

#[test]
fn a_check_with_other_rights_steps_or_outputs_is_refused() {
    // Catches: a write scope on the check; the comparison made as text or the warning dropped, so
    // the log no longer says why the set was skipped; and the attempt output dropped.
    let got = problems_with(
        "      contents: read\n    outputs:\n      set:",
        "      contents: read\n      issues: read\n    outputs:\n      set:",
    );
    assert_eq!(
        got,
        ["release.yml: job no-downgrade must hold exactly `contents: read`"]
    );
    let compare = r#""set=\(. <= (env.VERSION | split(".") | map(tonumber)))""#;
    let as_text = r#""set=\(map(tostring) | join(".") <= env.VERSION)""#;
    let got = problems_with(compare, as_text);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with("release.yml: job no-downgrade steps are "),
        "{got:?}"
    );
    let got = problems_with("would move every host back", "is older");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with("release.yml: job no-downgrade steps are "),
        "{got:?}"
    );
    assert_eq!(
        problems_with("      attempt: ${{ github.run_attempt }}\n", ""),
        [format!(
            "release.yml: job no-downgrade outputs are {:?}, not {OUTPUTS:?}",
            &OUTPUTS[1..]
        )]
    );
    assert_eq!(STEPS.len(), 2);
}
