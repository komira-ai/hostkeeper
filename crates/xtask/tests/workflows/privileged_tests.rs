//! The rules of `privileged.yml` ([`super::lint`]): a job that runs as root is allowed in that
//! file only, and there only on a push to main and by hand, on the runner the lint lists for the
//! job (the one macOS job, `macos-device`, on its own list), under every other rule; and each job
//! of the committed file holds exactly one `sudo`, the cargo runner of the run of its privileged
//! tests, with their build before it as the runner's user. Each rule is shown
//! to refuse its defect by putting it into an otherwise clean workflow and reading the exact
//! problems it reports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::lint::{
    HOSTED_RUNNERS, ID_TOKEN_ALLOWED, MACOS_DEVICE_JOB, MACOS_DEVICE_RUNNERS, PRIVILEGED_ON,
    PRIVILEGED_RUNNERS, PRIVILEGED_WORKFLOW, WRITE_ALLOWED, entries, entry, lint,
    privileged_runners, root_line, scalars,
};
use super::net_install::{
    NET_INSTALL_ARM64_FIRST_LINE, NET_INSTALL_ARM64_JOB, NET_INSTALL_FIRST_LINE, NET_INSTALL_JOB,
    NET_JOBS, apt_steps,
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

/// The `run:` values of every step of `job`, in order.
fn runs(job: &Node) -> Vec<&str> {
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps.as_slice(),
        _ => &[],
    };
    steps
        .iter()
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

/// How many times `sudo` stands as a word among the values of `node` (a comment is not a value).
fn sudo_count(node: &Node) -> usize {
    let mut values = Vec::new();
    scalars(node, &mut values);
    values.iter().map(|v| word_count(v, "sudo")).sum()
}

/// What is wrong with the shape of privileged.yml's `text`, whose jobs and the privileged test
/// targets of each are `targets` (job name to targets): a job that is not one of them, or one of
/// them missing; per job, `sudo` named other than once in its values, and none outside the jobs;
/// other than one build step and one step as root; the build as root or after the run; a step
/// that builds or runs another set of test targets than the job's; a step that runs more than
/// one command (`;`, `&&`, `||`, a pipe or more than one line), whose logic belongs in a Rust
/// tool. Each problem of a job names it.
pub fn shape_problems(text: &str, targets: &BTreeMap<&str, BTreeSet<String>>) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let named: BTreeSet<&str> = jobs.iter().filter_map(|(name, _)| name.as_str()).collect();
    for wanted in targets.keys().filter(|name| !named.contains(*name)) {
        problems.push(format!("no job {wanted}"));
    }
    let outside: usize =
        sudo_count(&doc) - jobs.iter().map(|(_, job)| sudo_count(job)).sum::<usize>();
    if outside != 0 {
        problems.push(format!("sudo is named {outside} times outside the jobs"));
    }
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        let Some(targets) = targets.get(name) else {
            problems.push(format!("job {name} is not a job of privileged.yml"));
            continue;
        };
        problems.extend(
            job_shape_problems(name, job, targets)
                .into_iter()
                .map(|problem| format!("job {name}: {problem}")),
        );
    }
    problems
}

/// [`shape_problems`] of one job, whose privileged test targets are `targets`.
fn job_shape_problems(name: &str, job: &Node, targets: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let runs = runs(job);
    let builds: Vec<usize> = (0..runs.len())
        .filter(|&i| runs[i].starts_with(BUILD))
        .collect();
    let net = NET_JOBS.iter().find(|net| net.name == name);
    let (apt_problems, apt_allowed) = apt_steps(&runs, builds.first().copied(), net);
    problems.extend(apt_problems);
    let sudo = sudo_count(job) - apt_allowed.len();
    if sudo != 1 {
        problems.push(format!("sudo is named {sudo} times, not once"));
    }
    for run in &runs {
        let chained = [";", "&&", "||", "|", "\n"]
            .iter()
            .any(|sep| run.trim_end().contains(sep));
        if chained {
            problems.push(format!("the step `{run}` runs more than one command"));
        }
    }
    let rooted: Vec<usize> = (0..runs.len())
        .filter(|&i| !apt_allowed.contains(&i) && root_line(&[runs[i]]).is_some())
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

/// The first line of a privileged test file of the Linux job: the tests against systemd.
const LINUX_FIRST_LINE: &str = "#![cfg(feature = \"privileged\")]";
/// The job of the first-boot tests: the coordinator and the daemon started as programs, which
/// change the paths the daemon fixes on the host, on a runner of their own.
const FIRST_BOOT_JOB: &str = "first-boot";
/// The first line of a privileged test file of [`FIRST_BOOT_JOB`].
const FIRST_BOOT_FIRST_LINE: &str = "#![cfg(all(target_os = \"linux\", feature = \"privileged\"))]";
/// The first line of a privileged test file of [`MACOS_DEVICE_JOB`]: the tests against launchd.
const MACOS_FIRST_LINE: &str = "#![cfg(all(target_os = \"macos\", feature = \"privileged\"))]";

/// Whether `text` starts with the attribute `first`, as rustfmt may break it over lines: the same
/// characters, whitespace aside, up to the attribute's closing bracket.
pub fn starts_with_attribute(text: &str, first: &str) -> bool {
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    squeeze(text).starts_with(&squeeze(first))
}

/// The privileged test targets of crates/hostkeeper-privileged whose first line is `first`: the
/// files of its `tests/` that start with it, by name without `.rs`.
fn privileged_targets(first: &str) -> BTreeSet<String> {
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
        if starts_with_attribute(&text, first) {
            found.insert(target.to_owned());
        }
    }
    found
}

/// The jobs of privileged.yml and the targets of each: the Linux job's, the first-boot job's, the
/// macOS job's and the two network-install jobs'.
pub fn committed_targets() -> BTreeMap<&'static str, BTreeSet<String>> {
    BTreeMap::from([
        ("privileged", privileged_targets(LINUX_FIRST_LINE)),
        (FIRST_BOOT_JOB, privileged_targets(FIRST_BOOT_FIRST_LINE)),
        (MACOS_DEVICE_JOB, privileged_targets(MACOS_FIRST_LINE)),
        (NET_INSTALL_JOB, privileged_targets(NET_INSTALL_FIRST_LINE)),
        (
            NET_INSTALL_ARM64_JOB,
            privileged_targets(NET_INSTALL_ARM64_FIRST_LINE),
        ),
    ])
}

#[test]
fn the_committed_privileged_yml_passes_and_holds_the_one_sudo() {
    // Catches: privileged.yml missing, failing a rule, named otherwise than the release waits for,
    // with a second step as root or its build moved under root, leaving out a privileged test
    // file (or naming one that is not, or one of another job), the first-boot job missing, and a
    // job that runs as root in any other workflow.
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
    let targets = committed_targets();
    assert_eq!(
        targets["privileged"],
        BTreeSet::from([
            "observer".to_owned(),
            "self_update".to_owned(),
            "tierstore_shaped".to_owned(),
            "transient_unit".to_owned(),
            "unit_control".to_owned()
        ])
    );
    assert_eq!(
        targets[FIRST_BOOT_JOB],
        BTreeSet::from(["first_boot".to_owned(), "install_uninstall".to_owned()])
    );
    assert_eq!(
        targets[MACOS_DEVICE_JOB],
        BTreeSet::from([
            "device_mac".to_owned(),
            "device_mac_e2e".to_owned(),
            "unpack_mac".to_owned()
        ])
    );
    assert_eq!(
        targets[NET_INSTALL_JOB],
        BTreeSet::from(["net_install".to_owned()])
    );
    assert_eq!(
        targets[NET_INSTALL_ARM64_JOB],
        BTreeSet::from(["net_install_arm64".to_owned()])
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
    let targets = BTreeMap::from([(
        "privileged",
        BTreeSet::from(["observer".to_owned(), "transient_unit".to_owned()]),
    )]);
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
            "job privileged: sudo is named 2 times, not once",
            "job privileged: 2 run as root steps, not one"
        ]
    );
    let rooted_build = "      - run: cargo test -p hostkeeper-privileged --features privileged --locked --no-run --config \"target.'cfg(unix)'.runner = 'sudo'\" --test observer --test transient_unit\n";
    assert_eq!(
        shape_problems(&with_in(&clean, build, rooted_build), &targets),
        [
            "job privileged: sudo is named 2 times, not once",
            "job privileged: 2 run as root steps, not one",
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
            format!("job privileged: the run as root step is not `{RUN} ...`"),
            "job privileged: the build step runs as root".to_owned(),
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
        ["job privileged: the build step comes after the run as root"]
    );
    let missing = with_in(&clean, build, &build.replace(" --test observer", ""));
    assert_eq!(
        shape_problems(&missing, &targets),
        [
            "job privileged: the build step names the test targets {\"transient_unit\"}, not {\"observer\", \"transient_unit\"}"
        ]
    );
    for chained in [
        "      - run: test -d /run/systemd/system || exit 1\n",
        "      - run: systemctl --version && systemd-analyze --version\n",
        "      - run: systemctl --version; true\n",
        "      - run: journalctl --no-pager | tail\n",
        "      - run: |\n          systemctl --version\n          systemd-analyze --version\n",
    ] {
        let text = with_in(&clean, build, &format!("{chained}{build}"));
        let found = shape_problems(&text, &targets);
        assert_eq!(found.len(), 1, "{chained}: {found:?}");
        assert!(
            found[0].ends_with("runs more than one command"),
            "{found:?}"
        );
    }
    let no_build = with_in(&clean, build, "");
    assert_eq!(
        shape_problems(&no_build, &targets),
        ["job privileged: 0 build steps, not one"]
    );
    assert_eq!(
        shape_problems("on: [unclosed\n", &targets).len(),
        1,
        "a text that does not parse is one problem"
    );
}

/// The macOS job of privileged.yml: the device tests, built as the runner's user and run as root.
// No `\` after the opening quote: it would drop the job's leading spaces and lift it out of
// `jobs`.
const MACOS_JOB: &str = "  macos-device:
    runs-on: macos-15
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: cargo test -p hostkeeper-privileged --features privileged --locked --no-run --test device_mac
      - run: cargo test -p hostkeeper-privileged --features privileged --locked --config \"target.'cfg(unix)'.runner = 'sudo'\" --test device_mac -- --test-threads=1
";

/// The run as root of [`MACOS_JOB`].
const MACOS_RUN_AS_ROOT: &str = "cargo test -p hostkeeper-privileged --features privileged --locked --config \"target.'cfg(unix)'.runner = 'sudo'\" --test device_mac -- --test-threads=1";

/// What the macOS job on another runner than its list is refused with.
const MACOS_OUTSIDE_LIST: &str =
    "job macos-device: privileged job on a runner outside the privileged list";

/// [`PRIVILEGED_CLEAN`] with the macOS job after the Linux one.
fn with_macos() -> String {
    format!("{PRIVILEGED_CLEAN}{MACOS_JOB}")
}

#[test]
fn the_macos_device_job_on_push_to_main_and_dispatch_only_is_accepted() {
    // Catches: a lint that refuses the macOS device job where it belongs (privileged.yml, run
    // after a merge to main and by hand, on its listed runner), or that lets the Linux job onto
    // the macOS list; and a list that names a runner that is not a standard hosted macOS label
    // of a fixed version.
    assert_eq!(privileged(&with_macos()), Vec::<String>::new());
    assert_eq!(privileged_runners(MACOS_DEVICE_JOB), MACOS_DEVICE_RUNNERS);
    assert_eq!(privileged_runners("privileged"), PRIVILEGED_RUNNERS);
    assert_eq!(privileged_runners("another"), PRIVILEGED_RUNNERS);
    for label in MACOS_DEVICE_RUNNERS {
        assert!(HOSTED_RUNNERS.contains(label), "{label}");
        assert!(
            label.starts_with("macos-") && !label.contains("latest"),
            "{label}"
        );
    }
    let linux_on_mac = with_in(
        &with_macos(),
        "    runs-on: ubuntu-24.04\n",
        "    runs-on: macos-15\n",
    );
    assert_eq!(privileged(&linux_on_mac), [OUTSIDE_LIST]);
}

#[test]
fn a_macos_job_with_a_pull_request_trigger_is_refused() {
    // Catches: the macOS job allowed in privileged.yml whatever its trigger: run for a pull
    // request, the file is refused for its trigger, each job for running as root, and the macOS
    // job for being macOS.
    let on = "on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n  pull_request:\n";
    let macos_root =
        format!("job macos-device: privileged on pull_request (`{MACOS_RUN_AS_ROOT}`)");
    let linux_root = on_pull_request();
    assert_eq!(
        privileged(&with_in(&with_macos(), ON, on)),
        [
            PRIVILEGED_ON,
            linux_root.as_str(),
            macos_root.as_str(),
            "job macos-device: macOS on pull_request (`macos-15`)",
        ]
    );
}

#[test]
fn a_macos_job_in_a_pull_request_workflow_is_refused() {
    // Catches: the macOS device job let into a workflow that runs for pull requests under any
    // other name: it is refused for running as root and for being macOS, by the pull-request rule.
    let workflow =
        format!("name: device\non:\n  pull_request:\npermissions: {{}}\njobs:\n{MACOS_JOB}");
    let macos_root =
        format!("job macos-device: privileged on pull_request (`{MACOS_RUN_AS_ROOT}`)");
    for file in ["ci.yml", "device.yml"] {
        assert_eq!(
            lint(file, &workflow),
            [
                macos_root.as_str(),
                "job macos-device: macOS on pull_request (`macos-15`)",
            ],
            "{file}"
        );
    }
}

#[test]
fn a_self_hosted_or_larger_runner_label_is_refused() {
    // Catches: the macOS job on a self-hosted runner, a paid larger runner, a moving `-latest`
    // label, another macOS version than the listed one, or Linux.
    let runner = "    runs-on: macos-15\n";
    let not_hosted =
        |label: &str| format!("job macos-device: runner `{label}` is not a GitHub-hosted runner");
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "self-hosted",
            vec![
                "the document names self-hosted".to_owned(),
                not_hosted("self-hosted"),
                MACOS_OUTSIDE_LIST.to_owned(),
            ],
        ),
        (
            "macos-15-xlarge",
            vec![not_hosted("macos-15-xlarge"), MACOS_OUTSIDE_LIST.to_owned()],
        ),
        (
            "macos-latest-large",
            vec![
                not_hosted("macos-latest-large"),
                MACOS_OUTSIDE_LIST.to_owned(),
            ],
        ),
        ("macos-latest", vec![MACOS_OUTSIDE_LIST.to_owned()]),
        ("macos-14", vec![MACOS_OUTSIDE_LIST.to_owned()]),
        ("ubuntu-24.04", vec![MACOS_OUTSIDE_LIST.to_owned()]),
    ];
    for (label, want) in cases {
        let text = with_in(
            &with_macos(),
            MACOS_JOB,
            &MACOS_JOB.replace(runner, &format!("    runs-on: {label}\n")),
        );
        assert_eq!(privileged(&text), want, "{label}");
    }
}

#[test]
fn the_shape_check_holds_each_job_to_its_own_targets() {
    // Catches: a shape check that lets the macOS job run the Linux targets (or none), that lets a
    // job it does not know into the file, that misses a job it expects, or that misses `sudo`
    // outside the jobs.
    let linux = BTreeSet::from(["observer".to_owned()]);
    let macos = BTreeSet::from(["device_mac".to_owned()]);
    let targets = BTreeMap::from([("privileged", linux), (MACOS_DEVICE_JOB, macos)]);
    let clean = with_macos().replace(" --no-run\n", " --no-run --test observer\n");
    let clean = clean.replacen(
        "runner = 'sudo'\" --",
        "runner = 'sudo'\" --test observer --",
        1,
    );
    assert_eq!(shape_problems(&clean, &targets), Vec::<String>::new());

    let wrong = clean.replace("--test device_mac", "--test observer");
    assert_eq!(
        shape_problems(&wrong, &targets),
        [
            "job macos-device: the build step names the test targets {\"observer\"}, not {\"device_mac\"}",
            "job macos-device: the run as root step names the test targets {\"observer\"}, not {\"device_mac\"}",
        ]
    );
    let extra = format!("{clean}{}", MACOS_JOB.replace("macos-device:", "another:"));
    assert_eq!(
        shape_problems(&extra, &targets),
        ["job another is not a job of privileged.yml"]
    );
    let missing = with_in(&clean, MACOS_JOB, "");
    assert_eq!(shape_problems(&missing, &targets), ["no job macos-device"]);
    let env = with_in(
        &clean,
        "permissions: {}\n",
        "permissions: {}\nenv:\n  ROOT: sudo\n",
    );
    assert_eq!(
        shape_problems(&env, &targets),
        ["sudo is named 1 times outside the jobs"]
    );
}
