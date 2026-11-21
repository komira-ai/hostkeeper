//! The rules that let `main` merge through GitHub's merge queue: the two workflows whose checks
//! `main` requires ([`FILES`]) run, and report every required check ([`REQUIRED`]), for a merge
//! group exactly as for a pull request. A required check that does not run for a merge group
//! leaves the queue waiting until it times out; a job that runs but is skipped counts as passed,
//! and the queue would merge what nothing tested. So, for each of [`FILES`] ([`problems`]):
//!
//! - its `on` is exactly `pull_request` with no filter, `merge_group` of the type
//!   [`MERGE_GROUP_TYPE`] on the branch [`BRANCH`], and, in ci.yml alone, `push` to [`BRANCH`];
//! - its `concurrency` is exactly the group [`group`] with `cancel-in-progress:` [`CANCEL`]: a
//!   pull request's newer run cancels its older one (the key is the pull request's ref), and every
//!   other run, a merge group's included, has a group of its own (its run id) and is never
//!   cancelled by another;
//! - a job's `if:` is absent or [`ALWAYS`], so no job is skipped for a merge group;
//! - outside `on` and `concurrency`, nothing reads which event started the run (`github.event*`,
//!   `github.ref*`, `github.head_ref`, `github.base_ref`, also as `github['…']`, or the `GITHUB_EVENT*`,
//!   `GITHUB_REF*`, `GITHUB_HEAD_REF` and `GITHUB_BASE_REF` variables) but as an `if:` that starts
//!   [`PUSH_ONLY`] (the cache saves of a push to main): such a step is skipped for a pull request
//!   and for a merge group alike, so a merge group runs every step a pull request runs.
//!
//! Across the workflows ([`checks_problems`]): each check of [`REQUIRED`] is the check of exactly
//! one job of a workflow that runs for pull requests or merge groups, a job of its file; and no
//! workflow but [`FILES`] runs for a merge group. CONTRIBUTING.md's section [`DOC_SECTION`] gives
//! the ruleset and the squash-message settings as JSON; [`doc_problems`] holds its required checks
//! to [`REQUIRED`] and its queue and squash settings to [`QUEUE`] and [`SQUASH`].

use std::collections::BTreeSet;

use serde_json::{Value as Json, json};

use super::ci;
use super::lint::{
    OPENRC_DEVICE_JOB, entries, entry, expressions, runs_on_pull_requests, triggers,
};
use super::pr_device;
use super::yaml::{self, Node, Value};

/// The workflows whose checks `main` requires, held to the queue's rules.
pub const FILES: [&str; 2] = [ci::FILE, pr_device::FILE];

/// The checks `main` requires, each with the workflow whose job reports it.
pub const REQUIRED: [(&str, &str); 4] = [
    (ci::FILE, ci::LINT_JOB),
    (ci::FILE, ci::CHECK),
    (pr_device::FILE, pr_device::PRIVILEGED_JOB),
    (pr_device::FILE, OPENRC_DEVICE_JOB),
];

/// The one type of `merge_group` event: GitHub asks for the group's checks.
pub const MERGE_GROUP_TYPE: &str = "checks_requested";
/// The one branch with a merge queue.
pub const BRANCH: &str = "main";
/// `cancel-in-progress`: only a pull request's runs are cancelled, by its newer run.
pub const CANCEL: &str = "${{ github.event_name == 'pull_request' }}";
/// The key of the concurrency group after the workflow's own prefix: a pull request's ref
/// (`refs/pull/<n>/merge`, one per pull request), or else the run's id, unique to the run.
pub const KEY: &str = "${{ github.event_name == 'pull_request' && github.ref || github.run_id }}";
/// The one `if:` a job may have: it runs whatever the jobs it needs did.
pub const ALWAYS: &str = "${{ always() }}";
/// How an `if:` that reads the event starts: the step runs on a push to main alone.
pub const PUSH_ONLY: &str = "github.event_name == 'push' &&";
/// The integration id of GitHub Actions, whose checks the ruleset requires.
pub const ACTIONS_APP: u64 = 15368;
/// The heading of CONTRIBUTING.md's section on the queue.
pub const DOC_SECTION: &str = "## The merge queue";

/// The concurrency group of `file`: its name without `.yml`, `-`, and [`KEY`].
pub fn group(file: &str) -> String {
    format!("{}-{KEY}", file.trim_end_matches(".yml"))
}

/// The triggers of `file`, in its order, as [`on_problem`] says them.
fn wanted_on(file: &str) -> &'static [&'static str] {
    if file == ci::FILE {
        &["pull_request", "merge_group", "push"]
    } else {
        &["pull_request", "merge_group"]
    }
}

/// What `file` is refused with when its `on` is anything else.
pub fn on_problem(file: &str) -> String {
    let push = if file == ci::FILE {
        format!(", push to [{BRANCH}]")
    } else {
        String::new()
    };
    format!(
        "{file}: on must be exactly pull_request with no filter, merge_group of types [{MERGE_GROUP_TYPE}] on branches [{BRANCH}]{push}"
    )
}

/// What `file` is refused with when its `concurrency` is anything else.
pub fn concurrency_problem(file: &str) -> String {
    format!(
        "{file}: concurrency must be exactly the group {} with cancel-in-progress: {CANCEL}",
        group(file)
    )
}

/// Whether `node` is a trigger with no filter: no value, or an empty mapping.
fn unfiltered(node: &Node) -> bool {
    node.is_empty_map() || (matches!(node.value, Value::Scalar { .. }) && node.as_str().is_none())
}

/// The one value of a filter: a scalar, or a list of exactly one scalar.
fn one(node: &Node) -> Option<&str> {
    match &node.value {
        Value::Seq(items) if items.len() == 1 => items[0].as_str(),
        Value::Seq(_) | Value::Map(_) => None,
        Value::Scalar { .. } => node.as_str(),
    }
}

/// Whether `node` is a mapping of exactly the filters `want`, each with its one value.
fn filtered(node: &Node, want: &[(&str, &str)]) -> bool {
    let keys: BTreeSet<&str> = entries(node)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    keys == want.iter().map(|(k, _)| *k).collect()
        && entries(node).len() == want.len()
        && want
            .iter()
            .all(|(k, v)| entry(node, k).and_then(one) == Some(*v))
}

/// Whether `on` is exactly [`wanted_on`] of `file`, each trigger with its filters.
fn on_holds(file: &str, on: Option<&Node>) -> bool {
    let Some(on) = on else { return false };
    if !matches!(on.value, Value::Map(_)) {
        return false;
    }
    let names = triggers(on);
    let want = wanted_on(file);
    names.len() == want.len()
        && want.iter().all(|w| names.iter().any(|n| n == w))
        && entries(on).iter().all(|(k, v)| match k.as_str() {
            Some("pull_request") => unfiltered(v),
            Some("merge_group") => {
                filtered(v, &[("types", MERGE_GROUP_TYPE), ("branches", BRANCH)])
            }
            Some("push") => filtered(v, &[("branches", BRANCH)]),
            _ => false,
        })
}

/// Whether `concurrency` is exactly [`group`] of `file` with `cancel-in-progress:` [`CANCEL`].
fn concurrency_holds(file: &str, concurrency: Option<&Node>) -> bool {
    let Some(concurrency) = concurrency else {
        return false;
    };
    let keys: Vec<&str> = entries(concurrency)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    keys.len() == 2
        && keys.contains(&"group")
        && keys.contains(&"cancel-in-progress")
        && entry(concurrency, "group").and_then(Node::as_str) == Some(group(file).as_str())
        && entry(concurrency, "cancel-in-progress").and_then(Node::as_str) == Some(CANCEL)
}

/// Whether the expression `e` (between `${{` and `}}`) reads which event started the run.
/// `github['event_name']` and `github [ "ref" ]` read as `github.event_name` and `github.ref`.
fn reads_event(e: &str) -> bool {
    let e: String = e
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\'' | '"'))
        .collect();
    let e = e.to_ascii_lowercase().replace("github[", "github.");
    [
        "github.event",
        "github.ref",
        "github.head_ref",
        "github.base_ref",
    ]
    .iter()
    .any(|r| e.contains(r))
}

/// Whether the expression `e` reads the event only as [`PUSH_ONLY`] in front of the rest of it,
/// which then reads it no more: false for a pull request and a merge group alike.
fn push_only(e: &str) -> bool {
    let squeezed = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    squeezed(e)
        .strip_prefix(&squeezed(PUSH_ONLY))
        .is_some_and(|rest| !reads_event(rest))
}

/// Whether the `if:` `condition`, with or without `${{ }}` around it, is one expression that
/// reads the event only as [`push_only`] does.
fn push_only_condition(condition: &str) -> bool {
    let trimmed = condition.trim();
    let inner = trimmed
        .strip_prefix("${{")
        .and_then(|i| i.strip_suffix("}}"))
        .unwrap_or(trimmed);
    !inner.contains("${{") && !inner.contains("}}") && push_only(inner)
}

/// The event variables of the runner a step's command could read.
const EVENT_VARIABLES: [&str; 4] = [
    "GITHUB_EVENT",
    "GITHUB_REF",
    "GITHUB_HEAD_REF",
    "GITHUB_BASE_REF",
];

/// The values of `node` that read the event, but an `if:` that is [`push_only`], with the
/// first line of each.
fn event_reads(node: &Node, out: &mut Vec<String>) {
    match &node.value {
        Value::Scalar { .. } => {
            let text = node.as_str().unwrap_or_default();
            let read = expressions(text).iter().any(|e| reads_event(e))
                || EVENT_VARIABLES.iter().any(|v| text.contains(v));
            if read {
                out.push(text.lines().next().unwrap_or_default().to_owned());
            }
        }
        Value::Seq(items) => items.iter().for_each(|i| event_reads(i, out)),
        Value::Map(map) => {
            for (k, v) in map {
                event_reads(k, out);
                // An `if:` is an expression whole, with or without `${{ }}` around it.
                let condition = v.as_str().filter(|_| k.as_str() == Some("if"));
                match condition {
                    Some(c) if reads_event(c) && !push_only_condition(c) => {
                        out.push(c.lines().next().unwrap_or_default().to_owned());
                    }
                    Some(_) => {}
                    None => event_reads(v, out),
                }
            }
        }
    }
}

/// What is wrong with `file` (one of [`FILES`]), the document `doc`, for the merge queue.
pub fn problems(file: &str, doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !on_holds(file, entry(doc, "on")) {
        problems.push(on_problem(file));
    }
    if !concurrency_holds(file, entry(doc, "concurrency")) {
        problems.push(concurrency_problem(file));
    }
    for (name, job) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        if entry(job, "if").is_some_and(|c| c.as_str() != Some(ALWAYS)) {
            problems.push(format!(
                "{file}: job {name} has an if: other than {ALWAYS}, and could be skipped for a merge group, which counts as passed"
            ));
        }
    }
    let mut reads = Vec::new();
    for (key, value) in entries(doc) {
        if matches!(key.as_str(), Some("on" | "concurrency")) {
            continue;
        }
        event_reads(key, &mut reads);
        event_reads(value, &mut reads);
    }
    for line in reads {
        problems.push(format!(
            "{file}: `{line}` reads the event; only an if: that starts `{PUSH_ONLY}` may, so a merge group runs what a pull request runs"
        ));
    }
    problems
}

/// The check each job of `doc` reports: its `name`, or else its key.
fn checks(doc: &Node) -> Vec<String> {
    entry(doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .filter_map(|(key, job)| {
            entry(job, "name")
                .and_then(Node::as_str)
                .or(key.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// What is wrong across `workflows` (`(file, text)` of each): a required check that is not the
/// check of exactly one job of its file among the workflows that run for pull requests or merge
/// groups, and a workflow outside [`FILES`] that runs for a merge group.
pub fn checks_problems(workflows: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut reporting = Vec::new();
    for (file, text) in workflows {
        let doc = match yaml::load(text) {
            Ok(doc) => doc,
            Err(e) => {
                problems.push(format!("{file}: {e}"));
                continue;
            }
        };
        let on = entry(&doc, "on").map(triggers).unwrap_or_default();
        if on.iter().any(|t| t == "merge_group") && !FILES.contains(&file.as_str()) {
            problems.push(format!(
                "{file}: runs for a merge group; only {FILES:?} do, held to the queue's rules"
            ));
        }
        if on.iter().any(|t| runs_on_pull_requests(t)) {
            for check in checks(&doc) {
                reporting.push((file.as_str(), check));
            }
        }
    }
    for (file, check) in REQUIRED {
        let jobs: Vec<&str> = reporting
            .iter()
            .filter(|(_, c)| c == check)
            .map(|(f, _)| *f)
            .collect();
        if jobs != [file] {
            problems.push(format!(
                "the required check {check} is reported by jobs of {jobs:?}, not by exactly one job of {file}"
            ));
        }
    }
    problems
}

/// The parameters of the ruleset's `merge_queue` rule.
pub fn queue() -> Json {
    json!({
        "merge_method": "SQUASH",
        "grouping_strategy": "ALLGREEN",
        "max_entries_to_build": 5,
        "min_entries_to_merge": 1,
        "max_entries_to_merge": 5,
        "min_entries_to_merge_wait_minutes": 5,
        "check_response_timeout_minutes": 360
    })
}

/// The repository's squash-message settings, which the queue writes its commits with.
pub fn squash() -> Json {
    json!({
        "squash_merge_commit_title": "PR_TITLE",
        "squash_merge_commit_message": "BLANK"
    })
}

/// Names of [`queue`] and [`squash`] for the messages.
pub const QUEUE: &str = "the merge_queue rule's parameters";
pub const SQUASH: &str = "the squash-message settings";

/// The JSON blocks (```` ```json ````) of [`DOC_SECTION`] of `doc`, in order, each parsed.
fn doc_blocks(doc: &str) -> Result<Vec<Json>, String> {
    let (_, section) = doc
        .split_once(&format!("\n{DOC_SECTION}\n"))
        .ok_or_else(|| format!("no section {DOC_SECTION:?}"))?;
    let section = section.split("\n## ").next().unwrap_or_default();
    let mut blocks = Vec::new();
    let mut rest = section;
    while let Some((_, after)) = rest.split_once("```json\n") {
        let (body, tail) = after
            .split_once("```")
            .ok_or_else(|| "a ```json block that is not closed".to_owned())?;
        blocks.push(serde_json::from_str(body).map_err(|e| format!("a ```json block: {e}"))?);
        rest = tail;
    }
    Ok(blocks)
}

/// The rule of `ruleset` of the type `kind`, if it has exactly one.
fn rule<'a>(ruleset: &'a Json, kind: &str) -> Option<&'a Json> {
    let rules: Vec<&Json> = ruleset["rules"]
        .as_array()
        .map(|r| r.iter().filter(|r| r["type"] == kind).collect())
        .unwrap_or_default();
    match rules[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// What is wrong with CONTRIBUTING.md (`doc`): its section [`DOC_SECTION`] has exactly two JSON
/// blocks, the ruleset for [`BRANCH`] and the squash settings; the ruleset requires exactly the
/// checks of [`REQUIRED`], in order, from GitHub Actions, without requiring a branch up to date,
/// and its one `merge_queue` rule has the parameters [`queue`]; the settings are [`squash`].
pub fn doc_problems(doc: &str) -> Vec<String> {
    let blocks = match doc_blocks(doc) {
        Ok(blocks) => blocks,
        Err(e) => return vec![format!("CONTRIBUTING.md: {e}")],
    };
    let [ruleset, settings] = &blocks[..] else {
        return vec![format!(
            "CONTRIBUTING.md: {DOC_SECTION:?} has {} JSON blocks, not the ruleset and the squash settings",
            blocks.len()
        )];
    };
    let mut problems = Vec::new();
    let refs = json!({"include": [format!("refs/heads/{BRANCH}")], "exclude": []});
    if ruleset["target"] != "branch"
        || ruleset["enforcement"] != "active"
        || ruleset["conditions"]["ref_name"] != refs
    {
        problems.push(format!(
            "CONTRIBUTING.md: the ruleset is not an active branch ruleset of refs/heads/{BRANCH} alone"
        ));
    }
    let required: Vec<Json> = REQUIRED
        .iter()
        .map(|(_, check)| json!({"context": check, "integration_id": ACTIONS_APP}))
        .collect();
    let checks = rule(ruleset, "required_status_checks").map(|r| &r["parameters"]);
    let checks_hold = checks.is_some_and(|p| {
        p["required_status_checks"] == Json::Array(required.clone())
            && p["strict_required_status_checks_policy"] == false
    });
    if !checks_hold {
        let names: Vec<&str> = REQUIRED.iter().map(|(_, c)| *c).collect();
        problems.push(format!(
            "CONTRIBUTING.md: the ruleset must have one required_status_checks rule of exactly {names:?} from GitHub Actions ({ACTIONS_APP}), with strict_required_status_checks_policy false"
        ));
    }
    if rule(ruleset, "merge_queue").map(|r| &r["parameters"]) != Some(&queue()) {
        problems.push(format!(
            "CONTRIBUTING.md: the ruleset must have one merge_queue rule with {QUEUE} {}",
            queue()
        ));
    }
    if *settings != squash() {
        problems.push(format!(
            "CONTRIBUTING.md: {SQUASH} must be exactly {}",
            squash()
        ));
    }
    problems
}
