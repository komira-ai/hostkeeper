//! The rules of `pr-device.yml` ([`FILE`]), the one workflow that runs a macOS job or a job as
//! root for a pull request, and only when the pull request changes what those jobs prove.
//!
//! [`super::lint`] refuses a macOS job and a job as root in every workflow that runs for a pull
//! request but the jobs of [`JOBS`] that may (`macos` and `root` of each entry). [`problems`]
//! holds the file to its shape, so that what such a job may do is fixed here:
//! - its `on` is exactly `pull_request` with no filter (no `types`, `branches` or `paths`): the
//!   workflow starts for every pull request, so [`GATE_JOB`] always reports;
//! - its `concurrency` is exactly the group [`GROUP`], one per pull request, with
//!   `cancel-in-progress: true`: a newer push cancels the older run and its macOS runners;
//! - its jobs are exactly those of [`JOBS`]; each has exactly the `if:` and `needs` its entry
//!   gives (the jobs that need a list run only when [`CHANGES_JOB`] says a changed path is on it;
//!   [`GATE_JOB`] runs always and needs every other job), and runs on a runner of its entry.
//!
//! [`mirror_problems`] holds the jobs of [`JOBS`] whose `mirrors` is set to privileged.yml's job
//! of the same name, key for key, `needs` and `if:` aside, and the file's top-level `env` to that
//! file's, so a pull request runs exactly what runs after the merge. Every other rule of
//! [`super::lint`] holds for the file as for any pull-request workflow: no write scope (no
//! `id-token: write`), no secret and no token named, pinned actions, hosted runners.

use std::collections::BTreeSet;

use super::lint::{
    MACOS_DEVICE_JOB, MACOS_DEVICE_RUNNERS, PRIVILEGED_RUNNERS, entries, entry, triggers,
};
use super::yaml::{self, Node, Value};

/// The workflow.
pub const FILE: &str = "pr-device.yml";
/// Its job that says which lists the pull request's changed paths are on.
pub const CHANGES_JOB: &str = "changes";
/// Its job that a pull request needs: it fails when another job failed or was cancelled.
pub const GATE_JOB: &str = "gate";
/// The `if:` of the jobs of the Mac list.
pub const MAC_IF: &str = "${{ needs.changes.outputs.mac == 'true' }}";
/// The `if:` of the job of the Linux privileged list.
pub const LINUX_IF: &str = "${{ needs.changes.outputs.linux-privileged == 'true' }}";
/// The `if:` of [`GATE_JOB`]: it runs whatever the others did.
pub const GATE_IF: &str = "${{ always() }}";
/// The concurrency group: one per pull request.
pub const GROUP: &str = "pr-device-${{ github.event.pull_request.number }}";

/// A job of [`FILE`]: its name, its `if:` (none for [`CHANGES_JOB`]), whether it may name macOS
/// and run as root on a pull request, whether it is a copy of privileged.yml's job of its name,
/// and the runners it may run on.
pub struct Job {
    pub name: &'static str,
    pub condition: Option<&'static str>,
    pub macos: bool,
    pub root: bool,
    pub mirrors: bool,
    pub runners: &'static [&'static str],
}

/// Every job of [`FILE`].
pub const JOBS: &[Job] = &[
    Job {
        name: CHANGES_JOB,
        condition: None,
        macos: false,
        root: false,
        mirrors: false,
        runners: PRIVILEGED_RUNNERS,
    },
    Job {
        name: "macos-test",
        condition: Some(MAC_IF),
        macos: true,
        root: false,
        mirrors: false,
        runners: MACOS_DEVICE_RUNNERS,
    },
    Job {
        name: MACOS_DEVICE_JOB,
        condition: Some(MAC_IF),
        macos: true,
        root: true,
        mirrors: true,
        runners: MACOS_DEVICE_RUNNERS,
    },
    Job {
        name: "privileged",
        condition: Some(LINUX_IF),
        macos: false,
        root: true,
        mirrors: true,
        runners: PRIVILEGED_RUNNERS,
    },
    Job {
        name: GATE_JOB,
        condition: Some(GATE_IF),
        macos: false,
        root: false,
        mirrors: false,
        runners: PRIVILEGED_RUNNERS,
    },
];

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

/// The job names `needs` lists, as a scalar or a list.
fn needs(job: &Node) -> Option<BTreeSet<&str>> {
    let needs = entry(job, "needs")?;
    Some(match &needs.value {
        Value::Seq(items) => items.iter().filter_map(Node::as_str).collect(),
        _ => needs.as_str().into_iter().collect(),
    })
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
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    let named: BTreeSet<&str> = jobs.iter().filter_map(|(name, _)| name.as_str()).collect();
    for wanted in JOBS.iter().filter(|j| !named.contains(j.name)) {
        problems.push(format!("no job {}", wanted.name));
    }
    let others: BTreeSet<&str> = JOBS
        .iter()
        .map(|j| j.name)
        .filter(|name| *name != GATE_JOB)
        .collect();
    for (name, node) in jobs {
        let name = name.as_str().unwrap_or_default();
        let Some(wanted) = job(FILE, name) else {
            problems.push(format!("job {name} is not a job of {FILE}"));
            continue;
        };
        let condition = entry(node, "if").and_then(Node::as_str);
        if condition != wanted.condition {
            problems.push(match wanted.condition {
                Some(c) => format!("job {name}: its `if:` must be exactly `{c}`"),
                None => format!("job {name}: has an `if:`, and runs for every pull request"),
            });
        }
        let want_needs = match name {
            CHANGES_JOB => None,
            GATE_JOB => Some(others.clone()),
            _ => Some(BTreeSet::from([CHANGES_JOB])),
        };
        if needs(node) != want_needs {
            problems.push(match want_needs {
                Some(n) => format!("job {name}: its `needs` must be exactly {n:?}"),
                None => format!("job {name}: needs no job"),
            });
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

/// `job` without its `needs` and `if:`.
fn without_needs_and_if(job: &Node) -> Node {
    let kept = entries(job)
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), Some("needs" | "if")))
        .cloned()
        .collect();
    Node {
        value: Value::Map(kept),
        span: job.span,
    }
}

/// What differs between [`FILE`] (`text`) and privileged.yml (`privileged`): a job of [`JOBS`]
/// whose `mirrors` is set that is not privileged.yml's job of its name with `needs` and `if:`
/// added, and a top-level `env` that is not privileged.yml's.
pub fn mirror_problems(text: &str, privileged: &str) -> Vec<String> {
    let (doc, original) = match (yaml::load(text), yaml::load(privileged)) {
        (Ok(doc), Ok(original)) => (doc, original),
        (Err(e), _) | (_, Err(e)) => return vec![e],
    };
    let mut problems = Vec::new();
    let env = (entry(&doc, "env"), entry(&original, "env"));
    let same_env = match env {
        (Some(a), Some(b)) => same(a, b),
        (a, b) => a.is_none() && b.is_none(),
    };
    if !same_env {
        problems.push(format!("{FILE}: its top-level env is not privileged.yml's"));
    }
    for wanted in JOBS.iter().filter(|j| j.mirrors) {
        let copy = entry(&doc, "jobs").and_then(|jobs| entry(jobs, wanted.name));
        let source = entry(&original, "jobs").and_then(|jobs| entry(jobs, wanted.name));
        let copied = match (copy, source) {
            (Some(copy), Some(source)) => same(&without_needs_and_if(copy), source),
            _ => false,
        };
        if !copied {
            problems.push(format!(
                "job {}: not privileged.yml's job {0} with `needs` and `if:` added",
                wanted.name
            ));
        }
    }
    problems
}
