//! The rules of `pr-device.yml` ([`FILE`]), the one workflow that runs a job as root for a pull
//! request: on every pull request, each required check ([`REQUIRED`]) the check of exactly one
//! job, which always runs. No workflow runs a macOS job for a pull request (AGENTS.md B9): the
//! Mac's tests are the stage `macos-test` and `macos-device` of release.yml
//! ([`super::release_macos`]), and [`super::lint`] refuses a macOS job in every workflow that runs
//! for a pull request, this one included.
//!
//! [`super::lint`] refuses a job as root in every workflow that runs for a pull request but the
//! jobs of [`JOBS`] that may (`root` of each entry). [`problems`] holds the file to its shape, so
//! that what such a job may do is fixed here:
//! - its `on` is exactly `pull_request` with no filter (no `types`, `branches` or `paths`): the
//!   workflow starts for every pull request, so a required check never waits for a run that did
//!   not start;
//! - its top-level keys are among [`TOP_LEVEL`] (no `defaults`, which would change the copies);
//! - its `concurrency` is exactly the group [`GROUP`], one per pull request, with
//!   `cancel-in-progress: true`: a newer push cancels the older run;
//! - its jobs are exactly those of [`JOBS`], each on a runner of its entry, with no `name`,
//!   `needs` or `if:`: each runs on every pull request, as a check of its own name.
//!
//! [`mirror_problems`] holds every job of [`JOBS`] to privileged.yml's job of the same name, key
//! for key, and the file's top-level `env` to that file's, so a pull request runs exactly what
//! runs after the merge. Every other rule of [`super::lint`] holds for the file as for any
//! pull-request workflow: no write scope (no `id-token: write`), no secret and no token named,
//! pinned actions, hosted runners.

use std::collections::BTreeSet;

use super::lint::{OPENRC_DEVICE_JOB, PRIVILEGED_RUNNERS, entries, entry, triggers};
use super::yaml;
use super::yaml::{Node, Value};

/// The workflow.
pub const FILE: &str = "pr-device.yml";
/// The concurrency group: one per pull request.
pub const GROUP: &str = "pr-device-${{ github.event.pull_request.number }}";

/// The checks of [`FILE`] a pull request needs, by name: each is the check of exactly one job,
/// which runs on every pull request.
pub const REQUIRED: [&str; 2] = [PRIVILEGED_JOB, OPENRC_DEVICE_JOB];

/// The job that runs the privileged tests against systemd, privileged.yml's job of that name.
pub const PRIVILEGED_JOB: &str = "privileged";

/// A job of [`FILE`]: its name and the runners it may run on. Each may run as root and is a copy
/// of privileged.yml's job of its name.
pub struct Job {
    pub name: &'static str,
    pub runners: &'static [&'static str],
}

/// Every job of [`FILE`], in the order of the file.
pub const JOBS: &[Job] = &[
    Job {
        name: PRIVILEGED_JOB,
        runners: PRIVILEGED_RUNNERS,
    },
    Job {
        name: OPENRC_DEVICE_JOB,
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

/// Whether the job `name` of `file` may run as root on a pull request.
pub fn root_allowed(file: &str, name: &str) -> bool {
    job(file, name).is_some()
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
        for key in ["name", "needs", "if"] {
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
/// that is not privileged.yml's job of its name, and a top-level `env` that is not privileged.yml's.
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
    for wanted in JOBS {
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
