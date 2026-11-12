//! The rules of the macOS stage of `release.yml` ([`FILE`]): the Mac's tests, which no pull request
//! runs (AGENTS.md B9; [`super::lint`] refuses a macOS job in every workflow that runs for a pull
//! request), run here, after the build and before the Mac's installer is promoted to stable, for
//! the newest release of a run. Two jobs, [`JOBS`]:
//! - [`TEST_JOB`]: the tests of the crates the `hostkeeper` and `hello` binaries are built from,
//!   as the runner's user ([`crate_problems`] holds the crate list to the binaries' path
//!   dependencies);
//! - [`DEVICE_JOB`]: the device tests under launchd as root, the one job of the file that runs as
//!   root ([`root_allowed`]); the workflow lint holds its test targets to the files of
//!   `crates/hostkeeper-privileged/tests` that start with the macOS attribute, its single `sudo`
//!   and its previous-release download (`previous_release.rs`).
//!
//! [`problems`] holds each of them to this shape, so that a failure of the Mac holds back the
//! Mac's installer and nothing else:
//! - it needs exactly [`NEEDS`] and runs under the release's `if:` ([`RELEASE_IF`]), on the
//!   runner [`RUNNER`], and checks out the commit of the release (`needs.newest.outputs.commit`);
//! - it holds `contents: read` alone, with no environment, no secret and no token;
//! - the job `promote-pkg` waits for both and runs only when both succeeded (the row of
//!   `packages.rs`), and no other job of the file names either as a dependency (the Linux
//!   packages, the coordinator and the fleet's desired version never wait for a Mac).

use std::collections::BTreeSet;

use super::lint::{MACOS_DEVICE_JOB, MACOS_DEVICE_RUNNERS, scalars};
use super::lint::{entries, entry};
use super::release::{RELEASE_IF, job, names, steps, text_of};
use super::yaml::{self, Node, Value};

/// The workflow.
pub const FILE: &str = "release.yml";
/// The job that runs the tests of the Mac's crates, as the runner's user.
pub const TEST_JOB: &str = "macos-test";
/// The job that runs the device tests under launchd, as root.
pub const DEVICE_JOB: &str = MACOS_DEVICE_JOB;
/// The jobs of the macOS stage.
pub const JOBS: [&str; 2] = [TEST_JOB, DEVICE_JOB];
/// The runner of both: a standard hosted arm64 macOS image of a fixed version.
pub const RUNNER: &str = MACOS_DEVICE_RUNNERS[0];
/// What each job needs, sorted: the plan said to release, and the newest release's commit.
pub const NEEDS: [&str; 2] = ["newest", "plan"];
/// The checkout of the commit of the release.
pub const CHECKOUT_REF: &str = "${{ needs.newest.outputs.commit }}";

/// Whether the job `name` of `file` may run as root: the device job of the macOS stage.
pub fn root_allowed(file: &str, name: &str) -> bool {
    file == FILE && name == DEVICE_JOB
}

/// Whether the job `name` of release.yml is a job of the macOS stage, which builds and tests the
/// repository with `cargo` as a test run and not as a release build.
pub fn is_stage_job(name: &str) -> bool {
    JOBS.contains(&name)
}

/// What is wrong with the macOS stage in the release.yml document `doc`, one line per problem.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for name in JOBS {
        let Some(j) = job(doc, name) else {
            // The list of jobs reports a missing job.
            continue;
        };
        let say = |what: &str| format!("release.yml: job {name} {what}");
        if text_of(Some(j), "runs-on") != Some(RUNNER) {
            problems.push(say(&format!("must run on {RUNNER}")));
        }
        if names(entry(j, "needs")) != NEEDS {
            problems.push(say(&format!("must need exactly {NEEDS:?}")));
        }
        if text_of(Some(j), "if") != Some(RELEASE_IF) {
            problems.push(say(&format!("needs the exact `if: {RELEASE_IF}`")));
        }
        let permissions: Vec<(&str, &str)> = entry(j, "permissions")
            .map_or(&[][..], entries)
            .iter()
            .filter_map(|(k, v)| Some((k.as_str()?, v.as_str()?)))
            .collect();
        if permissions != [("contents", "read")] {
            problems.push(say("must hold `contents: read` alone"));
        }
        for key in [
            "environment",
            "strategy",
            "container",
            "services",
            "outputs",
        ] {
            if entry(j, key).is_some() {
                problems.push(say(&format!("must not have `{key}`")));
            }
        }
        let checkouts: Vec<&Node> = steps(Some(j))
            .iter()
            .filter(|s| {
                text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/checkout@"))
            })
            .collect();
        let at_release = checkouts
            .iter()
            .all(|s| text_of(entry(s, "with"), "ref") == Some(CHECKOUT_REF));
        if checkouts.len() != 1 || !at_release {
            problems.push(say(&format!(
                "must check out the commit of the release once (`ref: {CHECKOUT_REF}`)"
            )));
        }
        let mut values = Vec::new();
        scalars(j, &mut values);
        let secret = values.iter().flat_map(|v| v.lines()).find(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("secrets")
                || lower.contains("github.token")
                || lower.contains("tojson(github")
        });
        if let Some(line) = secret {
            problems.push(say(&format!("names a secret or the token (`{line}`)")));
        }
    }
    problems.extend(waiters(doc));
    problems
}

/// The jobs of release.yml that name a job of the stage in their `needs`, other than the one that
/// is held to wait for them (`promote-pkg`, by its row in `packages.rs`): none may, so a Mac that
/// is red or a macOS runner that never comes holds nothing else back.
fn waiters(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, j) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        if name == "promote-pkg" || is_stage_job(name) {
            continue;
        }
        for waited in names(entry(j, "needs")).iter().filter(|n| is_stage_job(n)) {
            problems.push(format!(
                "release.yml: job {name} waits for {waited}; only promote-pkg may wait for the macOS stage"
            ));
        }
    }
    problems
}

/// The packages a `cargo test` line names with `-p`.
fn packages(run: &str) -> BTreeSet<&str> {
    let words: Vec<&str> = run.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[0] == "-p")
        .map(|pair| pair[1])
        .collect()
}

/// What is wrong with the crates [`TEST_JOB`] of release.yml (`text`) tests, against `want`:
/// other than exactly one step that builds the tests (`cargo test --locked --no-run`) and one that
/// runs them (`cargo test --locked --no-fail-fast`), each naming exactly `want` with `-p` and
/// nothing else (no `--workspace`, no `--exclude`, no other flag).
pub fn crate_problems(text: &str, want: &BTreeSet<String>) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let runs: Vec<&str> = match entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, TEST_JOB))
        .and_then(|job| entry(job, "steps"))
        .map(|s| &s.value)
    {
        Some(Value::Seq(steps)) => steps
            .iter()
            .filter_map(|step| entry(step, "run").and_then(Node::as_str))
            .filter(|run| run.starts_with("cargo test"))
            .collect(),
        _ => Vec::new(),
    };
    let mut problems = Vec::new();
    for start in [
        "cargo test --locked --no-run",
        "cargo test --locked --no-fail-fast",
    ] {
        let found: Vec<&&str> = runs.iter().filter(|r| r.starts_with(start)).collect();
        let [run] = found[..] else {
            problems.push(format!(
                "job {TEST_JOB}: {} steps `{start} ...`, not one",
                found.len()
            ));
            continue;
        };
        let named: BTreeSet<String> = packages(run).into_iter().map(str::to_owned).collect();
        let rest: Vec<&str> = run[start.len()..]
            .split_whitespace()
            .filter(|w| *w != "-p" && !named.contains(*w))
            .collect();
        if named != *want || !rest.is_empty() {
            problems.push(format!(
                "job {TEST_JOB}: `{start}` must name exactly the crates {want:?} with -p, and nothing else"
            ));
        }
    }
    if runs.len() != 2 {
        problems.push(format!(
            "job {TEST_JOB}: {} `cargo test` steps, not two",
            runs.len()
        ));
    }
    problems
}
