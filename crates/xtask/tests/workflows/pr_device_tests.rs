//! The rules of `pr-device.yml` ([`super::pr_device`]) and the pull-request rules of
//! [`super::lint`] that it is the one exception to. Each rule is shown to refuse its defect by
//! putting it into the committed file, which passes, and reading the exact problems it reports.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::lint::{PRIVILEGED_WORKFLOW, entries, entry, lint};
use super::pr_device::{
    CONCURRENCY_PROBLEM, FILE, GROUP, JOBS, ON_PROBLEM, TOP_LEVEL, crate_problems, mirror_problems,
    same,
};
use super::yaml::{self, Node};
use super::{CLEAN, with, with_in, workflows};

/// The committed text of `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
}

fn device() -> String {
    committed(FILE)
}

fn problems(text: &str) -> Vec<String> {
    lint(FILE, text)
}

/// The committed file with `from` (once in it) replaced by `to`.
fn device_with(from: &str, to: &str) -> String {
    with_in(&device(), from, to)
}

/// The start of the job `macos-test`, once in the file.
const MACOS_TEST: &str = "  macos-test:\n    runs-on: macos-15\n";

/// The directories of the workspace crates the manifest `manifest` of the crate in `dir` depends on,
/// through `[dependencies]` and `[build-dependencies]`, on any target (`[target.<cfg>.…]`): a
/// `path` of its own, from `dir`, or `workspace = true` with a `path` in the root's
/// `[workspace.dependencies]` (`workspace`), from `root`. A renamed dependency (`package = ...`)
/// is found by its path all the same.
fn path_dependencies(
    manifest: &toml::Table,
    dir: &Path,
    workspace: &toml::Table,
    root: &Path,
) -> Vec<PathBuf> {
    let kinds = ["dependencies", "build-dependencies"];
    let mut tables: Vec<&toml::Table> = kinds
        .iter()
        .filter_map(|k| manifest.get(*k).and_then(toml::Value::as_table))
        .collect();
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            tables.extend(
                kinds
                    .iter()
                    .filter_map(|k| target.get(*k).and_then(toml::Value::as_table)),
            );
        }
    }
    let mut found = Vec::new();
    for (name, dependency) in tables.into_iter().flatten() {
        if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
            found.push(dir.join(path));
        } else if dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
            let declared = workspace.get(name);
            if let Some(path) = declared
                .and_then(|d| d.get("path"))
                .and_then(toml::Value::as_str)
            {
                found.push(root.join(path));
            }
        }
    }
    found
}

/// The manifest of the crate (or workspace root) in `dir`.
fn manifest(dir: &Path) -> toml::Table {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).expect("read a manifest");
    text.parse().expect("a manifest parses")
}

/// The package name of the crate in `dir`.
fn package_name(dir: &Path) -> String {
    manifest(dir)["package"]["name"]
        .as_str()
        .expect("a package name")
        .to_owned()
}

/// The crates the `hostkeeper` and `hello` binaries are built from: the two crates and every
/// workspace crate they depend on, through normal and build dependencies, on any target.
fn mac_crates() -> BTreeSet<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = manifest(&root)["workspace"]["dependencies"]
        .as_table()
        .expect("the root's [workspace.dependencies]")
        .clone();
    let mut pending = vec![root.join("crates/hostkeeper"), root.join("examples/hello")];
    let mut seen = BTreeSet::new();
    while let Some(dir) = pending.pop() {
        if seen.insert(package_name(&dir)) {
            pending.extend(path_dependencies(&manifest(&dir), &dir, &workspace, &root));
        }
    }
    seen
}

#[test]
fn the_committed_pr_device_yml_passes_and_its_jobs_are_the_listed_ones() {
    // Catches: the workflow failing a rule; a job of it that is not in the lint's list (or one of
    // the list missing); a copy that drifted from privileged.yml; and macos-test leaving out a
    // crate the Mac's binaries are built from (a crate added to `hostkeeper` or `hello` later).
    let text = device();
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_eq!(
        mirror_problems(&text, &committed(PRIVILEGED_WORKFLOW)),
        Vec::<String>::new()
    );
    let crates = mac_crates();
    assert!(crates.contains("hostkeeper-launchd") && crates.contains("hostkeeper-daemon"));
    assert!(
        !crates.contains("hostkeeper-mdm-step"),
        "the MDM server is not the Mac's"
    );
    assert_eq!(crate_problems(&text, &crates), Vec::<String>::new());
    let doc = yaml::load(&text).expect("pr-device.yml parses");
    assert_eq!(
        entry(&doc, "name").and_then(Node::as_str),
        Some("pr-device")
    );
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let names: Vec<&str> = jobs.iter().filter_map(|(n, _)| n.as_str()).collect();
    let listed: Vec<&str> = JOBS.iter().map(|j| j.name).collect();
    assert_eq!(names, listed);
}

#[test]
fn a_macos_job_with_a_secret_on_a_pull_request_is_refused() {
    // Catches: a secret or the token handed to a macOS job of a pull request, as an expression in
    // its env, a step's `with:` or a run line, in any case, through `toJSON(secrets)`, or passed
    // with a `secrets:` key; and a secret set once for every job beside `jobs`.
    let cases: [(&str, &str); 9] = [
        (
            "    env:\n      TOKEN: ${{ secrets.DEPLOY_KEY }}\n",
            "job macos-test: a secret on pull_request (`${{ secrets.DEPLOY_KEY }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github.token }}\n",
            "job macos-test: a secret on pull_request (`${{ github.token }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ SECRETS['DEPLOY_KEY'] }}\n",
            "job macos-test: a secret on pull_request (`${{ SECRETS['DEPLOY_KEY'] }}`)",
        ),
        (
            "    env:\n      ALL: ${{ toJSON(secrets) }}\n",
            "job macos-test: a secret on pull_request (`${{ toJSON(secrets) }}`)",
        ),
        (
            "    env:\n      TOKEN: a-${{ GitHub.Token }}-b\n",
            "job macos-test: a secret on pull_request (`a-${{ GitHub.Token }}-b`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github['token'] }}\n",
            "job macos-test: a secret on pull_request (`${{ github['token'] }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github [ \"token\" ] }}\n",
            "job macos-test: a secret on pull_request (`${{ github [ \"token\" ] }}`)",
        ),
        (
            "    env:\n      ALL: ${{ toJSON( github ) }}\n",
            "job macos-test: a secret on pull_request (`${{ toJSON( github ) }}`)",
        ),
        (
            "    secrets:\n      key: x\n",
            "job macos-test: a secret on pull_request (`secrets`)",
        ),
    ];
    for (added, want) in cases {
        let text = device_with(MACOS_TEST, &format!("{MACOS_TEST}{added}"));
        assert_eq!(problems(&text), [want], "{added}");
    }
    // In the device job, in a step's `with:`.
    let text = device_with(
        "          persist-credentials: false\n      # The system the tests run against",
        "          persist-credentials: false\n          token: ${{ secrets.GITHUB_TOKEN }}\n      # The system the tests run against",
    );
    assert_eq!(
        problems(&text),
        ["job macos-device: a secret on pull_request (`${{ secrets.GITHUB_TOKEN }}`)"]
    );
    // Beside `jobs`, for every job.
    let text = device_with(
        "  CARGO_TERM_COLOR: always\n",
        "  CARGO_TERM_COLOR: always\n  KEY: ${{ secrets.KEY }}\n",
    );
    assert_eq!(
        problems(&text),
        ["workflow: a secret on pull_request (`${{ secrets.KEY }}`)"]
    );
}

#[test]
fn the_secret_rule_holds_in_every_pull_request_workflow_and_reads_only_expressions() {
    // Catches: the rule held to pr-device.yml alone, so ci.yml could hand its jobs a secret; a
    // step name or text that says "secrets" outside an expression refused; and the rule holding
    // where no pull request runs (a push-only workflow may use the token).
    let step = "      - run: cargo test --workspace --locked\n";
    let secret = "      - run: echo ${{ secrets.X }}\n";
    assert_eq!(
        lint("ci.yml", &with(step, secret)),
        ["job test: a secret on pull_request (`echo ${{ secrets.X }}`)"]
    );
    assert_eq!(
        lint("ci.yml", &with_in(&super::push_only(), step, secret)),
        Vec::<String>::new()
    );
    for innocent in [
        "      - name: Hold no secrets here\n        run: cargo test\n",
        "      - run: cargo test secrets_are_redacted\n",
        "      - run: echo ${{ github.sha }} ${{ github.token_kind }}x\n",
        "      - run: echo ${{ inputs.secretsless }}\n",
        "      - run: echo ${{ github['sha'] }} ${{ toJSON(github.event.pull_request.labels) }}\n",
    ] {
        assert_eq!(
            lint("ci.yml", &with(step, innocent)),
            Vec::<String>::new(),
            "{innocent}"
        );
    }
    // An expression left open runs to the end of the value, and still counts.
    assert_eq!(
        lint("ci.yml", &with(step, "      - run: echo ${{ secrets.X\n")),
        ["job test: a secret on pull_request (`echo ${{ secrets.X`)"]
    );
}

#[test]
fn a_write_scope_or_id_token_on_a_pr_device_job_is_refused() {
    // Catches: an OIDC token or a write scope granted to a job that runs pull-request code as
    // root on a macOS runner, which the exception for root and macOS must not let through.
    let read = "    runs-on: macos-15\n    permissions:\n      contents: read\n    # A bound on the whole job, against a test that hangs; no test depends on it.\n    timeout-minutes: 45\n";
    for (scope, wants) in [
        (
            "id-token",
            "job macos-device: `id-token: write` is not allowed in pr-device.yml",
        ),
        (
            "contents",
            "job macos-device: `contents: write` is not allowed in pr-device.yml",
        ),
    ] {
        let text = device_with(
            read,
            &read.replace("contents: read", &format!("{scope}: write")),
        );
        assert_eq!(
            problems(&text),
            [
                wants.to_owned(),
                format!("job macos-device: privileged on pull_request (`{scope}: write`)"),
            ]
        );
    }
}

#[test]
fn root_and_macos_on_a_pull_request_are_allowed_only_in_the_jobs_listed_for_them() {
    // Catches: the exception reaching a job it does not name: root in the macOS test job, a macOS
    // runner for the Linux copy, and a job named like a listed one in another workflow (ci.yml),
    // which is held to the rule in full.
    let run = "        run: cargo test --locked --no-fail-fast -p hello";
    let text = device_with(run, &run.replace("run: cargo", "run: sudo cargo"));
    let found = problems(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("job macos-test: privileged on pull_request (`sudo cargo test"),
        "{found:?}"
    );
    let linux = "    runs-on: ubuntu-24.04\n";
    let text = device_with(linux, "    runs-on: macos-15\n");
    assert_eq!(
        problems(&text),
        [
            "job privileged: macOS on pull_request (`macos-15`)",
            "job privileged: runs on a runner outside [\"ubuntu-24.04\"]",
        ]
    );
    let in_ci = with(
        "  test:\n    runs-on: ubuntu-latest\n",
        "  macos-device:\n    runs-on: macos-15\n",
    );
    let in_ci = with_in(
        &in_ci,
        "      - run: cargo test --workspace --locked\n",
        "      - run: sudo cargo test\n",
    );
    assert_eq!(
        lint("ci.yml", &in_ci),
        [
            "job macos-device: privileged on pull_request (`sudo cargo test`)",
            "job macos-device: macOS on pull_request (`macos-15`)",
        ]
    );
}

#[test]
fn the_trigger_of_pr_device_yml_is_pull_request_alone_with_no_filter() {
    // Catches: a paths or branches filter on `on`, which leaves the required checks unreported on
    // a pull request it filters out (a check that never starts), a push trigger that would run the copies again after the merge,
    // and a `types` list that skips the pushes to an open pull request.
    let on = "on:\n  pull_request:\n\npermissions";
    for changed in [
        "on:\n  pull_request:\n    paths: ['crates/hostkeeper-launchd/**']\n\npermissions",
        "on:\n  pull_request:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n    types: [opened]\n\npermissions",
        "on:\n  pull_request:\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  workflow_dispatch:\n\npermissions",
        "on:\n  pull_request: x\n\npermissions",
        "on:\n  pull_request: [x]\n\npermissions",
        "on: [pull_request, push]\n\npermissions",
    ] {
        let found = problems(&device_with(on, changed));
        assert!(
            found.contains(&ON_PROBLEM.to_owned()),
            "{changed}: {found:?}"
        );
    }
    for same in [
        "on: pull_request\n\npermissions",
        "on: [pull_request]\n\npermissions",
        "on:\n  pull_request: {}\n\npermissions",
    ] {
        assert_eq!(
            problems(&device_with(on, same)),
            Vec::<String>::new(),
            "{same}"
        );
    }
    let doc = yaml::load(&device()).expect("parses");
    assert!(entry(&doc, "on").is_some());
    // With no `on` it runs for no pull request, and its copies' root is refused as outside
    // privileged.yml.
    let none = problems(&device_with("on:\n  pull_request:\n\n", ""));
    assert_eq!(none.len(), 3, "{none:?}");
    assert!(none[0].starts_with("job macos-device: privileged outside privileged.yml"));
    assert!(none[1].starts_with("job privileged: privileged outside privileged.yml"));
    assert_eq!(none[2], ON_PROBLEM);
}

#[test]
fn the_concurrency_of_pr_device_yml_is_one_group_per_pull_request_that_cancels() {
    // Catches: superseded runs left running (and holding macOS runners) after a newer push, a
    // group shared by every pull request (which cancels another pull request's run), and a
    // workflow with no concurrency at all.
    let block = format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: true\n");
    assert!(device().contains(&block));
    for changed in [
        format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: false\n"),
        "concurrency:\n  group: pr-device\n  cancel-in-progress: true\n".to_owned(),
        format!("concurrency:\n  group: {GROUP}\n"),
        format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: true\n  x: y\n"),
        "concurrency: pr-device\n".to_owned(),
        String::new(),
    ] {
        assert_eq!(
            problems(&device_with(&block, &changed)),
            [CONCURRENCY_PROBLEM],
            "{changed}"
        );
    }
}

#[test]
fn each_job_of_pr_device_yml_runs_on_every_pull_request_on_its_runner() {
    // Catches: a job that a condition or another job's result can skip (a required check that
    // then passes, or never reports, without its tests having run), a job the list does not name,
    // one of the list missing, and a job on another runner.
    let cases: [(&str, &str, &str); 5] = [
        (
            MACOS_TEST,
            "  macos-test:\n    if: ${{ github.event.pull_request.draft == false }}\n    runs-on: macos-15\n",
            "job macos-test: has `if`, and must run and report on every pull request",
        ),
        (
            MACOS_TEST,
            "  macos-test:\n    needs: privileged\n    runs-on: macos-15\n",
            "job macos-test: has `needs`, and must run and report on every pull request",
        ),
        (
            MACOS_TEST,
            "  macos-test:\n    runs-on: macos-14\n",
            "job macos-test: runs on a runner outside [\"macos-15\"]",
        ),
        (
            MACOS_TEST,
            "  macos-test:\n    runs-on: [macos-15]\n",
            "job macos-test: runs on a runner outside [\"macos-15\"]",
        ),
        (
            "jobs:\n",
            "jobs:\n  extra:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: cargo test\n",
            "job extra is not a job of pr-device.yml",
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(problems(&device_with(from, to)), [want], "{to}");
    }
    // A job of the list missing: privileged, the last, removed with every line after its start.
    let text = device();
    let at = text
        .find("  # privileged.yml's job privileged;")
        .expect("the copy");
    assert_eq!(problems(&text[..at]), ["no job privileged"]);
}

#[test]
fn macos_test_must_test_exactly_the_macs_crates() {
    // Catches: a crate of the Mac left out of the build or the run, a crate added that does not
    // build for macOS (the MDM server), `--workspace` or `--exclude` in place of the list, a run
    // that stops at the first failure, and a build or run step missing or doubled.
    let crates = mac_crates();
    let want = |start: &str| {
        format!(
            "job macos-test: `{start}` must name exactly the crates {crates:?} with -p, and nothing else"
        )
    };
    let build = "cargo test --locked --no-run";
    let run = "cargo test --locked --no-fail-fast";
    let cases: [(&str, &str, Vec<String>); 4] = [
        ("--no-run -p hello ", "--no-run ", vec![want(build)]),
        (
            "--no-fail-fast -p hello ",
            "--no-fail-fast -p hostkeeper-mdm-step -p hello ",
            vec![want(run)],
        ),
        (
            "--no-fail-fast -p hello ",
            "--no-fail-fast --workspace -p hello ",
            vec![want(run)],
        ),
        (
            "--no-fail-fast -p hello ",
            "--no-fail-fast --exclude hostkeeper-mdm-step -p hello ",
            vec![want(run)],
        ),
    ];
    for (from, to, wants) in cases {
        assert_eq!(
            crate_problems(&device_with(from, to), &crates),
            wants,
            "{to}"
        );
    }
    // The run without `--no-fail-fast` is no run step; a second build step is one too many.
    assert_eq!(
        crate_problems(
            &device_with("--locked --no-fail-fast -p hello ", "--locked -p hello "),
            &crates
        ),
        [format!("job macos-test: 0 steps `{run} ...`, not one")]
    );
    let step = "      - name: Run the tests of the Mac's crates\n";
    let doubled = device_with(step, &format!("      - run: {build} -p hello\n{step}"));
    assert_eq!(
        crate_problems(&doubled, &crates),
        [
            format!("job macos-test: 2 steps `{build} ...`, not one"),
            "job macos-test: 3 `cargo test` steps, not two".to_owned(),
        ]
    );
    // Without the job, or with a file that does not parse.
    let at = device().find(MACOS_TEST).expect("the job");
    let end = device()
        .find("  # privileged.yml's job macos-device;")
        .expect("the copy");
    let without = format!("{}{}", &device()[..at], &device()[end..]);
    assert_eq!(crate_problems(&without, &crates).len(), 3);
    assert_eq!(crate_problems("on: [unclosed\n", &crates).len(), 1);
}

#[test]
fn a_copy_that_drifted_from_privileged_yml_is_refused() {
    // Catches: a pull request running other device tests than run after the merge: a step, a
    // runner, an env or a test target changed in the copy or in privileged.yml alone, a copy
    // missing, and a top-level env (RUSTFLAGS) that differs.
    let privileged = committed(PRIVILEGED_WORKFLOW);
    let drift = |job: &str| format!("job {job}: not privileged.yml's job {job}, key for key");
    let device_mac = "--test device_mac --test device_mac_e2e --test device_mac_selfupdate --test self_update_across_releases --test unpack_mac -- --test-threads=1 --nocapture";
    let changed = device_with(
        device_mac,
        "--test device_mac -- --test-threads=1 --nocapture",
    );
    assert_eq!(
        mirror_problems(&changed, &privileged),
        [drift("macos-device")]
    );
    let linux = "    timeout-minutes: 60\n    env:\n      GH_VERSION: 2.102.1\n";
    let mut copy = device_with(linux, &linux.replace("2.102.1", "2.102.2"));
    assert_eq!(mirror_problems(&copy, &privileged), [drift("privileged")]);
    let original = with_in(&privileged, linux, &linux.replace("2.102.1", "2.102.2"));
    assert_eq!(mirror_problems(&copy, &original), Vec::<String>::new());
    copy = device_with("  RUSTFLAGS: -D warnings\n", "");
    assert_eq!(
        mirror_problems(&copy, &privileged),
        ["pr-device.yml: its top-level env is not privileged.yml's"]
    );
    let no_env = |text: &str| {
        let at = text.find("\nenv:\n").expect("env");
        let end = text[at + 1..].find("\n\n").expect("end of env") + at + 1;
        format!("{}{}", &text[..at + 1], &text[end + 1..])
    };
    assert_eq!(
        mirror_problems(&no_env(&device()), &no_env(&privileged)),
        Vec::<String>::new()
    );
    assert_eq!(
        mirror_problems(&no_env(&device()), &privileged),
        ["pr-device.yml: its top-level env is not privileged.yml's"]
    );
    let at = device()
        .find("  # privileged.yml's job privileged;")
        .expect("the copy");
    let without = device()[..at].to_owned();
    assert_eq!(
        mirror_problems(&without, &privileged),
        [drift("privileged")]
    );
    assert_eq!(
        mirror_problems("on: [unclosed\n", &privileged).len(),
        1,
        "a file that does not parse is one problem"
    );
    assert_eq!(mirror_problems(&device(), "").len(), 1);
    // A copy with an `if:` added is no copy either (and `problems` refuses the `if:` itself).
    let copy = device_with(
        "  macos-device:\n    runs-on: macos-15\n",
        "  macos-device:\n    if: ${{ always() }}\n    runs-on: macos-15\n",
    );
    assert_eq!(mirror_problems(&copy, &privileged), [drift("macos-device")]);
}

#[test]
fn same_compares_trees_not_where_they_were_written() {
    // Catches: a comparison that passes a quoted scalar for a plain one, a list for a mapping,
    // a shorter list or mapping, or one that fails two equal trees written on other lines.
    let load = |text: &str| yaml::load(text).expect("parses");
    assert!(same(&load("a: [1, 2]\n"), &load("\n\na:\n  - 1\n  - 2\n")));
    for (a, b) in [
        ("a: 1\n", "a: '1'\n"),
        ("a: [1]\n", "a: {b: 1}\n"),
        ("a: [1, 2]\n", "a: [1]\n"),
        ("a: 1\nb: 2\n", "a: 1\n"),
        ("a: 1\n", "b: 1\n"),
        ("a: 1\n", "a: 2\n"),
    ] {
        assert!(!same(&load(a), &load(b)), "{a} against {b}");
    }
    assert!(CLEAN.contains("pull_request"));
}

#[test]
fn a_top_level_key_that_would_change_the_copies_is_refused() {
    // Catches: a top-level `defaults` (a shell or a working directory for every step), which
    // changes what the copies of privileged.yml's jobs run while their keys still compare equal.
    let at = "\npermissions: {}\n";
    for (added, key) in [
        (
            "defaults:\n  run:\n    working-directory: crates\n",
            "defaults",
        ),
        ("run-name: x\n", "run-name"),
    ] {
        assert_eq!(
            problems(&device_with(at, &format!("{at}{added}"))),
            [format!(
                "pr-device.yml: the top-level key `{key}` is not one of {TOP_LEVEL:?}"
            )],
            "{added}"
        );
    }
}

#[test]
fn the_macs_crates_are_found_through_every_shape_of_path_dependency() {
    // Catches: a crate of the Mac's binaries missed by the list's own check, so macos-test stops
    // testing it with the check still green, when it is a build dependency, a dependency of one
    // target only, a renamed one, or one declared through the root's workspace dependencies; and
    // a dev-dependency, or an external crate, counted as one of the Mac's crates.
    let manifest: toml::Table = r#"
[dependencies]
a = { path = "../a" }
external = "1"
[build-dependencies]
b = { path = "../b" }
[target.'cfg(target_os = "macos")'.dependencies]
c = { path = "../c" }
[target.'cfg(unix)'.build-dependencies]
d = { path = "../d" }
renamed = { package = "e", path = "../e" }
[dependencies.f]
workspace = true
[dependencies.g]
workspace = true
[dev-dependencies]
h = { path = "../h" }
"#
    .parse()
    .expect("parses");
    let workspace: toml::Table = r#"
f = { path = "crates/f" }
g = "2"
"#
    .parse()
    .expect("parses");
    let found: BTreeSet<PathBuf> = path_dependencies(
        &manifest,
        Path::new("crates/x"),
        &workspace,
        Path::new("root"),
    )
    .into_iter()
    .collect();
    let want: BTreeSet<PathBuf> = [
        "crates/x/../a",
        "crates/x/../b",
        "crates/x/../c",
        "crates/x/../d",
        "crates/x/../e",
        "root/crates/f",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    assert_eq!(found, want);
}
