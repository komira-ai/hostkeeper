//! The workflow lint: every file in `.github/workflows` is held to the rules in [`lint`], and
//! the lint is shown to refuse each defect by putting it into a workflow that is otherwise
//! clean ([`CLEAN`]) and reading the exact problems it reports.

mod lint;
mod yaml;

use std::path::PathBuf;

use lint::{HOSTED_RUNNERS, WRITE_ALLOWED, lint};

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
    // macOS job on pull_request, in any workflow file of this repository.
    let all = workflows();
    assert!(
        all.iter().any(|(name, _)| name == "ci.yml"),
        "ci.yml is missing"
    );
    let mut found = Vec::new();
    for (file, text) in all {
        for p in lint(&file, &text) {
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
        WRITE_ALLOWED.is_empty(),
        "no job of this repository writes yet"
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
            "      - run: cargo xtask sudo-test\n",
            "job test: privileged on pull_request (`cargo xtask sudo-test`)",
        ),
        (
            "      - run: |\n          cargo build\n          docker run --privileged image\n",
            "job test: privileged on pull_request (`docker run --privileged image`)",
        ),
    ];
    for (run, want) in cases {
        assert_problems(&with(step, run), &[want]);
    }
    let container = "    container:\n      image: rust:1\n      options: --cap-add NET_ADMIN\n    runs-on: ubuntu-latest\n";
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
    // refusing privileged or macOS jobs in a workflow that never runs on a pull request.
    let on = "on:\n  pull_request:\n  push:\n    branches: [main]\n";
    let mac = "    runs-on: macos-15\n";
    let want = ["job test: macOS on pull_request (`macos-15`)"];
    for shape in ["on: pull_request\n", "on: [push, pull_request]\n"] {
        let text = with_in(&with(on, shape), "    runs-on: ubuntu-latest\n", mac);
        assert_problems(&text, &want);
    }
    let push = push_only();
    let text = with_in(&push, "    runs-on: ubuntu-latest\n", mac);
    let text = with_in(
        &text,
        "      - run: cargo test --workspace --locked\n",
        "      - run: sudo -E cargo test\n",
    );
    assert_problems(&text, &[]);
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
