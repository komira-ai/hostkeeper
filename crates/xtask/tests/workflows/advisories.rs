//! The rules `advisories.yml`, the advisory monitor (`cargo xtask advisories`), is held to
//! beyond those of every workflow, on the real file and on [`MONITOR`], a small workflow that
//! keeps every rule, each case adding one defect to it and reading the exact problems.
//!
//! The rules keep advisories out of the pull-request gate (AGENTS.md B9, R6) and the issue
//! write on main. [`problems`] refuses the workflow when:
//! - its `on` is not exactly `schedule` (with at least one `cron`), `push` with `branches:
//!   [main]` and no other filter, and `workflow_dispatch`: no `pull_request` or other trigger;
//! - its top-level `permissions` holds any scope (every workflow's rule is `{}`);
//! - a job holds `issues: write` (or any write scope) and is not [`JOB`] under exactly
//!   [`ISSUE_IF`], or [`JOB`]'s `permissions` is not exactly `contents: read` and `issues:
//!   write`;
//! - [`JOB`] does not install cargo-deny with `cargo xtask install-tool --sha256
//!   "$DENY_SHA256"`, or its `DENY_VERSION` and `DENY_SHA256` are not the ones `ci.yml`'s `lint`
//!   job pins, or a step runs `cargo install`;
//! - no step runs [`VERB`], or a `run:` holds more than one line: the logic is the verb's, in
//!   Rust.
//!
//! And [`ci_problems`] refuses a `ci.yml` that names the advisories anywhere it runs (a job,
//! a `needs`, a step, the list of `cargo deny check`): the pull-request gate stays as it is.

use super::lint::{entries, entry, has_word, scalars, triggers};
use super::yaml::{self, Node};
use super::{with_in, workflows_dir};

/// The file these rules are for, in `.github/workflows`.
pub const FILE: &str = "advisories.yml";
/// Its one job.
pub const JOB: &str = "advisories";
/// The `if:` of the job that writes the issue: a run of main.
pub const ISSUE_IF: &str = "${{ github.ref == 'refs/heads/main' }}";
/// The command the job runs.
pub const VERB: &str = "cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"";
/// The install of cargo-deny's download, checked against its pinned SHA-256.
pub const INSTALL: &str = "cargo xtask install-tool --download \"$RUNNER_TEMP/cargo-deny.tar.gz\" --sha256 \"$DENY_SHA256\"";
/// The variables of the cargo-deny pin, which must equal ci.yml's.
pub const PIN: [&str; 2] = ["DENY_VERSION", "DENY_SHA256"];

/// The text of a scalar entry of `map`, if there is one.
fn text<'a>(map: &'a Node, key: &str) -> Option<&'a str> {
    entry(map, key).and_then(Node::as_str)
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

/// The `env` value `key` of the job `job` of the workflow `doc`.
fn job_env<'a>(doc: &'a Node, job: &str, key: &str) -> Option<&'a str> {
    entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, job))
        .and_then(|j| entry(j, "env"))
        .and_then(|env| text(env, key))
}

/// Whether `on` is exactly the schedule, the push to main and the dispatch.
fn on_problems(on: Option<&Node>) -> Vec<String> {
    let Some(on) = on else {
        return vec!["no `on`".to_owned()];
    };
    let mut problems = Vec::new();
    let mut names = triggers(on);
    names.sort();
    if names != ["push", "schedule", "workflow_dispatch"] {
        problems.push(format!(
            "on must be schedule, push and workflow_dispatch only, not {names:?}"
        ));
    }
    let crons = entry(on, "schedule")
        .and_then(|s| match &s.value {
            yaml::Value::Seq(items) => {
                Some(items.iter().filter(|i| text(i, "cron").is_some()).count())
            }
            _ => None,
        })
        .unwrap_or_default();
    if names.iter().any(|n| n == "schedule") && crons == 0 {
        problems.push("schedule has no `cron`".to_owned());
    }
    if names.iter().any(|n| n == "push") {
        let push = entry(on, "push");
        let keys: Vec<&str> = push
            .map(entries)
            .unwrap_or_default()
            .iter()
            .filter_map(|(k, _)| k.as_str())
            .collect();
        let mut branches = Vec::new();
        if let Some(b) = push.and_then(|p| entry(p, "branches")) {
            scalars(b, &mut branches);
        }
        if keys != ["branches"] || branches != ["main"] {
            problems.push("push must be `branches: [main]` and nothing else".to_owned());
        }
    }
    problems
}

/// One line per rule of the module docs that `workflow` breaks; `ci` is the text of `ci.yml`.
pub fn problems(workflow: &str, ci: &str) -> Vec<String> {
    let doc = match yaml::load(workflow) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let mut problems = on_problems(entry(&doc, "on"));
    if !scopes(entry(&doc, "permissions")).is_empty() {
        problems.push("the top-level `permissions` holds a scope".to_owned());
    }
    let jobs = entry(&doc, "jobs").map(entries).unwrap_or_default();
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        let held = scopes(entry(job, "permissions"));
        let writes = held.iter().any(|(_, level)| level == "write");
        if name != JOB {
            if writes {
                problems.push(format!("job {name}: holds a write scope; only {JOB} may"));
            }
            continue;
        }
        let want = [
            ("contents".to_owned(), "read".to_owned()),
            ("issues".to_owned(), "write".to_owned()),
        ];
        if held != want {
            problems.push(format!(
                "job {name}: permissions must be exactly contents: read and issues: write"
            ));
        }
        if text(job, "if") != Some(ISSUE_IF) {
            problems.push(format!("job {name}: needs the exact `if: {ISSUE_IF}`"));
        }
    }
    if !jobs.iter().any(|(k, _)| k.as_str() == Some(JOB)) {
        problems.push(format!("no job {JOB}"));
    }
    let runs: Vec<&str> = jobs
        .iter()
        .filter_map(|(_, job)| entry(job, "steps"))
        .flat_map(|steps| match &steps.value {
            yaml::Value::Seq(items) => items.iter().filter_map(|s| text(s, "run")).collect(),
            _ => Vec::new(),
        })
        .collect();
    for run in &runs {
        if run.trim_end().lines().count() > 1 {
            problems.push(format!("a `run:` holds more than one line: {run:?}"));
        }
        if run.contains("cargo install") {
            problems.push(format!("`{run}` builds a tool from an unpinned source"));
        }
    }
    if !runs.iter().any(|r| r.starts_with(INSTALL)) {
        problems.push("no step installs cargo-deny with its pinned SHA-256".to_owned());
    }
    if !runs.contains(&VERB) {
        problems.push(format!("no step runs `{VERB}`"));
    }
    match yaml::load(ci) {
        Ok(ci) => {
            for key in PIN {
                let ours = job_env(&doc, JOB, key);
                let theirs = job_env(&ci, "lint", key);
                if ours.is_none() || ours != theirs {
                    problems.push(format!("{key} is {ours:?}, ci.yml pins {theirs:?}"));
                }
            }
        }
        Err(e) => problems.push(format!("ci.yml: {e}")),
    }
    let sha = job_env(&doc, JOB, "DENY_SHA256").unwrap_or_default();
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        problems.push(format!(
            "DENY_SHA256 {sha:?} is not 64 lowercase hex digits"
        ));
    }
    problems
}

/// One line for each place `ci.yml` (its text `ci`) names the advisories in what it runs:
/// comments are not read, so a comment may point at advisories.yml.
pub fn ci_problems(ci: &str) -> Vec<String> {
    let doc = match yaml::load(ci) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let mut values = Vec::new();
    scalars(&doc, &mut values);
    values
        .into_iter()
        .filter(|v| has_word(v, "advisories"))
        .map(|v| format!("ci.yml runs the advisories: `{v}`"))
        .collect()
}

fn read(name: &str) -> String {
    std::fs::read_to_string(workflows_dir().join(name)).expect("read the workflow")
}

/// A monitor that keeps every rule; its `ci.yml` is [`CI`].
const MONITOR: &str = "\
name: advisories
on:
  schedule:
    - cron: '23 5 * * *'
  push:
    branches: [main]
  workflow_dispatch:
permissions: {}
jobs:
  advisories:
    runs-on: ubuntu-latest
    if: ${{ github.ref == 'refs/heads/main' }}
    permissions:
      contents: read
      issues: write
    env:
      DENY_VERSION: 0.20.2
      DENY_SHA256: 9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
    steps:
      - run: cargo xtask install-tool --download \"$RUNNER_TEMP/cargo-deny.tar.gz\" --sha256 \"$DENY_SHA256\" --out x
      - env:
          GH_TOKEN: ${{ github.token }}
        run: cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"
";

/// The part of a `ci.yml` these rules read: the lint job's cargo-deny pin and its check.
const CI: &str = "\
on:
  pull_request:
jobs:
  lint:
    env:
      DENY_VERSION: 0.20.2
      DENY_SHA256: 9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
    steps:
      # Advisories: see advisories.yml.
      - run: cargo deny check bans licenses sources
";

fn assert_problems(text: &str, want: &[&str]) {
    assert_eq!(problems(text, CI), want, "in:\n{text}");
}

#[test]
fn the_real_monitor_keeps_every_rule_and_ci_never_runs_the_advisories() {
    // Catches: advisories.yml that gains a pull_request trigger, loses its schedule, push or
    // dispatch, holds a write scope at the top or off its job's main `if:`, installs an
    // unpinned cargo-deny or one other than ci.yml's, or stops running the verb; and a ci.yml
    // that starts checking advisories or waits for the monitor's job.
    let ci = read("ci.yml");
    assert_eq!(problems(&read(FILE), &ci), Vec::<String>::new());
    assert_eq!(ci_problems(&ci), Vec::<String>::new());
    // ci.yml's comment may point at the monitor; the lint reads no comment.
    assert!(ci.contains(FILE), "ci.yml points at {FILE}");
}

#[test]
fn the_small_monitor_keeps_every_rule() {
    // Catches: a rule that refuses a good monitor, so every case below would fail for that
    // reason instead of the defect it adds.
    assert_problems(MONITOR, &[]);
    assert_eq!(ci_problems(CI), Vec::<String>::new());
}

#[test]
fn a_trigger_other_than_schedule_push_to_main_and_dispatch_is_refused() {
    // Catches: the monitor run on a pull request (it would gate it, or write the issue from a
    // branch), a missing schedule, push or dispatch, a schedule with no cron, and a push of any
    // branch or with a path filter.
    let on = "on:\n  schedule:\n    - cron: '23 5 * * *'\n  push:\n    branches: [main]\n  workflow_dispatch:\n";
    let cases: [(&str, &[&str]); 7] = [
        (
            "on:\n  pull_request:\n  schedule:\n    - cron: '23 5 * * *'\n  push:\n    branches: [main]\n  workflow_dispatch:\n",
            &[
                "on must be schedule, push and workflow_dispatch only, not [\"pull_request\", \"push\", \"schedule\", \"workflow_dispatch\"]",
            ],
        ),
        (
            "on: [schedule, push, workflow_dispatch, pull_request]\n",
            &[
                "on must be schedule, push and workflow_dispatch only, not [\"pull_request\", \"push\", \"schedule\", \"workflow_dispatch\"]",
                "schedule has no `cron`",
                "push must be `branches: [main]` and nothing else",
            ],
        ),
        (
            "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n",
            &[
                "on must be schedule, push and workflow_dispatch only, not [\"push\", \"workflow_dispatch\"]",
            ],
        ),
        (
            "on:\n  schedule:\n  push:\n    branches: [main]\n  workflow_dispatch:\n",
            &["schedule has no `cron`"],
        ),
        (
            "on:\n  schedule:\n    - cron: '23 5 * * *'\n  push:\n  workflow_dispatch:\n",
            &["push must be `branches: [main]` and nothing else"],
        ),
        (
            "on:\n  schedule:\n    - cron: '23 5 * * *'\n  push:\n    branches: [main, dev]\n  workflow_dispatch:\n",
            &["push must be `branches: [main]` and nothing else"],
        ),
        (
            "on:\n  schedule:\n    - cron: '23 5 * * *'\n  push:\n    branches: [main]\n    paths: [Cargo.lock]\n  workflow_dispatch:\n",
            &["push must be `branches: [main]` and nothing else"],
        ),
    ];
    for (to, want) in cases {
        assert_problems(&with_in(MONITOR, on, to), want);
    }
    assert_problems(&with_in(MONITOR, on, ""), &["no `on`"]);
}

#[test]
fn a_write_scope_at_the_top_or_off_the_main_guarded_job_is_refused() {
    // Catches: issues: write for every job, a second job that writes, the issue job run for
    // any ref (a branch run by hand would write the issue), and a job wider or narrower than
    // reading the code and writing the issue.
    let top = "permissions: {}\n";
    assert_problems(
        &with_in(MONITOR, top, "permissions:\n  issues: write\n"),
        &["the top-level `permissions` holds a scope"],
    );
    assert_problems(
        &with_in(MONITOR, top, "permissions:\n  contents: read\n"),
        &["the top-level `permissions` holds a scope"],
    );
    let guard = "    if: ${{ github.ref == 'refs/heads/main' }}\n";
    assert_problems(
        &with_in(MONITOR, guard, ""),
        &["job advisories: needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`"],
    );
    assert_problems(
        &with_in(MONITOR, guard, "    if: ${{ always() }}\n"),
        &["job advisories: needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`"],
    );
    let scopes = "      contents: read\n      issues: write\n";
    let wrong = "job advisories: permissions must be exactly contents: read and issues: write";
    for to in [
        "      contents: write\n      issues: write\n",
        "      contents: read\n      issues: write\n      pull-requests: write\n",
        "      contents: read\n",
    ] {
        assert_problems(&with_in(MONITOR, scopes, to), &[wrong]);
    }
    let second = "jobs:\n  notify:\n    runs-on: ubuntu-latest\n    permissions:\n      issues: write\n    steps:\n      - run: cargo test\n  advisories:\n";
    assert_problems(
        &with_in(MONITOR, "jobs:\n  advisories:\n", second),
        &["job notify: holds a write scope; only advisories may"],
    );
    let reader = "jobs:\n  check:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n  advisories:\n";
    assert_problems(&with_in(MONITOR, "jobs:\n  advisories:\n", reader), &[]);
    assert_problems(
        &with_in(
            MONITOR,
            "  advisories:\n    runs-on",
            "  monitor:\n    runs-on",
        ),
        &[
            "job monitor: holds a write scope; only advisories may",
            "no job advisories",
            "DENY_VERSION is None, ci.yml pins Some(\"0.20.2\")",
            "DENY_SHA256 is None, ci.yml pins Some(\"9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f\")",
            "DENY_SHA256 \"\" is not 64 lowercase hex digits",
        ],
    );
}

#[test]
fn an_unpinned_or_unchecked_cargo_deny_or_inline_logic_is_refused() {
    // Catches: cargo-deny installed from a floating version, a download not checked against
    // its SHA-256, a pin that drifts from ci.yml's, a SHA-256 that is not one, logic written as
    // a shell script in the workflow, and the verb not run.
    let install = "      - run: cargo xtask install-tool --download \"$RUNNER_TEMP/cargo-deny.tar.gz\" --sha256 \"$DENY_SHA256\" --out x\n";
    let no_install = "no step installs cargo-deny with its pinned SHA-256";
    assert_problems(
        &with_in(MONITOR, install, "      - run: cargo install cargo-deny\n"),
        &[
            "`cargo install cargo-deny` builds a tool from an unpinned source",
            no_install,
        ],
    );
    assert_problems(
        &with_in(
            MONITOR,
            install,
            "      - run: cargo xtask install-tool --download \"$RUNNER_TEMP/cargo-deny.tar.gz\" --out x\n",
        ),
        &[no_install],
    );
    let version = "      DENY_VERSION: 0.20.2\n";
    assert_problems(
        &with_in(MONITOR, version, "      DENY_VERSION: 0.19.0\n"),
        &["DENY_VERSION is Some(\"0.19.0\"), ci.yml pins Some(\"0.20.2\")"],
    );
    let sha =
        "      DENY_SHA256: 9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f\n";
    assert_problems(
        &with_in(MONITOR, sha, ""),
        &[
            "DENY_SHA256 is None, ci.yml pins Some(\"9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f\")",
            "DENY_SHA256 \"\" is not 64 lowercase hex digits",
        ],
    );
    let upper = "9F12ED4C49936E09B48BF862B595CDE2FE64FCBD9D74DFACAC6131CA824C8D5F";
    let both = |text: &str| text.replace(&upper.to_lowercase(), upper);
    assert_eq!(
        problems(&both(MONITOR), &both(CI)),
        [format!(
            "DENY_SHA256 \"{upper}\" is not 64 lowercase hex digits"
        )]
    );
    let verb = "        run: cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"\n";
    assert_problems(
        &with_in(
            MONITOR,
            verb,
            "        run: |\n          gh issue list\n          cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"\n",
        ),
        &[
            "a `run:` holds more than one line: \"gh issue list\\ncargo xtask advisories --repo \\\"$GITHUB_REPOSITORY\\\"\\n\"",
            "no step runs `cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"`",
        ],
    );
    assert_problems(
        &with_in(MONITOR, verb, "        run: cargo deny check advisories\n"),
        &["no step runs `cargo xtask advisories --repo \"$GITHUB_REPOSITORY\"`"],
    );
}

#[test]
fn a_document_that_does_not_parse_is_refused() {
    // Catches: a monitor or a ci.yml that is not YAML read as having no problems.
    assert_eq!(problems("on: [unclosed\n", CI).len(), 1);
    let bad_ci = problems(MONITOR, "on: [unclosed\n");
    assert_eq!(bad_ci.len(), 1, "{bad_ci:?}");
    assert!(bad_ci[0].starts_with("ci.yml: "), "{bad_ci:?}");
    assert_eq!(ci_problems("on: [unclosed\n").len(), 1);
}

#[test]
fn a_ci_that_runs_or_waits_for_the_advisories_is_refused() {
    // Catches: the PR gate changed to check advisories (in the cargo-deny step's list, a step
    // of its own, a job, or a `needs` on the monitor's job), which would fail every open pull
    // request on a new advisory.
    let step = "      - run: cargo deny check bans licenses sources\n";
    let cases: [(&str, &str); 3] = [
        (
            "      - run: cargo deny check advisories bans licenses sources\n",
            "ci.yml runs the advisories: `cargo deny check advisories bans licenses sources`",
        ),
        (
            "      - run: cargo xtask advisories --repo x/y\n",
            "ci.yml runs the advisories: `cargo xtask advisories --repo x/y`",
        ),
        (
            "      - run: cargo test\n    needs: advisories\n",
            "ci.yml runs the advisories: `advisories`",
        ),
    ];
    for (to, want) in cases {
        assert_eq!(ci_problems(&with_in(CI, step, to)), [want]);
    }
    let job = "jobs:\n  advisories:\n    steps: []\n  lint:\n";
    assert_eq!(
        ci_problems(&with_in(CI, "jobs:\n  lint:\n", job)),
        ["ci.yml runs the advisories: `advisories`"]
    );
}
