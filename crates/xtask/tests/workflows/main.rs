//! The workflow lint: every file in `.github/workflows` is held to the rules in [`lint`], and
//! the lint is shown to refuse each defect by putting it into a workflow that is otherwise
//! clean ([`CLEAN`]) and reading the exact problems it reports. The rules of `privileged.yml`, the
//! workflow that runs the privileged tests as root after a merge, are tested in
//! `privileged_tests.rs`; those of `net-install.yml`, the other workflow with jobs as root, the
//! network-install jobs on main, in `net_install_tests.rs`; those of its job `openrc-device`, in
//! `openrc_tests.rs`, and of its target that runs on both platforms, in `both_platforms_tests.rs`; those of `pr-device.yml`, the one workflow with macOS jobs and jobs as root on a pull request, and of
//! no secret on a pull request, in `pr_device_tests.rs`, its path rule (the job `changes` and its
//! skip list) in `pr_paths_tests.rs`, and the conditions of its macOS jobs and of their gates,
//! evaluated against every outcome, in `pr_device_gate_tests.rs`; those of `advisories.yml`, the advisory
//! monitor, in `advisories.rs`.

mod advisories;
mod both_platforms_tests;
mod callers;
mod callers_tests;
mod channel_join_tests;
mod channel_run_tests;
mod ci;
mod ci_tests;
mod desired;
mod desired_tests;
mod downloads;
mod downloads_tests;
mod expr;
mod hello_run_tests;
mod image_push_tests;
mod lint;
mod linux_packages_tests;
mod linux_releases;
mod linux_releases_tests;
mod mdm_package_tests;
mod net_install;
mod net_install_tests;
mod one_command;
mod one_command_tests;
mod openrc;
mod openrc_tests;
mod packages;
mod packages_tests;
mod pkg_signing;
mod pkg_signing_tests;
mod pr_device;
mod pr_device_gate_tests;
mod pr_device_tests;
mod pr_paths;
mod pr_paths_tests;
mod previous_release;
mod previous_release_tests;
mod privileged_tests;
mod release;
mod release_files;
mod release_tests;
#[path = "../hygiene/rust_text.rs"]
mod rust_text;
mod services_tests;
mod set_desired;
mod set_desired_calls;
mod set_desired_calls_tests;
mod set_desired_outputs_tests;
mod set_desired_tag;
mod set_desired_tag_tests;
mod set_desired_tests;
mod template_doc;
mod template_doc_tests;
#[path = "../../src/testdir.rs"]
mod testdir;
mod verify_doc;
mod verify_doc_tests;
mod xtask_calls;
mod xtask_calls_tests;
mod yaml;

use std::path::PathBuf;

use lint::{HOSTED_RUNNERS, PRIVILEGED_WORKFLOW, WRITE_ALLOWED, lint};

/// The directory of the workflows of this repository.
fn workflows_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows")
        .canonicalize()
        .expect("the workflows directory")
}

/// `(file name, text)` of every workflow, sorted by name.
fn workflows() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(workflows_dir()).expect("list the workflows") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if name.ends_with(".yml") || name.ends_with(".yaml") {
            found.push((name, std::fs::read_to_string(&path).expect("read")));
        }
    }
    found.sort();
    found
}

/// A workflow that passes every rule: it runs on pull requests and on pushes to main, holds no
/// token rights at the top, and its one job reads the repository on a hosted runner with a
/// pinned action. The `uses:` is on line 13.
const CLEAN: &str = "\
name: example
on:
  pull_request:
  push:
    branches: [main]
permissions: {}
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: cargo test --workspace --locked
";

/// [`CLEAN`] with the one occurrence of `from` replaced by `to`.
fn with(from: &str, to: &str) -> String {
    with_in(CLEAN, from, to)
}

fn with_in(text: &str, from: &str, to: &str) -> String {
    assert_eq!(
        text.matches(from).count(),
        1,
        "{from:?} is not in the text once"
    );
    text.replacen(from, to, 1)
}

/// [`CLEAN`] that runs on pushes to main and by hand, not on pull requests.
fn push_only() -> String {
    with("  pull_request:\n", "  workflow_dispatch:\n")
}

fn problems(text: &str) -> Vec<String> {
    lint("ci.yml", text)
}

fn assert_problems(text: &str, want: &[&str]) {
    assert_eq!(problems(text), want, "in:\n{text}");
}

#[test]
fn every_workflow_of_the_repository_passes_the_lint() {
    // Catches: an unpinned action, a missing top-level `permissions: {}`, pull_request_target,
    // a runner that is not GitHub-hosted, a write scope outside the table, or a privileged or
    // macOS job on pull_request outside pr-device.yml's jobs that may be, a copy in pr-device.yml
    // that drifted from privileged.yml's job, a release workflow out of its shape, or a workflow
    // that tags or publishes on pull_request or outside the release group, in any workflow file
    // of this repository; and ci.yml's test shards or coverage gate out of their shape.
    let all = workflows();
    let text_of = |required: &str| {
        all.iter()
            .find(|(name, _)| name == required)
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| panic!("{required} is missing"))
    };
    let mut found =
        pr_device::mirror_problems(&text_of(pr_device::FILE), &text_of(PRIVILEGED_WORKFLOW));
    found.extend(
        ci::problems(&text_of(ci::FILE))
            .into_iter()
            .map(|p| format!("{}: {p}", ci::FILE)),
    );
    for (file, text) in all {
        let publish = release::publish_problems(&file, &text);
        for p in lint(&file, &text).into_iter().chain(publish) {
            found.push(format!("{file}: {p}"));
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn the_clean_workflow_and_its_push_only_form_pass() {
    // Catches: a lint that refuses a good workflow, so every case below would fail for that
    // reason instead of the defect it adds.
    assert_problems(CLEAN, &[]);
    assert_problems(&push_only(), &[]);
}

#[test]
fn an_action_not_pinned_to_a_commit_sha_with_a_version_comment_is_refused() {
    // Catches: an action that a tag or branch move would change under us, an upper-case or
    // short SHA, a local action the lint does not read, and a pin with no version comment.
    let pinned = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
    let cases: [(&str, &str); 7] = [
        (
            "actions/checkout@v7",
            "line 13: `uses: actions/checkout@v7` is not pinned to a 40-digit lowercase commit SHA",
        ),
        (
            "actions/checkout@main",
            "line 13: `uses: actions/checkout@main` is not pinned to a 40-digit lowercase commit SHA",
        ),
        (
            "actions/checkout",
            "line 13: `uses: actions/checkout` is not pinned (owner/repo@<40 hex digits>)",
        ),
        (
            "actions/checkout@3D3C42E5AAC5BA805825DA76410C181273BA90B1 # v7.0.1",
            "line 13: `uses: actions/checkout@3D3C42E5AAC5BA805825DA76410C181273BA90B1` is not pinned to a 40-digit lowercase commit SHA",
        ),
        (
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b # v7.0.1",
            "line 13: `uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b` is not pinned to a 40-digit lowercase commit SHA",
        ),
        (
            "./.github/actions/setup",
            "line 13: `uses: ./.github/actions/setup` names a local path",
        ),
        (
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
            "line 13: `uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1` has no version comment (`# v1.2.3`) on its line",
        ),
    ];
    for (uses, want) in cases {
        assert_problems(&with(pinned, uses), &[want]);
    }
    // A docker image is not a pinned action either.
    assert_problems(
        &with(pinned, "docker://alpine:3"),
        &["line 13: `uses: docker://alpine:3` is not pinned (owner/repo@<40 hex digits>)"],
    );
}

#[test]
fn a_self_hosted_or_other_non_hosted_runner_is_refused() {
    // Catches: a job on a self-hosted runner (it runs pull-request code on a machine we keep),
    // in each way `runs-on` can name one, and on a paid larger runner's custom label.
    let hosted = "    runs-on: ubuntu-latest\n";
    let cases: [(&str, &[&str]); 5] = [
        (
            "    runs-on: self-hosted\n",
            &[
                "the document names self-hosted",
                "job test: runner `self-hosted` is not a GitHub-hosted runner",
            ],
        ),
        (
            "    runs-on: [self-hosted, linux, x64]\n",
            &[
                "the document names self-hosted",
                "job test: runner `self-hosted` is not a GitHub-hosted runner",
                "job test: runner `linux` is not a GitHub-hosted runner",
                "job test: runner `x64` is not a GitHub-hosted runner",
            ],
        ),
        (
            "    runs-on:\n      group: builders\n",
            &["job test: `runs-on` names a runner group, not a GitHub-hosted runner"],
        ),
        (
            "    runs-on: ubuntu-latest-16-cores\n",
            &["job test: runner `ubuntu-latest-16-cores` is not a GitHub-hosted runner"],
        ),
        (
            "    runs-on: ${{ vars.RUNNER }}\n",
            &[
                "job test: `runs-on: ${{ vars.RUNNER }}` is an expression other than `${{ matrix.<key> }}`",
            ],
        ),
    ];
    for (runs_on, want) in cases {
        assert_problems(&with(hosted, runs_on), want);
    }
}

#[test]
fn a_matrix_runner_is_read_from_the_jobs_own_matrix() {
    // Catches: a matrix that hides a self-hosted runner behind `${{ matrix.os }}`, through its
    // list or its `include`, and a key the matrix does not have; and a matrix of hosted
    // runners refused.
    let hosted = "    runs-on: ubuntu-latest\n";
    let matrix = |values: &str| {
        format!("    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      matrix:\n{values}")
    };
    let good = matrix("        os: [ubuntu-24.04, ubuntu-24.04-arm]\n");
    assert_problems(&with_in(&push_only(), hosted, &good), &[]);
    let listed = matrix("        os: [ubuntu-24.04, builder-1]\n");
    assert_problems(
        &with(hosted, &listed),
        &["job test: runner `builder-1` is not a GitHub-hosted runner"],
    );
    let included = matrix("        include:\n          - os: builder-2\n");
    assert_problems(
        &with(hosted, &included),
        &["job test: runner `builder-2` is not a GitHub-hosted runner"],
    );
    let missing = matrix("        arch: [x64]\n");
    assert_problems(
        &with(hosted, &missing),
        &["job test: `runs-on: ${{ matrix.os }}` has no values in the job's `strategy.matrix`"],
    );
    let nested = "    runs-on: [ubuntu-latest, [x]]\n";
    assert_problems(
        &with(hosted, nested),
        &["job test: a `runs-on` label is not text"],
    );
}

#[test]
fn a_job_with_no_runner_is_refused() {
    // Catches: a job that calls a reusable workflow, whose jobs this lint cannot read, slipping
    // past the runner and pull-request rules.
    assert_problems(
        &with("    runs-on: ubuntu-latest\n", ""),
        &["job test: no `runs-on` (a job that calls a reusable workflow is not evaluated)"],
    );
}

#[test]
fn a_missing_or_non_empty_top_level_permissions_is_refused() {
    // Catches: a workflow whose jobs start with the repository's default token rights.
    let empty = "permissions: {}\n";
    for top in [
        "",
        "permissions: read-all\n",
        "permissions:\n  contents: read\n",
    ] {
        assert_problems(&with(empty, top), &["no top-level `permissions: {}`"]);
    }
    // `{}` written as a block mapping with no entries is the same document.
    assert_problems(&with(empty, "permissions: { }\n"), &[]);
}

#[test]
fn pull_request_target_is_refused_in_every_shape() {
    // Catches: a workflow that runs fork code with the base repository's token and secrets,
    // whether `on` is a mapping, a list or one name, or the word is anywhere else.
    let as_map = with("  pull_request:\n", "  pull_request_target:\n");
    let want = [
        "`on` names pull_request_target",
        "the document names pull_request_target",
    ];
    assert_problems(&as_map, &want);
    let on = "on:\n  pull_request:\n  push:\n    branches: [main]\n";
    assert_problems(&with(on, "on: [push, pull_request_target]\n"), &want);
    assert_problems(&with(on, "on: pull_request_target\n"), &want);
    assert_problems(
        &with("name: example\n", "name: pull_request_target\n"),
        &["the document names pull_request_target"],
    );
}

#[test]
fn a_write_scope_outside_the_table_or_a_permissions_scalar_is_refused() {
    // Catches: a job that grants itself a write scope no rule allows, or every scope at once.
    assert!(
        WRITE_ALLOWED.iter().all(|a| a.file != "ci.yml"),
        "no job of ci.yml writes"
    );
    let read = "      contents: read\n";
    assert_problems(
        &with_in(&push_only(), read, "      contents: write\n"),
        &["job test: `contents: write` is not allowed in ci.yml"],
    );
    assert_problems(
        &with_in(&push_only(), read, "      id-token: write\n"),
        &["job test: `id-token: write` is not allowed in ci.yml"],
    );
    assert_problems(
        &with_in(
            &push_only(),
            "    permissions:\n      contents: read\n",
            "    permissions: write-all\n",
        ),
        &["job test: `permissions` is not a mapping of scopes"],
    );
    assert_problems(&with_in(&push_only(), read, "      contents: none\n"), &[]);
}

#[test]
fn a_privileged_job_on_pull_request_is_refused() {
    // Catches: root on a pull request, by `sudo` in a step, by a cargo runner of `sudo`, by a
    // privileged container, or by a write scope on the token.
    let step = "      - run: cargo test --workspace --locked\n";
    let cases: [(&str, &str); 4] = [
        (
            "      - run: sudo apt-get install --yes nftables\n",
            "job test: privileged on pull_request (`sudo apt-get install --yes nftables`)",
        ),
        (
            "      - run: cargo test --config \"target.'cfg(unix)'.runner = 'sudo'\"\n",
            "job test: privileged on pull_request (`cargo test --config \"target.'cfg(unix)'.runner = 'sudo'\"`)",
        ),
        (
            "      - run: cargo run --bin sudo-test\n",
            "job test: privileged on pull_request (`cargo run --bin sudo-test`)",
        ),
        (
            "      - run: |\n          cargo build\n          docker run --privileged image\n",
            "job test: privileged on pull_request (`docker run --privileged image`)",
        ),
    ];
    for (run, want) in cases {
        assert_problems(&with(step, run), &[want]);
    }
    let container = "    container:\n      image: rust@sha256:f9a7b9ebf6f56201f949db745e47bc423c0f0f8294bafe991cc10bf74b029657\n      options: --cap-add NET_ADMIN\n    runs-on: ubuntu-latest\n";
    assert_problems(
        &with("    runs-on: ubuntu-latest\n", container),
        &["job test: privileged on pull_request (`--cap-add NET_ADMIN`)"],
    );
    assert_problems(
        &with("      contents: read\n", "      checks: write\n"),
        &[
            "job test: `checks: write` is not allowed in ci.yml",
            "job test: privileged on pull_request (`checks: write`)",
        ],
    );
}

#[test]
fn a_macos_job_on_pull_request_is_refused() {
    // Catches: a macOS runner on a pull request, directly or through a matrix, and a darwin
    // target built there.
    let hosted = "    runs-on: ubuntu-latest\n";
    assert_problems(
        &with(hosted, "    runs-on: macos-15\n"),
        &["job test: macOS on pull_request (`macos-15`)"],
    );
    let matrix = "    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, macOS-14]\n";
    assert_problems(
        &with(hosted, matrix),
        &[
            "job test: runner `macOS-14` is not a GitHub-hosted runner",
            "job test: macOS on pull_request (`macOS-14`)",
        ],
    );
    assert_problems(
        &with(
            "      - run: cargo test --workspace --locked\n",
            "      - run: cargo check --target aarch64-apple-darwin\n",
        ),
        &["job test: macOS on pull_request (`cargo check --target aarch64-apple-darwin`)"],
    );
}

#[test]
fn the_pull_request_rule_holds_in_every_shape_of_on_and_only_there() {
    // Catches: `on: pull_request` written as one name or a list escaping the rule, and the rule
    // refusing a macOS job in a workflow that never runs on a pull request. Root in such a
    // workflow is refused by the rule of privileged.yml, not by this one; in privileged.yml on
    // its runner it is clean.
    let on = "on:\n  pull_request:\n  push:\n    branches: [main]\n";
    let mac = "    runs-on: macos-15\n";
    let want = ["job test: macOS on pull_request (`macos-15`)"];
    for shape in ["on: pull_request\n", "on: [push, pull_request]\n"] {
        let text = with_in(&with(on, shape), "    runs-on: ubuntu-latest\n", mac);
        assert_problems(&text, &want);
    }
    let push = push_only();
    let text = with_in(&push, "    runs-on: ubuntu-latest\n", mac);
    assert_problems(&text, &[]);
    let text = with_in(
        &text,
        "      - run: cargo test --workspace --locked\n",
        "      - run: sudo -E cargo test\n",
    );
    assert_problems(
        &text,
        &["job test: privileged outside privileged.yml and net-install.yml (`sudo -E cargo test`)"],
    );
    let ubuntu = with_in(&text, mac, "    runs-on: ubuntu-24.04\n");
    assert_eq!(lint(PRIVILEGED_WORKFLOW, &ubuntu), Vec::<String>::new());
}

#[test]
fn a_word_that_only_contains_sudo_or_a_comment_is_not_privileged() {
    // Catches: a rule so loose that every pull-request workflow fails on an innocent word, or
    // on a comment, which is not part of what runs.
    let step = "      - run: cargo test --workspace --locked\n";
    assert_problems(&with(step, "      - run: cargo test pseudo_random\n"), &[]);
    assert_problems(
        &with(
            step,
            "      # sudo and macos are named only in this comment\n      - run: cargo test\n",
        ),
        &[],
    );
}

#[test]
fn every_hosted_runner_label_is_one_of_githubs_standard_labels() {
    // Catches: a custom label added to the list, which would let a self-hosted or paid runner
    // pass.
    for label in HOSTED_RUNNERS {
        let os = label.split('-').next().unwrap_or_default();
        assert!(matches!(os, "ubuntu" | "macos"), "{label}");
        assert!(!label.contains("core"), "{label} is a larger runner");
    }
}

#[test]
fn a_document_the_lint_does_not_evaluate_is_refused() {
    // Catches: a workflow read some other way than GitHub reads it, by a tag, a second
    // document, an alias bomb, text that is not YAML, or a top level that is not a mapping.
    assert_eq!(
        problems("permissions: !!map {}\n"),
        ["line 1: a YAML tag (`tag:yaml.org,2002:!map`) is not evaluated"]
    );
    assert_eq!(
        problems("permissions: {}\n---\npermissions: {}\n"),
        ["line 3: a second YAML document is not evaluated"]
    );
    let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
    for (i, prev) in ["b", "c", "d", "e", "f"]
        .iter()
        .zip(["a", "b", "c", "d", "e"])
    {
        bomb.push_str(&format!(
            "{i}: &{i} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n"
        ));
    }
    let found = problems(&bomb);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].ends_with("the document expands to more than 100000 nodes"),
        "{found:?}"
    );
    let found = problems("on: [unclosed\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("line 0: not valid YAML: "),
        "{found:?}"
    );
    assert_eq!(problems(""), ["line 0: the file holds no YAML document"]);
    assert_eq!(problems("- a\n- b\n"), ["the document is not a mapping"]);
    assert_eq!(
        problems("x: *nothing\n").len(),
        1,
        "an alias to no anchor is refused"
    );
}

#[test]
fn workflow_level_env_and_defaults_on_pull_request_are_held_to_the_same_rule() {
    // Catches: a privileged runner or a darwin target set once for every job, beside `jobs`,
    // escaping the per-job scan; and the same values passing where no pull request runs.
    let at = "permissions: {}\n";
    let cases: [(&str, &str); 3] = [
        (
            "env:\n  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER: sudo\n",
            "workflow: privileged on pull_request (`sudo`)",
        ),
        (
            "env:\n  CARGO_BUILD_TARGET: aarch64-apple-darwin\n",
            "workflow: macOS on pull_request (`aarch64-apple-darwin`)",
        ),
        (
            "defaults:\n  run:\n    shell: sudo bash {0}\n",
            "workflow: privileged on pull_request (`sudo bash {0}`)",
        ),
    ];
    // Where no pull request runs, a darwin target is allowed and root is refused by the rule of
    // privileged.yml.
    let push_only_wants: [&[&str]; 3] = [
        &["workflow: privileged outside privileged.yml and net-install.yml (`sudo`)"],
        &[],
        &["workflow: privileged outside privileged.yml and net-install.yml (`sudo bash {0}`)"],
    ];
    for ((top, want), push_want) in cases.into_iter().zip(push_only_wants) {
        assert_problems(&with(at, &format!("{at}{top}")), &[want]);
        assert_problems(&with_in(&push_only(), at, &format!("{at}{top}")), push_want);
    }
}

#[test]
fn every_trigger_that_runs_for_a_pull_request_is_held_to_the_pull_request_rule() {
    // Catches: a macOS job reaching a pull request through a review event or the merge queue.
    let mac = with("    runs-on: ubuntu-latest\n", "    runs-on: macos-15\n");
    for trigger in [
        "pull_request_review",
        "pull_request_review_comment",
        "merge_group",
    ] {
        let text = with_in(&mac, "  pull_request:\n", &format!("  {trigger}:\n"));
        assert_problems(&text, &["job test: macOS on pull_request (`macos-15`)"]);
    }
}

#[test]
fn a_matrix_runner_list_that_is_not_a_list_is_refused() {
    // Catches: an expression standing for the runner list, next to a hosted `include`, leaving
    // the expression's labels unchecked.
    let runs_on = "    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: ${{ fromJSON(inputs.runners) }}\n        include:\n          - os: ubuntu-latest\n";
    assert_problems(
        &with("    runs-on: ubuntu-latest\n", runs_on),
        &["job test: `strategy.matrix.os` is not a list of runner labels"],
    );
}

#[test]
fn a_quoted_or_spaced_pin_and_an_alias_of_a_pin_pass() {
    // Catches: a version comment missed because the value is quoted, because of two spaces or
    // a tab before or after `#`, or because the `uses:` is an alias of a pinned anchor.
    let pinned =
        "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n";
    let sha = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
    for line in [
        format!("      - uses: '{sha}' # v7.0.1\n"),
        format!("      - uses: \"{sha}\" # v7.0.1\n"),
        format!("      - uses: {sha}   #   v7.0.1\n"),
        format!("      - uses: {sha}\t#\tv7.0.1\n"),
    ] {
        assert_problems(&with(pinned, &line), &[]);
    }
    let anchored = format!(
        "      - uses: &checkout {sha} # v7.0.1\n        with:\n          persist-credentials: false\n      - uses: *checkout\n"
    );
    let text = with(pinned, &anchored);
    let text = with_in(
        &text,
        "        with:\n          persist-credentials: false\n      - run:",
        "      - run:",
    );
    assert_problems(&text, &[]);
    // A `#` with no space before it is part of the value, not a comment.
    assert_problems(
        &with(pinned, &format!("      - uses: {sha}#v7.0.1\n")),
        &[&format!(
            "line 13: `uses: {sha}#v7.0.1` is not pinned to a 40-digit lowercase commit SHA"
        )],
    );
    // A quoted value with no comment after it still has none.
    assert_problems(
        &with(pinned, &format!("      - uses: '{sha}'\n")),
        &[&format!(
            "line 13: `uses: {sha}` has no version comment (`# v1.2.3`) on its line"
        )],
    );
}

#[test]
fn a_matrix_include_the_lint_cannot_read_is_refused_and_a_static_one_passes() {
    // Catches: an `include` given as an expression, or one entry of it given as an expression,
    // adding a runner beside a hosted static list without the lint seeing it.
    let hosted = "    runs-on: ubuntu-latest\n";
    let matrix = |include: &str| {
        format!(
            "    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      matrix:\n        os: [ubuntu-latest]\n{include}"
        )
    };
    assert_problems(
        &with(
            hosted,
            &matrix("        include: ${{ fromJSON(vars.EXTRA_RUNNERS) }}\n"),
        ),
        &["job test: `strategy.matrix.include` is not a list the lint can read"],
    );
    assert_problems(
        &with(
            hosted,
            &matrix("        include:\n          - ${{ fromJSON(vars.EXTRA_RUNNER) }}\n"),
        ),
        &["job test: an entry of `strategy.matrix.include` is not a mapping the lint can read"],
    );
    assert_problems(
        &with(
            hosted,
            &matrix(
                "        include:\n          - os: ubuntu-24.04-arm\n            arch: arm64\n",
            ),
        ),
        &[],
    );
}

#[test]
fn the_deny_self_hosted_runners_flag_is_not_a_self_hosted_runner() {
    // Catches: the self-hosted rule refusing `gh attestation verify --deny-self-hosted-runners`,
    // which refuses self-hosted runners, and the word passing beside that flag.
    let step = "      - run: cargo test --workspace --locked\n";
    let flag = "      - run: gh attestation verify x.tar.gz --deny-self-hosted-runners\n";
    assert_problems(&with(step, flag), &[]);
    assert_problems(
        &with(
            step,
            "      - run: gh attestation verify x.tar.gz --deny-self-hosted-runners self-hosted\n",
        ),
        &["the document names self-hosted"],
    );
}
