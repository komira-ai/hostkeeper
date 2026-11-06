//! The rules every workflow of this repository is held to.
//!
//! [`lint`] reads one workflow file and returns one line per problem, none when it is clean.
//! A workflow is refused when:
//! - it lacks a top-level `permissions: {}`, so that jobs start with no token rights;
//! - its `on` names `pull_request_target` (it runs with the base repository's token and
//!   secrets on code from a fork), or any key or value names `pull_request_target` or
//!   `self-hosted`;
//! - a step's (or a job's) `uses:` is not an action pinned to a full 40-character commit SHA
//!   followed on the same line by a version comment (`# v1.2.3`; the value may be quoted, and
//!   the spaces before and after `#` are free), or names a local path (a
//!   local action or reusable workflow is code this lint does not read);
//! - a job has no `runs-on` (but a job of [`callers::CALLERS`] that calls the CI client), or a
//!   runner that is not one of [`HOSTED_RUNNERS`]: a runner group, a custom label (a
//!   self-hosted or a paid larger runner), or an expression other than `${{ matrix.<key> }}`,
//!   whose values are read from the job's own `strategy.matrix` (a list
//!   or an `include` the lint cannot read, such as an expression, is refused);
//! - a job's `permissions` is not a mapping, or grants `write` for a scope that is not in
//!   [`WRITE_ALLOWED`] for that workflow file and job, or grants it under another `if:` than the
//!   one the table gives; or grants `id-token: write` in a job that is in none of that table,
//!   [`ID_TOKEN_ALLOWED`] and [`callers::CALLERS`];
//! - any key `secrets` has the value `inherit`;
//! - the workflow runs for a pull request (`pull_request`, its review events, or
//!   `merge_group`) and a job of it is privileged or macOS, whatever its `if:` says, or what
//!   sits beside `jobs` (`env`, `defaults`, ...) is, so that nothing of the kind runs on a pull
//!   request, as a gate or not.
//!   Privileged: a value of the job names `sudo` as a word (so `cargo xtask sudo-test` and a
//!   cargo runner of `sudo` too), `--privileged` or `--cap-add` (a container's options), or the
//!   job holds a `write` scope. macOS: a value of the job names `macos` or `darwin`, in any case
//!   (a runner, a matrix value, a `--target aarch64-apple-darwin`);
//! - it is not [`PRIVILEGED_WORKFLOW`], it does not run for a pull request, and a job of it, or
//!   what sits beside `jobs`, names root as above (`sudo` as a word, `--privileged`,
//!   `--cap-add`), whatever its trigger: root runs in that one workflow or nowhere. (A pull-request
//!   workflow is refused for the same values by the rule above, with its own message. A write
//!   scope is not root; outside a pull request it is held to [`WRITE_ALLOWED`].)
//! - it is [`PRIVILEGED_WORKFLOW`] and its `on` is not exactly `push` with `branches: [main]` and
//!   nothing else, and `workflow_dispatch` with no inputs (no other trigger, no `tags`, `paths` or
//!   other filter of `push`), or a job of it runs on a runner that is not in its list
//!   ([`privileged_runners`]: [`MACOS_DEVICE_RUNNERS`] for [`MACOS_DEVICE_JOB`], the one macOS
//!   job, and [`PRIVILEGED_RUNNERS`] for every other). Every other rule holds for it as for any
//!   file; [`WRITE_ALLOWED`] has no entry for it, so no job of it holds a write scope.
//!
//! - it is `release.yml` or `artifacts.yml` and breaks a rule of its own
//!   ([`release::release_rules`]).
//!
//! A workflow that tags or publishes is also held to [`release::publish_problems`].
//!
//! `set-desired.yml` is also held to the rules of [`set_desired`], which fix the order of its
//! steps. Every workflow is held to the rules of [`callers`]: which jobs may call it, and how.
//! `self-hosted` in `--deny-self-hosted-runners` names no runner.
//!
//! Keys are matched exactly. What this does not evaluate it refuses: a YAML tag, a second
//! document, an alias to an unfinished node, a document that does not parse.

use super::advisories;
use super::callers;
use super::release::{self, PLAN_IF, RELEASE_IF, SIGN_IF};
use super::set_desired;
use super::yaml::{self, Node, Value};

/// A write permission a job may hold: the workflow file, the job, the scope, and the `if:` of
/// the job, exactly.
pub struct Allowed {
    pub file: &'static str,
    pub job: &'static str,
    pub scope: &'static str,
    pub condition: &'static str,
}

/// The write scopes jobs may hold, each in one job of one file under one exact `if:`.
///
/// - `artifacts.yml` / `attest`: `id-token` and `attestations`, to sign, on a push to main. It
///   is the one job that can sign: the signer identity every consumer checks is that file.
/// - `release.yml` / `publish`: `contents`, to create the release and its tag.
/// - `release.yml` / `plan`: `issues`, to open or update the one issue that tracks a release
///   held by a failed privileged run.
/// - `advisories.yml` / `advisories`: `issues`, to open, rewrite or close the one issue that
///   lists the security advisories against the dependencies, on a run of main.
pub const WRITE_ALLOWED: &[Allowed] = &[
    Allowed {
        file: "artifacts.yml",
        job: "attest",
        scope: "id-token",
        condition: SIGN_IF,
    },
    Allowed {
        file: "artifacts.yml",
        job: "attest",
        scope: "attestations",
        condition: SIGN_IF,
    },
    Allowed {
        file: "release.yml",
        job: "publish",
        scope: "contents",
        condition: RELEASE_IF,
    },
    Allowed {
        file: "release.yml",
        job: "plan",
        scope: "issues",
        condition: PLAN_IF,
    },
    Allowed {
        file: advisories::FILE,
        job: advisories::JOB,
        scope: "issues",
        condition: advisories::ISSUE_IF,
    },
];

/// The jobs that may hold `id-token: write`, by workflow file and job. The scope writes nothing
/// to the repository: it lets the job's steps ask GitHub for an OIDC token. The one job is the
/// CI client's, whose order of steps [`set_desired`] holds.
pub const ID_TOKEN_ALLOWED: &[(&str, &str)] = &[(set_desired::FILE, set_desired::JOB)];

/// The one workflow that may run a job as root (`sudo`, a privileged container): the privileged
/// tests, after a merge to main and by hand, never on a pull request.
pub const PRIVILEGED_WORKFLOW: &str = "privileged.yml";

/// The runners a job of [`PRIVILEGED_WORKFLOW`] may run on, but [`MACOS_DEVICE_JOB`]:
/// GitHub-hosted Ubuntu with systemd as PID 1 and passwordless `sudo`. No macOS runner, and no
/// `-latest` label that moves under us.
pub const PRIVILEGED_RUNNERS: &[&str] = &["ubuntu-24.04"];

/// The one job of [`PRIVILEGED_WORKFLOW`] that runs on macOS: the device run of the launchd
/// adapters, the process probe and the package check (`crates/hostkeeper-privileged/tests/
/// device_mac.rs`).
pub const MACOS_DEVICE_JOB: &str = "macos-device";

/// The runners [`MACOS_DEVICE_JOB`] may run on: GitHub's standard hosted arm64 macOS image of a
/// named version (passwordless `sudo`, launchd), free for a public repository. No `-latest`
/// label, and no larger or self-hosted runner.
pub const MACOS_DEVICE_RUNNERS: &[&str] = &["macos-15"];

/// The runners the job `name` of [`PRIVILEGED_WORKFLOW`] may run on.
pub fn privileged_runners(name: &str) -> &'static [&'static str] {
    match name {
        MACOS_DEVICE_JOB => MACOS_DEVICE_RUNNERS,
        _ => PRIVILEGED_RUNNERS,
    }
}

/// What a [`PRIVILEGED_WORKFLOW`] whose `on` is anything else is refused with.
pub const PRIVILEGED_ON: &str =
    "privileged.yml: on must be push to main and workflow_dispatch only";

/// The one flag that names `self-hosted` to refuse it: `gh attestation verify`'s.
pub const DENY_SELF_HOSTED: &str = "--deny-self-hosted-runners";

/// The runner labels of GitHub's standard hosted runners, free for a public repository. Any
/// other label is a self-hosted runner or a paid larger runner.
pub const HOSTED_RUNNERS: &[&str] = &[
    "ubuntu-latest",
    "ubuntu-24.04",
    "ubuntu-22.04",
    "ubuntu-24.04-arm",
    "ubuntu-22.04-arm",
    "macos-latest",
    "macos-15",
    "macos-14",
];

pub fn entry<'a>(map: &'a Node, key: &str) -> Option<&'a Node> {
    entries(map)
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

pub fn entries(map: &Node) -> &[(Node, Node)] {
    match &map.value {
        Value::Map(entries) => entries,
        _ => &[],
    }
}

/// The names of the triggers `on` lists, in any of the three shapes GitHub reads.
pub fn triggers(on: &Node) -> Vec<String> {
    match &on.value {
        Value::Scalar { .. } => on.as_str().map(str::to_owned).into_iter().collect(),
        Value::Seq(items) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        Value::Map(entries) => entries
            .iter()
            .filter_map(|(k, _)| k.as_str().map(str::to_owned))
            .collect(),
    }
}

/// Every scalar of the tree, keys and values.
pub fn scalars<'a>(node: &'a Node, out: &mut Vec<&'a str>) {
    match &node.value {
        Value::Scalar { .. } => out.extend(node.as_str()),
        Value::Seq(items) => items.iter().for_each(|i| scalars(i, out)),
        Value::Map(entries) => {
            for (k, v) in entries {
                scalars(k, out);
                scalars(v, out);
            }
        }
    }
}

/// Every `uses:` value of the tree with its line.
fn uses<'a>(node: &'a Node, out: &mut Vec<(usize, &'a str)>) {
    match &node.value {
        Value::Scalar { .. } => {}
        Value::Seq(items) => items.iter().for_each(|i| uses(i, out)),
        Value::Map(entries) => {
            for (k, v) in entries {
                match (k.as_str(), v.as_str()) {
                    (Some("uses"), Some(value)) => out.push((v.line(), value)),
                    _ => uses(v, out),
                }
            }
        }
    }
}

/// What is wrong with the `uses:` value `value` on `line` of `text`, if anything.
fn pin_problem(text: &str, line: usize, value: &str) -> Option<String> {
    if value.starts_with('.') {
        return Some(format!("line {line}: `uses: {value}` names a local path"));
    }
    let Some((_, rest)) = value.split_once('@') else {
        return Some(format!(
            "line {line}: `uses: {value}` is not pinned (owner/repo@<40 hex digits>)"
        ));
    };
    let hex = rest.len() == 40 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !hex {
        return Some(format!(
            "line {line}: `uses: {value}` is not pinned to a 40-digit lowercase commit SHA"
        ));
    }
    // The comment after the value on the line the value is written on (for an alias, the line
    // of its anchor): past a closing quote, one or more spaces or tabs, `#`, then `v<digit>`.
    let source = text.lines().nth(line - 1).unwrap_or_default();
    let after = source
        .find(value)
        .map(|at| &source[at + value.len()..])
        .unwrap_or_default();
    let after = after.strip_prefix(['\'', '"']).unwrap_or(after);
    let spaced = after.trim_start_matches([' ', '\t']);
    let comment = (spaced.len() < after.len())
        .then(|| spaced.strip_prefix('#'))
        .flatten()
        .map(|c| c.trim_start_matches([' ', '\t']));
    let versioned = comment.is_some_and(|c| {
        c.strip_prefix('v')
            .is_some_and(|v| v.starts_with(|ch: char| ch.is_ascii_digit()))
    });
    if versioned {
        None
    } else {
        Some(format!(
            "line {line}: `uses: {value}` has no version comment (`# v1.2.3`) on its line"
        ))
    }
}

/// One line per problem in the workflow `text` of the file named `file` (`ci.yml`).
pub fn lint(file: &str, text: &str) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    if !matches!(doc.value, Value::Map(_)) {
        return vec!["the document is not a mapping".to_owned()];
    }
    let mut problems = Vec::new();
    match entry(&doc, "permissions") {
        Some(p) if p.is_empty_map() => {}
        _ => problems.push("no top-level `permissions: {}`".to_owned()),
    }
    let on = entry(&doc, "on").map(triggers).unwrap_or_default();
    if on.iter().any(|t| t == "pull_request_target") {
        problems.push("`on` names pull_request_target".to_owned());
    }
    let privileged_file = file == PRIVILEGED_WORKFLOW;
    if privileged_file && !push_to_main_and_dispatch_only(entry(&doc, "on")) {
        problems.push(PRIVILEGED_ON.to_owned());
    }
    let mut all = Vec::new();
    scalars(&doc, &mut all);
    for word in ["self-hosted", "pull_request_target"] {
        if all
            .iter()
            .any(|s| s.replace(DENY_SELF_HOSTED, "").contains(word))
        {
            problems.push(format!("the document names {word}"));
        }
    }
    problems.extend(inherited_secrets(&doc));
    let mut pinned = Vec::new();
    uses(&doc, &mut pinned);
    for (line, value) in pinned {
        problems.extend(pin_problem(text, line, value));
    }
    let on_pull_request = on.iter().any(|t| runs_on_pull_requests(t));
    // Root anywhere but in the privileged workflow; a pull-request workflow is refused for it by
    // the pull-request rule.
    let root_refused = !privileged_file && !on_pull_request;
    // What sits beside `jobs` (`env`, `defaults`, ...) applies to every job.
    let mut beside_jobs = Vec::new();
    for (k, v) in entries(&doc)
        .iter()
        .filter(|(k, _)| k.as_str() != Some("jobs"))
    {
        scalars(k, &mut beside_jobs);
        scalars(v, &mut beside_jobs);
    }
    if on_pull_request {
        problems.extend(word_problems("workflow", &beside_jobs));
    }
    if root_refused {
        problems.extend(root_problems("workflow", &beside_jobs));
    }
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        problems.extend(permission_problems(file, name, job));
        // A job of `callers::CALLERS` that calls the CI client runs on that workflow's runner,
        // which its own rules hold; every other job that calls a reusable workflow is refused.
        let calls_client = callers::caller(file, name).is_some()
            && callers::job_uses(job).is_some_and(callers::names_set_desired);
        if !calls_client {
            problems.extend(runner_problems(name, job));
        }
        if on_pull_request {
            problems.extend(pull_request_problems(name, job));
        }
        if root_refused {
            let mut values = Vec::new();
            scalars(job, &mut values);
            problems.extend(root_problems(&format!("job {name}"), &values));
        }
        if privileged_file {
            problems.extend(privileged_runner_problems(name, job));
        }
    }
    problems.extend(release::release_rules(file, text));
    if file == set_desired::FILE {
        problems.extend(set_desired::problems(&doc));
    }
    problems.extend(callers::problems(file, &doc));
    problems
}

/// Every `secrets: inherit` of the tree, by line: it hands every secret of the caller to the
/// workflow it calls, whatever that workflow needs.
fn inherited_secrets(node: &Node) -> Vec<String> {
    match &node.value {
        Value::Scalar { .. } => Vec::new(),
        Value::Seq(items) => items.iter().flat_map(inherited_secrets).collect(),
        Value::Map(entries) => entries
            .iter()
            .flat_map(|(k, v)| match (k.as_str(), v.as_str()) {
                (Some("secrets"), Some("inherit")) => vec![format!(
                    "line {}: `secrets: inherit` hands every secret of the caller on",
                    v.line()
                )],
                _ => inherited_secrets(v),
            })
            .collect(),
    }
}

fn permission_problems(file: &str, name: &str, job: &Node) -> Vec<String> {
    let Some(permissions) = entry(job, "permissions") else {
        return Vec::new();
    };
    if !matches!(permissions.value, Value::Map(_)) {
        return vec![format!(
            "job {name}: `permissions` is not a mapping of scopes"
        )];
    }
    let condition = entry(job, "if").and_then(Node::as_str);
    let mut problems = Vec::new();
    for (scope, level) in entries(permissions) {
        let (scope, level) = (scope.as_str().unwrap_or_default(), level.as_str());
        if level == Some("read") || level == Some("none") {
            continue;
        }
        let id_token = scope == "id-token"
            && level == Some("write")
            && (ID_TOKEN_ALLOWED.contains(&(file, name)) || callers::caller(file, name).is_some());
        if id_token {
            continue;
        }
        let allowed = WRITE_ALLOWED
            .iter()
            .find(|a| a.file == file && a.job == name && a.scope == scope);
        match (level, allowed) {
            (Some("write"), Some(a)) if condition == Some(a.condition) => {}
            (Some("write"), Some(a)) => problems.push(format!(
                "job {name}: `{scope}: write` needs the exact `if: {}`",
                a.condition
            )),
            _ => problems.push(format!(
                "job {name}: `{scope}: {}` is not allowed in {file}",
                level.unwrap_or("(none)")
            )),
        }
    }
    problems
}

/// The runner labels of `job`, or why they cannot be read.
fn runner_labels<'a>(name: &str, job: &'a Node) -> Result<Vec<&'a str>, String> {
    let Some(runs_on) = entry(job, "runs-on") else {
        return Err(format!(
            "job {name}: no `runs-on` (a job that calls a reusable workflow is not evaluated)"
        ));
    };
    let scalar = |node: &'a Node| {
        node.as_str()
            .ok_or_else(|| format!("job {name}: a `runs-on` label is not text"))
    };
    let label = match &runs_on.value {
        Value::Scalar { .. } => scalar(runs_on)?,
        Value::Seq(list) => return list.iter().map(scalar).collect(),
        Value::Map(_) => {
            return Err(format!(
                "job {name}: `runs-on` names a runner group, not a GitHub-hosted runner"
            ));
        }
    };
    if !label.contains("${{") {
        return Ok(vec![label]);
    }
    let key = label
        .strip_prefix("${{ matrix.")
        .and_then(|rest| rest.strip_suffix(" }}"))
        .filter(|key| !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .ok_or_else(|| {
            format!("job {name}: `runs-on: {label}` is an expression other than `${{{{ matrix.<key> }}}}`")
        })?;
    let matrix = entry(job, "strategy").and_then(|s| entry(s, "matrix"));
    let listed = match matrix.and_then(|m| entry(m, key)) {
        None => &[][..],
        Some(Node {
            value: Value::Seq(list),
            ..
        }) => list,
        Some(_) => {
            return Err(format!(
                "job {name}: `strategy.matrix.{key}` is not a list of runner labels"
            ));
        }
    };
    // An `include` the lint cannot read (an expression for the list, or for one entry) could
    // add any runner, so it is refused rather than read as empty.
    let include = match matrix.and_then(|m| entry(m, "include")) {
        None => &[][..],
        Some(Node {
            value: Value::Seq(list),
            ..
        }) => list,
        Some(_) => {
            return Err(format!(
                "job {name}: `strategy.matrix.include` is not a list the lint can read"
            ));
        }
    };
    if include.iter().any(|i| !matches!(i.value, Value::Map(_))) {
        return Err(format!(
            "job {name}: an entry of `strategy.matrix.include` is not a mapping the lint can read"
        ));
    }
    let included = include.iter().filter_map(|i| entry(i, key));
    let values: Vec<&Node> = listed.iter().chain(included).collect();
    if values.is_empty() {
        return Err(format!(
            "job {name}: `runs-on: {label}` has no values in the job's `strategy.matrix`"
        ));
    }
    values.into_iter().map(scalar).collect()
}

fn runner_problems(name: &str, job: &Node) -> Vec<String> {
    match runner_labels(name, job) {
        Err(e) => vec![e],
        Ok(labels) => labels
            .into_iter()
            .filter(|l| !HOSTED_RUNNERS.contains(l))
            .map(|l| format!("job {name}: runner `{l}` is not a GitHub-hosted runner"))
            .collect(),
    }
}

/// Whether `text` holds `word` with no letter, digit or `_` either side of it.
pub fn has_word(text: &str, word: &str) -> bool {
    let inner = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(inner) && !after.is_some_and(inner)
    })
}

/// The first line of `values` for which `found` holds.
fn first_line<'a>(values: &[&'a str], found: impl Fn(&str) -> bool) -> Option<&'a str> {
    values
        .iter()
        .flat_map(|v| v.lines())
        .find(|line| found(line))
}

/// Whether the trigger `name` runs the workflow for a pull request: `pull_request` and its
/// review events, and the merge queue (`pull_request_target` is refused on its own).
pub fn runs_on_pull_requests(name: &str) -> bool {
    name.starts_with("pull_request") || name == "merge_group"
}

/// What a job of a workflow that runs on pull requests may not be: privileged or macOS.
fn pull_request_problems(name: &str, job: &Node) -> Vec<String> {
    let mut values = Vec::new();
    scalars(job, &mut values);
    let who = format!("job {name}");
    let mut problems = word_problems(&who, &values);
    let writes = entry(job, "permissions")
        .map_or(&[][..], entries)
        .iter()
        .find(|(_, level)| level.as_str() == Some("write"));
    if let Some((scope, _)) = writes {
        problems.push(format!(
            "{who}: privileged on pull_request (`{}: write`)",
            scope.as_str().unwrap_or_default()
        ));
    }
    problems
}

/// The first line of `values` that runs something as root: `sudo` as a word, a privileged
/// container (`--privileged`) or one given a capability (`--cap-add`).
pub fn root_line<'a>(values: &[&'a str]) -> Option<&'a str> {
    first_line(values, |line| {
        has_word(line, "sudo") || line.contains("--privileged") || line.contains("--cap-add")
    })
}

/// Root among `values`, the values of `who` (a job or the workflow) of a workflow that is not
/// [`PRIVILEGED_WORKFLOW`].
fn root_problems(who: &str, values: &[&str]) -> Vec<String> {
    root_line(values)
        .map(|line| format!("{who}: privileged outside {PRIVILEGED_WORKFLOW} (`{line}`)"))
        .into_iter()
        .collect()
}

/// Whether `on` is exactly `push` to `branches: [main]` and a `workflow_dispatch` with no inputs,
/// as a mapping: no other trigger, no other branch, no `tags`, `paths` or other filter.
fn push_to_main_and_dispatch_only(on: Option<&Node>) -> bool {
    let Some(Value::Map(triggers)) = on.map(|on| &on.value) else {
        return false;
    };
    let push_to_main = |push: &Node| match entries(push) {
        [
            (
                key,
                Node {
                    value: Value::Seq(branches),
                    ..
                },
            ),
        ] => {
            key.as_str() == Some("branches")
                && matches!(&branches[..], [b] if b.as_str() == Some("main"))
        }
        _ => false,
    };
    let no_inputs = |dispatch: &Node| match &dispatch.value {
        Value::Scalar { .. } => dispatch.as_str().is_none(),
        Value::Map(entries) => entries.is_empty(),
        Value::Seq(_) => false,
    };
    let has = |name: &str, holds: &dyn Fn(&Node) -> bool| {
        triggers
            .iter()
            .any(|(k, v)| k.as_str() == Some(name) && holds(v))
    };
    triggers.len() == 2 && has("push", &push_to_main) && has("workflow_dispatch", &no_inputs)
}

/// A job of [`PRIVILEGED_WORKFLOW`] on a runner that is not in its list ([`privileged_runners`]).
/// Labels that cannot be read are refused by [`runner_problems`] already.
fn privileged_runner_problems(name: &str, job: &Node) -> Vec<String> {
    let listed = privileged_runners(name);
    match runner_labels(name, job) {
        Ok(labels) if labels.iter().any(|l| !listed.contains(l)) => vec![format!(
            "job {name}: privileged job on a runner outside the privileged list"
        )],
        Ok(_) | Err(_) => Vec::new(),
    }
}

/// The privileged and macOS words among `values`, the values of `who` (a job or the workflow).
fn word_problems(who: &str, values: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(line) = root_line(values) {
        problems.push(format!("{who}: privileged on pull_request (`{line}`)"));
    }
    let macos = first_line(values, |line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("macos") || lower.contains("darwin")
    });
    if let Some(line) = macos {
        problems.push(format!("{who}: macOS on pull_request (`{line}`)"));
    }
    problems
}
