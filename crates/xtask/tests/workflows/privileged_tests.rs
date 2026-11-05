//! The rules of `privileged.yml` ([`super::lint`]): a job that runs as root is allowed in that
//! file only, and there only on a push to main and by hand, on the runner the lint lists, under
//! every other rule; and the committed file holds exactly one `sudo`, the cargo runner of the run
//! of the privileged tests, with their build before it as the runner's user. Each rule is shown
//! to refuse its defect by putting it into an otherwise clean workflow and reading the exact
//! problems it reports.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::lint::{
    HOSTED_RUNNERS, ID_TOKEN_ALLOWED, PRIVILEGED_ON, PRIVILEGED_RUNNERS, PRIVILEGED_WORKFLOW,
    WRITE_ALLOWED, entries, entry, lint, root_line, scalars,
};
use super::release::{REQUIRED_WORKFLOWS, publish_problems};
use super::yaml::{self, Node, Value};
use super::{push_only, with_in, workflows};

/// The run of the privileged tests as root: the cargo runner `sudo`.
const RUN_AS_ROOT: &str = "cargo test -p hostkeeper-privileged --features privileged --locked --config \"target.'cfg(unix)'.runner = 'sudo'\" -- --test-threads=1";

/// A privileged.yml that passes every rule: a push to main and by hand, no token rights at the
/// top, one job on the privileged runner that reads the repository, builds the tests and runs
/// them as root. The `uses:` is on line 13.
const PRIVILEGED_CLEAN: &str = "\
name: privileged
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions: {}
jobs:
  privileged:
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: cargo test -p hostkeeper-privileged --features privileged --locked --no-run
      - run: cargo test -p hostkeeper-privileged --features privileged --locked --config \"target.'cfg(unix)'.runner = 'sudo'\" -- --test-threads=1
";

/// The `on` of [`PRIVILEGED_CLEAN`].
const ON: &str = "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n";

/// What a job of privileged.yml on another runner is refused with.
const OUTSIDE_LIST: &str = "job privileged: privileged job on a runner outside the privileged list";

fn privileged(text: &str) -> Vec<String> {
    lint(PRIVILEGED_WORKFLOW, text)
}

/// The pull-request rule's refusal of the job of [`PRIVILEGED_CLEAN`].
fn on_pull_request() -> String {
    format!("job privileged: privileged on pull_request (`{RUN_AS_ROOT}`)")
}

#[test]
fn the_clean_privileged_workflow_passes() {
    // Catches: a lint that refuses a good privileged.yml, so every case below would fail for
    // that reason instead of the defect it adds.
    assert_eq!(privileged(PRIVILEGED_CLEAN), Vec::<String>::new());
    // The order of the two triggers, and `{}` for no inputs, are the same workflow.
    let swapped = "on:\n  workflow_dispatch: {}\n  push:\n    branches: [\"main\"]\n";
    assert_eq!(
        privileged(&with_in(PRIVILEGED_CLEAN, ON, swapped)),
        Vec::<String>::new()
    );
}

#[test]
fn a_privileged_job_outside_privileged_yml_is_refused() {
    // Catches: a lint that still allows root in a workflow that does not run on pull requests,
    // whatever its name (one close to privileged.yml included), by `sudo` in a step, by a cargo
    // runner of `sudo`, by a privileged container, or by a container given a capability.
    let push = push_only();
    let step = "      - run: cargo test --workspace --locked\n";
    let cases: [(&str, &str); 3] = [
        (
            "      - run: sudo -E cargo test\n",
            "job test: privileged outside privileged.yml (`sudo -E cargo test`)",
        ),
        (
            "      - run: cargo test --config \"target.'cfg(unix)'.runner = 'sudo'\"\n",
            "job test: privileged outside privileged.yml (`cargo test --config \"target.'cfg(unix)'.runner = 'sudo'\"`)",
        ),
        (
            "      - run: docker run --privileged image\n",
            "job test: privileged outside privileged.yml (`docker run --privileged image`)",
        ),
    ];
    for file in [
        "ci.yml",
        "nightly.yml",
        "privileged.yaml",
        "privileged-tests.yml",
    ] {
        for (run, want) in cases {
            assert_eq!(lint(file, &with_in(&push, step, run)), [want], "{file}");
        }
    }
    let container = "    container:\n      image: rust:1\n      options: --cap-add NET_ADMIN\n    runs-on: ubuntu-latest\n";
    assert_eq!(
        lint(
            "ci.yml",
            &with_in(&push, "    runs-on: ubuntu-latest\n", container)
        ),
        ["job test: privileged outside privileged.yml (`--cap-add NET_ADMIN`)"]
    );
    // A word that only holds `sudo` is not root.
    let pseudo = with_in(&push, step, "      - run: cargo test pseudo_random\n");
    assert_eq!(lint("ci.yml", &pseudo), Vec::<String>::new());
}

#[test]
fn a_privileged_job_on_pull_request_is_still_refused_in_privileged_yml() {
    // Catches: an allowance keyed on the file name alone: privileged.yml run for a pull request,
    // in every shape of `on` (one name, a list, a mapping), by a review event or the merge
    // queue, is refused for its trigger and its job is refused by the pull-request rule.
    let shapes = [
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  pull_request:\n",
        "on:\n  pull_request:\n    branches: [main]\n  workflow_dispatch:\n",
        "on: pull_request\n",
        "on: [push, pull_request]\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  pull_request_review:\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  pull_request_review_comment:\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  merge_group:\n",
    ];
    let job = on_pull_request();
    for shape in shapes {
        assert_eq!(
            privileged(&with_in(PRIVILEGED_CLEAN, ON, shape)),
            [PRIVILEGED_ON, job.as_str()],
            "{shape}"
        );
    }
}

#[test]
fn privileged_yml_must_trigger_on_push_to_main_and_dispatch_only() {
    // Catches: privileged.yml run by another trigger, for a push of another branch or of tags,
    // for a push filtered by paths, without one of its two triggers, with dispatch inputs, or
    // with an `on` that is not a mapping.
    let cases = [
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  schedule:\n    - cron: '17 4 * * *'\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  workflow_call:\n",
        "on:\n  push:\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [main, next]\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [next]\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: ['*']\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: []\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: main\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches-ignore: [next]\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [main]\n    tags: ['v*']\n  workflow_dispatch:\n",
        "on:\n  push:\n    tags: ['v*']\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [main]\n    paths: ['crates/**']\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [main]\n",
        "on:\n  workflow_dispatch:\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n    inputs:\n      target:\n        type: string\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch: now\n",
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch: [x]\n",
        "on: [push, workflow_dispatch]\n",
        "on: push\n",
        "",
    ];
    for on in cases {
        assert_eq!(
            privileged(&with_in(PRIVILEGED_CLEAN, ON, on)),
            [PRIVILEGED_ON],
            "{on}"
        );
    }
    let job = on_pull_request();
    assert_eq!(
        privileged(&with_in(
            PRIVILEGED_CLEAN,
            ON,
            "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  pull_request:\n"
        )),
        [PRIVILEGED_ON, job.as_str()]
    );
}

#[test]
fn privileged_yml_runs_only_on_the_listed_runner() {
    // Catches: a privileged job on a runner whose image moves under us (`ubuntu-latest`), on
    // another Ubuntu, on macOS, through a matrix list or `include`, or on a custom label; and the
    // listed runner refused, directly, in a list or through a matrix.
    let runner = "    runs-on: ubuntu-24.04\n";
    let matrix = |values: &str| {
        format!("    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      matrix:\n{values}")
    };
    let cases: [(String, &[&str]); 8] = [
        ("    runs-on: ubuntu-latest\n".to_owned(), &[OUTSIDE_LIST]),
        ("    runs-on: ubuntu-22.04\n".to_owned(), &[OUTSIDE_LIST]),
        (
            "    runs-on: ubuntu-24.04-arm\n".to_owned(),
            &[OUTSIDE_LIST],
        ),
        ("    runs-on: macos-15\n".to_owned(), &[OUTSIDE_LIST]),
        (
            matrix("        os: [ubuntu-24.04, ubuntu-22.04]\n"),
            &[OUTSIDE_LIST],
        ),
        (
            matrix("        os: [ubuntu-24.04]\n        include:\n          - os: macos-15\n"),
            &[OUTSIDE_LIST],
        ),
        (
            "    runs-on: builder-1\n".to_owned(),
            &[
                "job privileged: runner `builder-1` is not a GitHub-hosted runner",
                OUTSIDE_LIST,
            ],
        ),
        (
            "    runs-on: [ubuntu-24.04, gpu]\n".to_owned(),
            &[
                "job privileged: runner `gpu` is not a GitHub-hosted runner",
                OUTSIDE_LIST,
            ],
        ),
    ];
    for (runs_on, want) in cases {
        assert_eq!(
            privileged(&with_in(PRIVILEGED_CLEAN, runner, &runs_on)),
            want,
            "{runs_on}"
        );
    }
    for good in [
        matrix("        os: [ubuntu-24.04]\n"),
        "    runs-on: [ubuntu-24.04]\n".to_owned(),
    ] {
        assert_eq!(
            privileged(&with_in(PRIVILEGED_CLEAN, runner, &good)),
            Vec::<String>::new(),
            "{good}"
        );
    }
    // The list holds hosted Ubuntu labels that name their version only.
    for label in PRIVILEGED_RUNNERS {
        assert!(HOSTED_RUNNERS.contains(label), "{label}");
        assert!(
            label.starts_with("ubuntu-") && !label.contains("latest"),
            "{label}"
        );
    }
}

#[test]
fn privileged_yml_keeps_the_common_rules() {
    // Catches: privileged.yml exempted from a rule every workflow keeps: a pinned action, a
    // top-level `permissions: {}`, no write scope (no table entry names the file), hosted
    // runners only, and no pull_request_target.
    assert!(WRITE_ALLOWED.iter().all(|a| a.file != PRIVILEGED_WORKFLOW));
    assert!(
        ID_TOKEN_ALLOWED
            .iter()
            .all(|(f, _)| *f != PRIVILEGED_WORKFLOW)
    );
    let pinned = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
    let read = "      contents: read\n";
    let job = on_pull_request();
    let cases: [(&str, &str, &[&str]); 6] = [
        (
            pinned,
            "actions/checkout@v7",
            &[
                "line 13: `uses: actions/checkout@v7` is not pinned to a 40-digit lowercase commit SHA",
            ],
        ),
        ("permissions: {}\n", "", &["no top-level `permissions: {}`"]),
        (
            read,
            "      contents: write\n",
            &["job privileged: `contents: write` is not allowed in privileged.yml"],
        ),
        (
            read,
            "      id-token: write\n",
            &["job privileged: `id-token: write` is not allowed in privileged.yml"],
        ),
        (
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: self-hosted\n",
            &[
                "the document names self-hosted",
                "job privileged: runner `self-hosted` is not a GitHub-hosted runner",
                OUTSIDE_LIST,
            ],
        ),
        (
            "  workflow_dispatch:\n",
            "  workflow_dispatch:\n  pull_request_target:\n",
            &[
                "`on` names pull_request_target",
                PRIVILEGED_ON,
                "the document names pull_request_target",
                job.as_str(),
            ],
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(
            privileged(&with_in(PRIVILEGED_CLEAN, from, to)),
            want,
            "{to}"
        );
    }
}

/// How many times `word` stands in `text` with no letter, digit or `_` either side of it.
fn word_count(text: &str, word: &str) -> usize {
    let inner = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word)
        .filter(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[*at + word.len()..].chars().next();
            !before.is_some_and(inner) && !after.is_some_and(inner)
        })
        .count()
}

/// The `run:` values of every step of every job of `doc`, in order.
fn runs(doc: &Node) -> Vec<&str> {
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    jobs.iter()
        .flat_map(|(_, job)| match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(steps)) => steps.as_slice(),
            _ => &[],
        })
        .filter_map(|step| entry(step, "run").and_then(Node::as_str))
        .collect()
}

/// The names that follow `--test` in `run`.
fn test_targets(run: &str) -> BTreeSet<String> {
    let words: Vec<&str> = run.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[0] == "--test")
        .map(|pair| pair[1].to_owned())
        .collect()
}

const BUILD: &str = "cargo test -p hostkeeper-privileged --features privileged --locked --no-run";
const RUN: &str = "cargo test -p hostkeeper-privileged --features privileged --locked --config \"target.'cfg(unix)'.runner = 'sudo'\"";

/// What is wrong with the shape of privileged.yml's `text`, whose privileged test targets are
/// `targets`: `sudo` named other than once in its values (a comment is not a value); other than
/// one build step and one step as root; the build as root or after the run; a step that builds
/// or runs another set of test targets.
fn shape_problems(text: &str, targets: &BTreeSet<String>) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let mut values = Vec::new();
    scalars(&doc, &mut values);
    let sudo: usize = values.iter().map(|v| word_count(v, "sudo")).sum();
    if sudo != 1 {
        problems.push(format!("sudo is named {sudo} times, not once"));
    }
    let runs = runs(&doc);
    let builds: Vec<usize> = (0..runs.len())
        .filter(|&i| runs[i].starts_with(BUILD))
        .collect();
    let rooted: Vec<usize> = (0..runs.len())
        .filter(|&i| root_line(&[runs[i]]).is_some())
        .collect();
    for (what, found, start) in [("build", &builds, BUILD), ("run as root", &rooted, RUN)] {
        match found[..] {
            [at] => {
                if !runs[at].starts_with(start) {
                    problems.push(format!("the {what} step is not `{start} ...`"));
                }
                if test_targets(runs[at]) != *targets {
                    problems.push(format!(
                        "the {what} step names the test targets {:?}, not {targets:?}",
                        test_targets(runs[at])
                    ));
                }
            }
            _ => problems.push(format!("{} {what} steps, not one", found.len())),
        }
    }
    if let ([build], [root]) = (&builds[..], &rooted[..]) {
        if build == root {
            problems.push("the build step runs as root".to_owned());
        } else if build > root {
            problems.push("the build step comes after the run as root".to_owned());
        }
    }
    problems
}

/// The privileged test targets of crates/hostkeeper-privileged: the files of its `tests/` whose
/// first line is `#![cfg(feature = "privileged")]`, by name without `.rs`.
fn privileged_targets() -> BTreeSet<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hostkeeper-privileged/tests");
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("list the privileged crate's tests") {
        let path = entry.expect("an entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(target) = name.strip_suffix(".rs") else {
            continue;
        };
        let text = std::fs::read_to_string(&path).expect("read a test file");
        if text.lines().next() == Some("#![cfg(feature = \"privileged\")]") {
            found.insert(target.to_owned());
        }
    }
    found
}

#[test]
fn the_committed_privileged_yml_passes_and_holds_the_one_sudo() {
    // Catches: privileged.yml missing, failing a rule, named otherwise than the release waits for,
    // with a second step as root or its build moved under root, leaving out a privileged test
    // file (or naming one that is not), and a job that runs as root in any other workflow.
    let all = workflows();
    let text = &all
        .iter()
        .find(|(file, _)| file == PRIVILEGED_WORKFLOW)
        .expect("privileged.yml is in the repository")
        .1;
    assert_eq!(privileged(text), Vec::<String>::new());
    assert_eq!(
        publish_problems(PRIVILEGED_WORKFLOW, text),
        Vec::<String>::new()
    );
    let doc = yaml::load(text).expect("privileged.yml parses");
    let name = entry(&doc, "name").and_then(Node::as_str);
    assert_eq!(name, Some("privileged"));
    assert!(
        REQUIRED_WORKFLOWS.contains(&"privileged"),
        "a release waits for it"
    );
    let targets = privileged_targets();
    assert_eq!(
        targets,
        BTreeSet::from(["observer".to_owned(), "transient_unit".to_owned()])
    );
    assert_eq!(shape_problems(text, &targets), Vec::<String>::new());
    for (file, other) in all.iter().filter(|(file, _)| file != PRIVILEGED_WORKFLOW) {
        let doc = yaml::load(other).expect("a workflow parses");
        let mut values = Vec::new();
        scalars(&doc, &mut values);
        assert_eq!(root_line(&values), None, "{file} runs something as root");
    }
}

#[test]
fn the_shape_check_of_privileged_yml_refuses_each_defect() {
    // Catches: a shape check that passes a second step as root, a build moved under root or after
    // the run, a step that leaves out a test target, or a missing build; and one that counts a
    // `sudo` in a comment, which runs nothing.
    let targets = BTreeSet::from(["observer".to_owned(), "transient_unit".to_owned()]);
    let clean = format!(
        "{}{}",
        PRIVILEGED_CLEAN
            .replace(
                " --no-run\n",
                " --no-run --test observer --test transient_unit\n"
            )
            .replace(
                "runner = 'sudo'\" --",
                "runner = 'sudo'\" --test observer --test transient_unit --"
            ),
        "      # sudo is named here, in a comment, which is not a value\n"
    );
    assert_eq!(shape_problems(&clean, &targets), Vec::<String>::new());
    let build = "      - run: cargo test -p hostkeeper-privileged --features privileged --locked --no-run --test observer --test transient_unit\n";
    let second = format!("{build}      - run: sudo journalctl --no-pager\n");
    assert_eq!(
        shape_problems(&with_in(&clean, build, &second), &targets),
        [
            "sudo is named 2 times, not once",
            "2 run as root steps, not one"
        ]
    );
    let rooted_build = "      - run: cargo test -p hostkeeper-privileged --features privileged --locked --no-run --config \"target.'cfg(unix)'.runner = 'sudo'\" --test observer --test transient_unit\n";
    assert_eq!(
        shape_problems(&with_in(&clean, build, rooted_build), &targets),
        [
            "sudo is named 2 times, not once",
            "2 run as root steps, not one",
        ]
    );
    // The run as root folded into the build: one step as root, and it is the build.
    let run_step = clean
        .lines()
        .find(|line| line.contains("runner = 'sudo'"))
        .map(|line| format!("{line}\n"))
        .expect("the run step");
    let folded = with_in(&with_in(&clean, &run_step, ""), build, rooted_build);
    assert_eq!(
        shape_problems(&folded, &targets),
        [
            format!("the run as root step is not `{RUN} ...`"),
            "the build step runs as root".to_owned(),
        ]
    );
    let run_line = clean
        .lines()
        .find(|line| line.contains("runner = 'sudo'"))
        .expect("the run step")
        .to_owned();
    let after = with_in(&clean, &format!("{run_line}\n"), "");
    let after = with_in(&after, build, &format!("{run_line}\n{build}"));
    assert_eq!(
        shape_problems(&after, &targets),
        ["the build step comes after the run as root"]
    );
    let missing = with_in(&clean, build, &build.replace(" --test observer", ""));
    assert_eq!(
        shape_problems(&missing, &targets),
        [
            "the build step names the test targets {\"transient_unit\"}, not {\"observer\", \"transient_unit\"}"
        ]
    );
    let no_build = with_in(&clean, build, "");
    assert_eq!(
        shape_problems(&no_build, &targets),
        ["0 build steps, not one"]
    );
    assert_eq!(
        shape_problems("on: [unclosed\n", &targets).len(),
        1,
        "a text that does not parse is one problem"
    );
}
