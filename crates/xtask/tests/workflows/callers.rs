//! The jobs of this repository that call the CI client, `set-desired.yml`
//! (docs/deploy/ci-client.md), and the rules of `hello-desired.yml`, the hello proof run.
//!
//! A job that calls a reusable workflow has no `runs-on` and no steps, so the shared rules of
//! [`super::lint`] refuse it, and grant `id-token: write` to no job of a caller. The one exception
//! is a job of [`CALLERS`] that calls the CI client. [`problems`] refuses the workflow when:
//! - a job calls `set-desired.yml` (its job-level `uses:` names the file) and is not in
//!   [`CALLERS`]: every way CI sets a desired version is listed here;
//! - a job of [`CALLERS`] calls another workflow than [`SET_DESIRED`] of this repository (the
//!   pin, a full commit with a version comment, is the shared rule's), has another `if:` than its
//!   entry gives, a `permissions` that is not exactly `contents: read` and `id-token: write`, or
//!   passes `secrets` (the CI client takes none: the OIDC token is its one credential);
//! - a job of [`CALLERS`] for the file is missing, or does not call the CI client;
//! - it is [`HELLO_FILE`] and runs on anything but `workflow_dispatch`, lacks the input `version`
//!   (a required string) or `variant` (a required choice of exactly [`HELLO_VARIANTS`]), or its
//!   caller does not pass `service: hello` and the `hostkeeper_commit` its `uses:` is pinned to,
//!   or a job of it lacks the exact `if:` [`HELLO_IF`] (a dispatch from another branch runs
//!   nothing).

use super::lint::{entries, entry, triggers};
use super::yaml::{Node, Value};

/// The CI client, as a caller's `uses:` names it before the `@`.
pub const SET_DESIRED: &str = "komira-ai/hostkeeper/.github/workflows/set-desired.yml";

/// A job that calls [`SET_DESIRED`]: the workflow file, the job, and its `if:`, exactly.
pub struct Caller {
    pub file: &'static str,
    pub job: &'static str,
    pub condition: &'static str,
}

/// The hello proof run.
pub const HELLO_FILE: &str = "hello-desired.yml";
/// Its job that calls the CI client.
pub const HELLO_JOB: &str = "set-desired";
/// Its `if:`: a dispatch from main only.
pub const HELLO_IF: &str = "${{ github.ref == 'refs/heads/main' }}";
/// The values of its input `variant`, in order: the build that passes, and the one whose
/// `/readyz` answers 503 (docs/hello.md, step 4).
pub const HELLO_VARIANTS: [&str; 2] = ["hello", "fail-ready"];

/// Every job of this repository that calls the CI client.
pub const CALLERS: &[Caller] = &[Caller {
    file: HELLO_FILE,
    job: HELLO_JOB,
    condition: HELLO_IF,
}];

/// The caller entry of the job `job` of `file`, if it is one.
pub fn caller(file: &str, job: &str) -> Option<&'static Caller> {
    CALLERS.iter().find(|c| c.file == file && c.job == job)
}

/// The job-level `uses:` of `job`: the reusable workflow it calls, if any.
pub fn job_uses(job: &Node) -> Option<&str> {
    entry(job, "uses").and_then(Node::as_str)
}

/// Whether the `uses:` value names the CI client, by any owner, repository or ref.
pub fn names_set_desired(uses: &str) -> bool {
    let path = uses.split_once('@').map_or(uses, |(path, _)| path);
    path.rsplit('/').next() == Some("set-desired.yml")
}

/// The jobs of `doc`, by name.
fn jobs(doc: &Node) -> impl Iterator<Item = (&str, &Node)> {
    entry(doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| (k.as_str().unwrap_or_default(), v))
}

/// The `(scope, level)` pairs of a job's `permissions`, sorted; empty when it has none.
fn permissions(job: &Node) -> Vec<(&str, &str)> {
    let mut found: Vec<(&str, &str)> = entry(job, "permissions")
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default(),
                v.as_str().unwrap_or_default(),
            )
        })
        .collect();
    found.sort_unstable();
    found
}

/// One line per rule of this module the workflow `doc` of the file `file` breaks.
pub fn problems(file: &str, doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, job) in jobs(doc) {
        let Some(uses) = job_uses(job).filter(|u| names_set_desired(u)) else {
            continue;
        };
        let Some(c) = caller(file, name) else {
            let listed: Vec<String> = CALLERS
                .iter()
                .map(|c| format!("{}/{}", c.file, c.job))
                .collect();
            problems.push(format!(
                "job {name} calls set-desired.yml, and only {listed:?} may"
            ));
            continue;
        };
        let path = uses.split_once('@').map_or(uses, |(path, _)| path);
        if path != SET_DESIRED {
            problems.push(format!("job {name} calls {path}, not {SET_DESIRED}"));
        }
        if entry(job, "if").and_then(Node::as_str) != Some(c.condition) {
            problems.push(format!("job {name} needs the exact `if: {}`", c.condition));
        }
        if permissions(job) != [("contents", "read"), ("id-token", "write")] {
            problems.push(format!(
                "job {name}: permissions must be exactly contents: read and id-token: write"
            ));
        }
        if entry(job, "secrets").is_some() {
            problems.push(format!("job {name} passes secrets to set-desired.yml"));
        }
    }
    for c in CALLERS.iter().filter(|c| c.file == file) {
        let calls = jobs(doc)
            .find(|(name, _)| *name == c.job)
            .and_then(|(_, job)| job_uses(job))
            .is_some_and(names_set_desired);
        if !calls {
            problems.push(format!(
                "{file}: job {} does not call set-desired.yml",
                c.job
            ));
        }
    }
    if file == HELLO_FILE {
        problems.extend(hello_problems(doc));
    }
    problems
}

/// The text of a scalar entry of `map`, if there is one.
fn text<'a>(map: Option<&'a Node>, key: &str) -> Option<&'a str> {
    map.and_then(|m| entry(m, key)).and_then(Node::as_str)
}

/// What `hello-desired.yml` must be beyond the rules of every caller.
fn hello_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let on = entry(doc, "on");
    if on.map(triggers).unwrap_or_default() != ["workflow_dispatch"] {
        problems.push(format!("{HELLO_FILE}: on must be workflow_dispatch only"));
    }
    for (name, job) in jobs(doc) {
        if entry(job, "if").and_then(Node::as_str) != Some(HELLO_IF) {
            problems.push(format!(
                "{HELLO_FILE}: job {name} needs the exact `if: {HELLO_IF}`"
            ));
        }
    }
    let inputs = on
        .and_then(|o| entry(o, "workflow_dispatch"))
        .and_then(|d| entry(d, "inputs"));
    let input = |name: &str| inputs.and_then(|i| entry(i, name));
    let version = input("version");
    if text(version, "type") != Some("string") || text(version, "required") != Some("true") {
        problems.push(format!(
            "{HELLO_FILE}: input version must be a required string"
        ));
    }
    let variant = input("variant");
    let options: Vec<&str> = match variant.and_then(|v| entry(v, "options")).map(|o| &o.value) {
        Some(Value::Seq(items)) => items.iter().filter_map(Node::as_str).collect(),
        _ => Vec::new(),
    };
    let choice =
        text(variant, "type") == Some("choice") && text(variant, "required") == Some("true");
    if !choice || options != HELLO_VARIANTS {
        problems.push(format!(
            "{HELLO_FILE}: input variant must be a required choice of exactly {HELLO_VARIANTS:?}"
        ));
    }
    let job = jobs(doc)
        .find(|(name, _)| *name == HELLO_JOB)
        .map(|(_, j)| j);
    let with = job.and_then(|j| entry(j, "with"));
    if text(with, "service") != Some("hello") {
        problems.push(format!(
            "{HELLO_FILE}: job {HELLO_JOB} must pass `service: hello`"
        ));
    }
    let pin = job
        .and_then(job_uses)
        .and_then(|u| u.split_once('@'))
        .map(|(_, at)| at);
    if pin.is_none() || text(with, "hostkeeper_commit") != pin {
        problems.push(format!(
            "{HELLO_FILE}: job {HELLO_JOB} must pass the hostkeeper_commit its `uses:` is pinned to"
        ));
    }
    problems
}
