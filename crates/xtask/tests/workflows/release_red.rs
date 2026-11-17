//! The rules `release-red.yml`, the alert of a red run of a suite that holds no release
//! (`cargo xtask release red`), is held to beyond those of every workflow, on the real file and
//! on [`SMALL`], a workflow that keeps every rule, each case adding one defect to it and reading
//! the exact problems.
//!
//! [`problems`] refuses the workflow when:
//! - its `on` is not exactly a `workflow_run` of [`WATCHED`], `types: [completed]`, `branches:
//!   [main]`: a workflow it does not watch would never be reported, and a pull-request trigger
//!   would let a branch's code hold the issue write;
//! - its top-level `permissions` holds any scope, or it has a concurrency group (a group replaces
//!   a waiting run, and that run's red would never be reported);
//! - its jobs are not exactly [`JOB`], or [`JOB`] is not under exactly [`RED_IF`], or its
//!   `permissions` are not exactly `actions: read`, `contents: read` and `issues: write`;
//! - its checkout is not of the whole history (a success closes the issue only for a commit
//!   after the red one, which the verb reads from git) or keeps the token;
//! - no step runs [`VERB`], or a `run:` holds more than one line: the logic is the verb's, in
//!   Rust.

use super::lint::{entries, entry, scalars, triggers};
use super::yaml::{self, Node, Value};

/// The file these rules are for, in `.github/workflows`.
pub const FILE: &str = "release-red.yml";
/// Its one job.
pub const JOB: &str = "red";
/// The workflows whose runs it reports on, sorted: the release (the jobs after its deploy) and
/// the privileged tests, the suites no release waits for.
pub const WATCHED: [&str; 2] = ["privileged", "release"];
/// The `if:` of the job: a run of main of this repository.
pub const RED_IF: &str = "${{ github.ref == 'refs/heads/main' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository }}";
/// The command the job runs.
pub const VERB: &str = "cargo xtask release red --repo \"$GITHUB_REPOSITORY\" --workflow \"$WORKFLOW\" --run-id \"$RUN_ID\" --commit \"$COMMIT\"";

/// The text of a scalar entry of `map`, if there is one.
fn text<'a>(map: &'a Node, key: &str) -> Option<&'a str> {
    entry(map, key).and_then(Node::as_str)
}

/// The scalars of the sequence at `key` of `map`, sorted.
fn list(map: Option<&Node>, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(node) = map.and_then(|m| entry(m, key)) {
        scalars(node, &mut out);
    }
    let mut out: Vec<String> = out.into_iter().map(str::to_owned).collect();
    out.sort();
    out
}

/// The `(scope, level)` pairs of a `permissions` mapping, sorted.
fn scopes(permissions: Option<&Node>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = permissions
        .map(entries)
        .unwrap_or_default()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default().to_owned(),
                v.as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    out.sort();
    out
}

/// What is wrong with `on`.
fn on_problems(on: Option<&Node>) -> Vec<String> {
    let Some(on) = on else {
        return vec!["no `on`".to_owned()];
    };
    let mut problems = Vec::new();
    if triggers(on) != ["workflow_run"] {
        problems.push(format!(
            "on must be workflow_run only, not {:?}",
            triggers(on)
        ));
    }
    let run = entry(on, "workflow_run");
    if list(run, "workflows") != WATCHED {
        problems.push(format!(
            "workflow_run.workflows must be {WATCHED:?}, not {:?}",
            list(run, "workflows")
        ));
    }
    if list(run, "types") != ["completed"] || list(run, "branches") != ["main"] {
        problems
            .push("workflow_run must be `types: [completed]` and `branches: [main]`".to_owned());
    }
    problems
}

/// The steps of `job`.
fn steps(job: &Node) -> &[Node] {
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(items)) => items,
        _ => &[],
    }
}

/// One line per rule of the module docs that `workflow` breaks.
pub fn problems(workflow: &str) -> Vec<String> {
    let doc = match yaml::load(workflow) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let mut problems = on_problems(entry(&doc, "on"));
    if !scopes(entry(&doc, "permissions")).is_empty() {
        problems.push("the top-level `permissions` holds a scope".to_owned());
    }
    if entry(&doc, "concurrency").is_some() {
        problems.push("a concurrency group would replace a waiting run, unreported".to_owned());
    }
    let jobs = entry(&doc, "jobs").map(entries).unwrap_or_default();
    let names: Vec<&str> = jobs.iter().filter_map(|(k, _)| k.as_str()).collect();
    if names != [JOB] {
        problems.push(format!("the jobs must be exactly {JOB}, not {names:?}"));
    }
    let Some((_, job)) = jobs.iter().find(|(k, _)| k.as_str() == Some(JOB)) else {
        return problems;
    };
    if text(job, "if") != Some(RED_IF) {
        problems.push(format!("job {JOB}: needs the exact `if: {RED_IF}`"));
    }
    let want = [
        ("actions".to_owned(), "read".to_owned()),
        ("contents".to_owned(), "read".to_owned()),
        ("issues".to_owned(), "write".to_owned()),
    ];
    if scopes(entry(job, "permissions")) != want {
        problems.push(format!(
            "job {JOB}: permissions must be exactly actions: read, contents: read and issues: write"
        ));
    }
    let checkout = steps(job)
        .iter()
        .find(|s| text(s, "uses").is_some_and(|u| u.starts_with("actions/checkout@")));
    let with = checkout.and_then(|s| entry(s, "with"));
    if with.and_then(|w| text(w, "fetch-depth")) != Some("0")
        || with.and_then(|w| text(w, "persist-credentials")) != Some("false")
    {
        problems.push(format!(
            "job {JOB}: its checkout must be of the whole history (`fetch-depth: 0`) without the token (`persist-credentials: false`)"
        ));
    }
    let runs: Vec<&str> = steps(job).iter().filter_map(|s| text(s, "run")).collect();
    for run in &runs {
        if run.trim_end().lines().count() > 1 {
            problems.push(format!("a `run:` holds more than one line: {run:?}"));
        }
    }
    if !runs.contains(&VERB) {
        problems.push(format!("no step runs `{VERB}`"));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::super::{lint, with_in, workflows_dir};
    use super::*;

    /// A workflow that keeps every rule.
    const SMALL: &str = "\
name: release-red
on:
  workflow_run:
    workflows: [release, privileged]
    types: [completed]
    branches: [main]
permissions: {}
jobs:
  red:
    if: ${{ github.ref == 'refs/heads/main' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository }}
    runs-on: ubuntu-24.04
    permissions:
      actions: read
      contents: read
      issues: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - run: cargo xtask release red --repo \"$GITHUB_REPOSITORY\" --workflow \"$WORKFLOW\" --run-id \"$RUN_ID\" --commit \"$COMMIT\"
";

    fn assert_problems(text: &str, want: &[&str]) {
        assert_eq!(problems(text), want, "in:\n{text}");
    }

    #[test]
    fn the_real_workflow_and_the_small_one_keep_every_rule() {
        // Catches: release-red.yml missing, out of its shape, or failing a rule every workflow
        // keeps (its write scope outside the table, an unpinned action).
        let real = std::fs::read_to_string(workflows_dir().join(FILE)).expect("release-red.yml");
        assert_eq!(problems(&real), Vec::<String>::new());
        assert_eq!(lint::lint(FILE, &real), Vec::<String>::new());
        assert_problems(SMALL, &[]);
    }

    #[test]
    fn a_trigger_other_than_the_completion_of_the_watched_runs_on_main_is_refused() {
        // Catches: a watched workflow dropped (its red never reported), a pull-request trigger,
        // or runs of another branch or of a run not yet completed.
        let workflows = "    workflows: [release, privileged]\n";
        assert_problems(
            &with_in(SMALL, workflows, "    workflows: [release]\n"),
            &["workflow_run.workflows must be [\"privileged\", \"release\"], not [\"release\"]"],
        );
        assert_problems(
            &with_in(SMALL, "on:\n", "on:\n  pull_request:\n"),
            &["on must be workflow_run only, not [\"pull_request\", \"workflow_run\"]"],
        );
        let shape = "workflow_run must be `types: [completed]` and `branches: [main]`";
        assert_problems(
            &with_in(
                SMALL,
                "    branches: [main]\n",
                "    branches: [main, dev]\n",
            ),
            &[shape],
        );
        assert_problems(
            &with_in(
                SMALL,
                "    types: [completed]\n",
                "    types: [requested]\n",
            ),
            &[shape],
        );
        let on = "on:\n  workflow_run:\n    workflows: [release, privileged]\n    types: [completed]\n    branches: [main]\n";
        assert_problems(&with_in(SMALL, on, ""), &["no `on`"]);
    }

    #[test]
    fn a_write_scope_a_group_another_job_or_another_condition_is_refused() {
        // Catches: the issue write held at the top or by a second job, a concurrency group that
        // drops a waiting run's report, the job run for another branch, or a scope it needs lost.
        assert_problems(
            &with_in(
                SMALL,
                "permissions: {}\n",
                "permissions:\n  issues: write\n",
            ),
            &["the top-level `permissions` holds a scope"],
        );
        assert_problems(
            &with_in(
                SMALL,
                "permissions: {}\n",
                "permissions: {}\nconcurrency:\n  group: red\n",
            ),
            &["a concurrency group would replace a waiting run, unreported"],
        );
        assert_problems(
            &format!("{SMALL}  other:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n"),
            &["the jobs must be exactly red, not [\"red\", \"other\"]"],
        );
        assert_problems(
            &with_in(SMALL, "  red:\n", "  alert:\n"),
            &["the jobs must be exactly red, not [\"alert\"]"],
        );
        assert_problems(
            &with_in(SMALL, "github.ref == 'refs/heads/main' && ", ""),
            &[&format!("job red: needs the exact `if: {RED_IF}`")],
        );
        let wrong =
            "job red: permissions must be exactly actions: read, contents: read and issues: write";
        assert_problems(&with_in(SMALL, "      actions: read\n", ""), &[wrong]);
        assert_problems(
            &with_in(SMALL, "      contents: read\n", "      contents: write\n"),
            &[wrong],
        );
    }

    #[test]
    fn a_shallow_checkout_one_that_keeps_the_token_or_logic_off_the_verb_is_refused() {
        // Catches: a checkout without the history the verb reads to close the issue, one that
        // leaves the token in the clone, the verb not run, or shell logic beside it.
        let checkout = "checkout must be of the whole history (`fetch-depth: 0`) without the token (`persist-credentials: false`)";
        let shape = format!("job red: its {checkout}");
        assert_problems(&with_in(SMALL, "          fetch-depth: 0\n", ""), &[&shape]);
        assert_problems(
            &with_in(
                SMALL,
                "          persist-credentials: false\n",
                "          persist-credentials: true\n",
            ),
            &[&shape],
        );
        assert_problems(
            &with_in(SMALL, " --commit \"$COMMIT\"\n", "\n"),
            &[&format!("no step runs `{VERB}`")],
        );
        assert_problems(
            &with_in(
                SMALL,
                "      - run: cargo xtask",
                "      - run: |\n          echo red\n          cargo xtask",
            ),
            &[
                "a `run:` holds more than one line: \"echo red\\ncargo xtask release red --repo \\\"$GITHUB_REPOSITORY\\\" --workflow \\\"$WORKFLOW\\\" --run-id \\\"$RUN_ID\\\" --commit \\\"$COMMIT\\\"\\n\"",
                &format!("no step runs `{VERB}`"),
            ],
        );
    }
}
