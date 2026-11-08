//! The rules of `pr-device.yml` ([`FILE`]), the one workflow that runs a macOS job or a job as
//! root for a pull request: on every pull request, each required check ([`REQUIRED`]) the check
//! of exactly one job, which always runs.
//!
//! [`super::lint`] refuses a macOS job and a job as root in every workflow that runs for a pull
//! request but the jobs of [`JOBS`] that may (`macos` and `root` of each entry). [`problems`]
//! holds the file to its shape, so that what such a job may do is fixed here:
//! - its `on` is exactly `pull_request` with no filter (no `types`, `branches` or `paths`): the
//!   workflow starts for every pull request, so a required check never waits for a run that did
//!   not start;
//! - its top-level keys are among [`TOP_LEVEL`] (no `defaults`, which would change the copies);
//! - its `concurrency` is exactly the group [`GROUP`], one per pull request, with
//!   `cancel-in-progress: true`: a newer push cancels the older run and its macOS runners;
//! - its jobs are exactly those of [`JOBS`], each on a runner of its entry, and each with the
//!   `name`, `needs` and `if:` of its [`Kind`]: [`Kind::Always`] has none; [`CHANGES_JOB`]
//!   ([`Kind::Changes`]) none either, and is the job [`super::pr_paths::changes_problems`] holds;
//!   a [`Kind::Gated`] job (the macOS jobs) is named `<job> on macos-15`, needs [`CHANGES_JOB`]
//!   and runs under [`GATED_IF`]; a [`Kind::Gate`] job is exactly [`gate_yaml`] of the job it
//!   reports on, the required check of that job's name.
//!
//! [`mirror_problems`] holds the jobs of [`JOBS`] whose `mirrors` is set to privileged.yml's job
//! of the same name, key for key (a gated copy but its `name`, `needs` and `if:`, which
//! [`problems`] holds), and the file's top-level `env` to that file's, so a pull request runs
//! exactly what runs after the merge. [`crate_problems`] holds the crates [`MACOS_TEST_JOB`] builds
//! and tests to a given set (the workflow test gives the path dependencies of the `hostkeeper` and
//! `hello` binaries). Every other rule of [`super::lint`] holds for the file as for any
//! pull-request workflow: no write scope (no `id-token: write`), no secret and no token named,
//! pinned actions, hosted runners.

use std::collections::BTreeSet;

use super::lint::{
    MACOS_DEVICE_JOB, MACOS_DEVICE_RUNNERS, PRIVILEGED_RUNNERS, entries, entry, triggers,
};
use super::pr_paths;
use super::yaml::{self, Node, Value};

/// The workflow.
pub const FILE: &str = "pr-device.yml";
/// Its job that runs the tests of the Mac's crates on macOS, as the runner's user.
pub const MACOS_TEST_JOB: &str = "macos-test";
/// The concurrency group: one per pull request.
pub const GROUP: &str = "pr-device-${{ github.event.pull_request.number }}";

/// The checks of [`FILE`] a pull request needs, by name: each is the check of exactly one job,
/// which runs on every pull request.
pub const REQUIRED: [&str; 3] = [MACOS_TEST_JOB, MACOS_DEVICE_JOB, PRIVILEGED_JOB];

/// The job that runs the privileged tests against systemd, privileged.yml's job of that name.
pub const PRIVILEGED_JOB: &str = "privileged";

/// The job that says whether the pull request changes a path the macOS jobs build, test or read
/// ([`super::pr_paths`]).
pub const CHANGES_JOB: &str = "changes";

/// The runner of the Linux jobs of [`FILE`] that are not copies: [`CHANGES_JOB`] and the gates.
pub const LINUX_RUNNERS: &[&str] = &["ubuntu-24.04"];

/// The condition of a [`Kind::Gated`] job: it runs unless [`CHANGES_JOB`] passed and answered
/// `needed=false`, so a changes job that failed, was cancelled or gave no answer runs it; it does
/// not run in a cancelled run.
pub const GATED_IF: &str = "${{ !cancelled() && !(needs.changes.result == 'success' && needs.changes.outputs.needed == 'false') }}";

/// How a job of [`FILE`] runs and reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// On every pull request, a check of its own name: no `name`, `needs` or `if:`.
    Always,
    /// [`CHANGES_JOB`]: on every pull request, no `name`, `needs` or `if:`.
    Changes,
    /// A macOS job, skipped when [`CHANGES_JOB`] answers `needed=false`: named
    /// `<job> on macos-15`, needs [`CHANGES_JOB`], runs under [`GATED_IF`].
    Gated,
    /// The required check named after the gated job `of`: [`gate_yaml`] of it.
    Gate { of: &'static str },
}

/// A job of [`FILE`]: its name, whether it may name macOS and run as root on a pull request,
/// whether it is a copy of privileged.yml's job of its name, the runners it may run on, and how it
/// runs and reports.
pub struct Job {
    pub name: &'static str,
    pub macos: bool,
    pub root: bool,
    pub mirrors: bool,
    pub runners: &'static [&'static str],
    pub kind: Kind,
}

/// Every job of [`FILE`], in the order of the file.
pub const JOBS: &[Job] = &[
    Job {
        name: CHANGES_JOB,
        macos: false,
        root: false,
        mirrors: false,
        runners: LINUX_RUNNERS,
        kind: Kind::Changes,
    },
    Job {
        name: MACOS_TEST_JOB,
        macos: true,
        root: false,
        mirrors: false,
        runners: MACOS_DEVICE_RUNNERS,
        kind: Kind::Gated,
    },
    Job {
        name: "macos-test-check",
        macos: true,
        root: false,
        mirrors: false,
        runners: LINUX_RUNNERS,
        kind: Kind::Gate { of: MACOS_TEST_JOB },
    },
    Job {
        name: MACOS_DEVICE_JOB,
        macos: true,
        root: true,
        mirrors: true,
        runners: MACOS_DEVICE_RUNNERS,
        kind: Kind::Gated,
    },
    Job {
        name: "macos-device-check",
        macos: true,
        root: false,
        mirrors: false,
        runners: LINUX_RUNNERS,
        kind: Kind::Gate {
            of: MACOS_DEVICE_JOB,
        },
    },
    Job {
        name: PRIVILEGED_JOB,
        macos: false,
        root: true,
        mirrors: true,
        runners: PRIVILEGED_RUNNERS,
        kind: Kind::Always,
    },
];

/// The check name of the gated job `job`: not `job`, which is its gate's.
pub fn gated_name(job: &str) -> String {
    format!("{job} on {}", MACOS_DEVICE_RUNNERS[0])
}

/// The one job [`FILE`] reports the gated job `of` with, as YAML (a job of `jobs:`, its key
/// left out): on Linux, holding nothing, after [`CHANGES_JOB`] and `of` whatever they did
/// (`always()`, cancelled runs included, since a skipped check passes); its first step states
/// that the path rule skipped `of`, when it did, and its second fails unless `of` passed or was
/// skipped on `needed=false`.
pub fn gate_yaml(of: &str) -> String {
    let not_needed = "needs.changes.result == 'success' && needs.changes.outputs.needed == 'false'";
    format!(
        "name: {of}
needs: [changes, {of}]
if: ${{{{ always() }}}}
runs-on: {runner}
permissions: {{}}
timeout-minutes: 5
steps:
  - name: Skipped by the path rule
    if: ${{{{ needs.{of}.result == 'skipped' && {not_needed} }}}}
    run: echo \"{of} was skipped by the path rule of pr-device.yml; every changed path is on the skip list of its job changes\"
  - name: {gated} did not pass
    if: ${{{{ always() && !(needs.{of}.result == 'success' || (needs.{of}.result == 'skipped' && {not_needed})) }}}}
    run: exit 1
",
        runner = LINUX_RUNNERS[0],
        gated = gated_name(of),
    )
}

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
        problems.extend(kind_problems(name, wanted.kind, node));
        let runner = entry(node, "runs-on").and_then(Node::as_str);
        if !runner.is_some_and(|r| wanted.runners.contains(&r)) {
            problems.push(format!(
                "job {name}: runs on a runner outside {:?}",
                wanted.runners
            ));
        }
    }
    problems.extend(pr_paths::changes_problems(doc));
    problems
}

/// The keys a [`Kind::Gated`] job has that a job reporting on its own has not.
const GATED_KEYS: [&str; 3] = ["name", "needs", "if"];

/// What is wrong with the `name`, `needs` and `if:` of the job `name` (`node`) of kind `kind`.
fn kind_problems(name: &str, kind: Kind, node: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let text = |key: &str| entry(node, key).and_then(Node::as_str);
    match kind {
        Kind::Always | Kind::Changes => {
            for key in GATED_KEYS {
                if entry(node, key).is_some() {
                    problems.push(format!(
                        "job {name}: has `{key}`, and must run and report on every pull request"
                    ));
                }
            }
        }
        Kind::Gated => {
            if text("name") != Some(gated_name(name).as_str()) {
                problems.push(format!("job {name}: must be named `{}`", gated_name(name)));
            }
            if text("needs") != Some(CHANGES_JOB) {
                problems.push(format!("job {name}: must need exactly {CHANGES_JOB}"));
            }
            if text("if") != Some(GATED_IF) {
                problems.push(format!("job {name}: must run under `if: {GATED_IF}`"));
            }
        }
        Kind::Gate { of } => {
            let want = yaml::load(&gate_yaml(of));
            if !want.is_ok_and(|want| same(node, &want)) {
                problems.push(format!(
                    "job {name}: not the gate of {of} (pr_device::gate_yaml), key for key"
                ));
            }
        }
    }
    problems
}

/// `job` without the keys of [`GATED_KEYS`].
fn without_gated_keys(job: &Node) -> Node {
    let mut copy = job.clone();
    if let Value::Map(entries) = &mut copy.value {
        entries.retain(|(k, _)| !k.as_str().is_some_and(|k| GATED_KEYS.contains(&k)));
    }
    copy
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
/// whose `mirrors` is set that is not privileged.yml's job of its name (a [`Kind::Gated`] copy
/// without its `name`, `needs` and `if:`), and a top-level `env` that is not privileged.yml's.
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
        let copy = copy.map(|c| match wanted.kind {
            Kind::Gated => without_gated_keys(c),
            _ => c.clone(),
        });
        if !matches!((copy, source), (Some(c), Some(s)) if same(&c, s)) {
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
