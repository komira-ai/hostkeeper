//! The rules of `pr-device.yml` ([`FILE`]), the one workflow that runs a macOS job or a job as
//! root for a pull request: on every pull request, each job a check of its own.
//!
//! [`super::lint`] refuses a macOS job and a job as root in every workflow that runs for a pull
//! request but the jobs of [`JOBS`] that may (`macos` and `root` of each entry). [`problems`]
//! holds the file to its shape, so that what such a job may do is fixed here:
//! - its `on` is exactly `pull_request` with no filter (no `types`, `branches` or `paths`): every
//!   job runs for every pull request, so a required check never waits for a run that did not
//!   start;
//! - its top-level keys are among [`TOP_LEVEL`] (no `defaults`, which would change the copies);
//! - its `concurrency` is exactly the group [`GROUP`], one per pull request, with
//!   `cancel-in-progress: true`: a newer push cancels the older run and its macOS runners;
//! - its jobs are exactly those of [`JOBS`], none with an `if:` or `needs` (each runs, and
//!   reports, on its own), each on a runner of its entry.
//!
//! [`mirror_problems`] holds the jobs of [`JOBS`] whose `mirrors` is set to privileged.yml's job
//! of the same name, key for key, and the file's top-level `env` to that file's, so a pull
//! request runs exactly what runs after the merge. [`crate_problems`] holds the crates
//! [`MACOS_TEST_JOB`] builds and tests to a given set (the workflow test gives the path
//! dependencies of the `hostkeeper` and `hello` binaries). Every other rule of [`super::lint`]
//! holds for the file as for any pull-request workflow: no write scope (no `id-token: write`), no
//! secret and no token named, pinned actions, hosted runners.

use std::collections::BTreeSet;

use super::lint::{
    MACOS_DEVICE_JOB, MACOS_DEVICE_RUNNERS, OPENRC_DEVICE_JOB, PRIVILEGED_RUNNERS, entries, entry,
    triggers,
};
use super::yaml::{self, Node, Value};

/// The workflow.
pub const FILE: &str = "pr-device.yml";
/// Its job that runs the tests of the Mac's crates on macOS, as the runner's user.
pub const MACOS_TEST_JOB: &str = "macos-test";
/// The concurrency group: one per pull request.
pub const GROUP: &str = "pr-device-${{ github.event.pull_request.number }}";

/// A job of [`FILE`]: its name, whether it may name macOS and run as root on a pull request,
/// whether it is a copy of privileged.yml's job of its name, and the runners it may run on.
pub struct Job {
    pub name: &'static str,
    pub macos: bool,
    pub root: bool,
    pub mirrors: bool,
    pub runners: &'static [&'static str],
}

/// Every job of [`FILE`].
pub const JOBS: &[Job] = &[
    Job {
        name: MACOS_TEST_JOB,
        macos: true,
        root: false,
        mirrors: false,
        runners: MACOS_DEVICE_RUNNERS,
    },
    Job {
        name: MACOS_DEVICE_JOB,
        macos: true,
        root: true,
        mirrors: true,
        runners: MACOS_DEVICE_RUNNERS,
    },
    Job {
        name: "privileged",
        macos: false,
        root: true,
        mirrors: true,
        runners: PRIVILEGED_RUNNERS,
    },
    Job {
        name: OPENRC_DEVICE_JOB,
        macos: false,
        root: true,
        mirrors: true,
        runners: PRIVILEGED_RUNNERS,
    },
];

/// The top-level keys [`FILE`] may have: a `defaults` or any other key would change how the
/// copies of privileged.yml's jobs run, where [`mirror_problems`] does not look.
pub const TOP_LEVEL: &[&str] = &["name", "on", "permissions", "concurrency", "env", "jobs"];

/// What [`FILE`] is refused with when its `on` is anything else.
pub const ON_PROBLEM: &str = "pr-device.yml: on must be pull_request alone, with no filter";
/// What [`FILE`] is refused with when its `concurrency` is anything else.
pub const CONCURRENCY_PROBLEM: &str =
    "pr-device.yml: concurrency must be the group of the pull request, cancel-in-progress: true";

/// The entry of [`JOBS`] for the job `job` of `file`, if it is one.
fn job(file: &str, job: &str) -> Option<&'static Job> {
    JOBS.iter().find(|j| file == FILE && j.name == job)
}

/// Whether the job `name` of `file` may name macOS on a pull request.
pub fn macos_allowed(file: &str, name: &str) -> bool {
    job(file, name).is_some_and(|j| j.macos)
}

/// Whether the job `name` of `file` may run as root on a pull request.
pub fn root_allowed(file: &str, name: &str) -> bool {
    job(file, name).is_some_and(|j| j.root)
}

/// Whether `on` is exactly `pull_request` with no filter: the one name, as a scalar, a list, or
/// a mapping whose one entry has no value or an empty one.
fn pull_request_only(on: Option<&Node>) -> bool {
    let Some(on) = on else { return false };
    let unfiltered = match &on.value {
        Value::Map(entries) => entries.iter().all(|(_, v)| {
            v.is_empty_map() || (matches!(v.value, Value::Scalar { .. }) && v.as_str().is_none())
        }),
        Value::Scalar { .. } | Value::Seq(_) => true,
    };
    triggers(on) == ["pull_request"] && unfiltered
}

/// Whether the workflow's `concurrency` is exactly [`GROUP`] with `cancel-in-progress: true`.
fn one_group_per_pull_request(concurrency: Option<&Node>) -> bool {
    let Some(concurrency) = concurrency else {
        return false;
    };
    let keys: BTreeSet<&str> = entries(concurrency)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    keys == BTreeSet::from(["group", "cancel-in-progress"])
        && entry(concurrency, "group").and_then(Node::as_str) == Some(GROUP)
        && entry(concurrency, "cancel-in-progress").and_then(Node::as_str) == Some("true")
}

/// What is wrong with the shape of [`FILE`], the document `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !pull_request_only(entry(doc, "on")) {
        problems.push(ON_PROBLEM.to_owned());
    }
    if !one_group_per_pull_request(entry(doc, "concurrency")) {
        problems.push(CONCURRENCY_PROBLEM.to_owned());
    }
    for (key, _) in entries(doc) {
        let key = key.as_str().unwrap_or_default();
        if !TOP_LEVEL.contains(&key) {
            problems.push(format!(
                "{FILE}: the top-level key `{key}` is not one of {TOP_LEVEL:?}"
            ));
        }
    }
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    let named: BTreeSet<&str> = jobs.iter().filter_map(|(name, _)| name.as_str()).collect();
    for wanted in JOBS.iter().filter(|j| !named.contains(j.name)) {
        problems.push(format!("no job {}", wanted.name));
    }
    for (name, node) in jobs {
        let name = name.as_str().unwrap_or_default();
        let Some(wanted) = job(FILE, name) else {
            problems.push(format!("job {name} is not a job of {FILE}"));
            continue;
        };
        for key in ["if", "needs"] {
            if entry(node, key).is_some() {
                problems.push(format!(
                    "job {name}: has `{key}`, and must run and report on every pull request"
                ));
            }
        }
        let runner = entry(node, "runs-on").and_then(Node::as_str);
        if !runner.is_some_and(|r| wanted.runners.contains(&r)) {
            problems.push(format!(
                "job {name}: runs on a runner outside {:?}",
                wanted.runners
            ));
        }
    }
    problems
}

/// Whether `a` and `b` are the same tree: the same scalars (and plainness), lists and mappings,
/// in the same order, wherever they were written.
pub fn same(a: &Node, b: &Node) -> bool {
    match (&a.value, &b.value) {
        (
            Value::Scalar { text, plain },
            Value::Scalar {
                text: other,
                plain: other_plain,
            },
        ) => text == other && plain == other_plain,
        (Value::Seq(x), Value::Seq(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((ka, va), (kb, vb))| same(ka, kb) && same(va, vb))
        }
        _ => false,
    }
}

/// What differs between [`FILE`] (`text`) and privileged.yml (`privileged`): a job of [`JOBS`]
/// whose `mirrors` is set that is not privileged.yml's job of its name, and a top-level `env`
/// that is not privileged.yml's.
pub fn mirror_problems(text: &str, privileged: &str) -> Vec<String> {
    let (doc, original) = match (yaml::load(text), yaml::load(privileged)) {
        (Ok(doc), Ok(original)) => (doc, original),
        (Err(e), _) | (_, Err(e)) => return vec![e],
    };
    let mut problems = Vec::new();
    let same_env = match (entry(&doc, "env"), entry(&original, "env")) {
        (Some(a), Some(b)) => same(a, b),
        (a, b) => a.is_none() && b.is_none(),
    };
    if !same_env {
        problems.push(format!("{FILE}: its top-level env is not privileged.yml's"));
    }
    for wanted in JOBS.iter().filter(|j| j.mirrors) {
        let copy = entry(&doc, "jobs").and_then(|jobs| entry(jobs, wanted.name));
        let source = entry(&original, "jobs").and_then(|jobs| entry(jobs, wanted.name));
        if !matches!((copy, source), (Some(c), Some(s)) if same(c, s)) {
            problems.push(format!(
                "job {}: not privileged.yml's job {0}, key for key",
                wanted.name
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

/// What is wrong with the crates [`MACOS_TEST_JOB`] of [`FILE`] (`text`) tests, against `want`:
/// other than exactly one step that builds the tests (`cargo test --locked --no-run`) and one that
/// runs them (`cargo test --locked --no-fail-fast`), each naming exactly `want` with `-p` and
/// nothing else (no `--workspace`, no `--exclude`, no other flag).
pub fn crate_problems(text: &str, want: &BTreeSet<String>) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let steps = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, MACOS_TEST_JOB))
        .and_then(|job| entry(job, "steps"));
    let runs: Vec<&str> = match steps.map(|s| &s.value) {
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
                "job {MACOS_TEST_JOB}: {} steps `{start} ...`, not one",
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
                "job {MACOS_TEST_JOB}: `{start}` must name exactly the crates {want:?} with -p, and nothing else"
            ));
        }
    }
    if runs.len() != 2 {
        problems.push(format!(
            "job {MACOS_TEST_JOB}: {} `cargo test` steps, not two",
            runs.len()
        ));
    }
    problems
}
