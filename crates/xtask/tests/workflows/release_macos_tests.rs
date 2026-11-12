//! The rules of the macOS stage of release.yml ([`super::release_macos`]): the Mac's tests run for
//! the newest release of a run, after the build, and hold back the Mac's installer alone. Each rule
//! is shown to refuse its defect by putting it into the committed file, which passes, and reading
//! the exact problems it reports.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::lint::{entries, entry, lint};
use super::privileged_tests::{job_shape_problems, macos_device_targets};
use super::release::{RELEASE_IF, release_rules};
use super::release_macos::{
    CHECKOUT_REF, DEVICE_JOB, FILE, JOBS, RUNNER, TEST_JOB, crate_problems, problems,
};
use super::yaml;
use super::{with_in, workflows};

/// The committed release.yml.
fn release() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == FILE)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{FILE} is in the repository"))
}

/// The problems of the stage in `text`.
fn stage(text: &str) -> Vec<String> {
    problems(&yaml::load(text).expect("release.yml parses"))
}

/// The committed file with `from` (once in it) replaced by `to`.
fn release_with(from: &str, to: &str) -> String {
    with_in(&release(), from, to)
}

/// The head of the job `name` up to its runner, once in the file.
fn head(name: &str) -> String {
    format!("  {name}:\n    needs: [plan, newest]\n    if: {RELEASE_IF}\n    runs-on: {RUNNER}\n")
}

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

#[test]
fn the_release_has_the_macos_stage_and_it_passes_every_rule() {
    // Catches: the Mac's tests missing from the release pipeline (they ran on every pull request
    // before, and a release shipped with no Mac test at all), a stage job renamed away, the stage
    // failing its own rules or any rule of the lint, and macos-test leaving out a crate the Mac's
    // binaries are built from (a crate added to `hostkeeper` or `hello` later).
    let text = release();
    let doc = yaml::load(&text).expect("release.yml parses");
    let jobs: Vec<&str> = entry(&doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .filter_map(|(n, _)| n.as_str())
        .collect();
    for job in JOBS {
        assert!(jobs.contains(&job), "release.yml has no job {job}");
    }
    assert_eq!(stage(&text), Vec::<String>::new());
    assert_eq!(lint(FILE, &text), Vec::<String>::new());
    assert_eq!(release_rules(FILE, &text), Vec::<String>::new());
    let crates = mac_crates();
    assert!(crates.contains("hostkeeper-launchd") && crates.contains("hostkeeper-daemon"));
    assert!(
        !crates.contains("hostkeeper-mdm-step"),
        "the MDM server is not the Mac's"
    );
    assert!(
        !crates.contains("hostkeeper-ci"),
        "the binary has no `ci` verb: CI runs hkctl"
    );
    assert_eq!(crate_problems(&text, &crates), Vec::<String>::new());
}

#[test]
fn macos_device_runs_the_macs_privileged_targets_with_one_sudo() {
    // Catches: the device job running other test targets than the files that start with the macOS
    // attribute (a new Mac device test left out, or a Linux one named), a second `sudo`, the
    // build run as root, and the build after the run.
    let targets = macos_device_targets();
    assert_eq!(
        targets,
        BTreeSet::from([
            "device_mac".to_owned(),
            "device_mac_e2e".to_owned(),
            "device_mac_selfupdate".to_owned(),
            "install_macos_pkg".to_owned()
        ])
    );
    let doc = yaml::load(&release()).expect("release.yml parses");
    let device = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, DEVICE_JOB))
        .expect("the device job");
    assert_eq!(
        job_shape_problems(device, &targets, None),
        Vec::<String>::new()
    );
    let fewer = BTreeSet::from(["device_mac".to_owned()]);
    let found = job_shape_problems(device, &fewer, None);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].starts_with("the build step names the test targets"),
        "{found:?}"
    );
}

#[test]
fn a_stage_job_on_another_runner_or_under_another_condition_is_refused() {
    // Catches: a stage that skips for a release it should test (another `if:`), waits for the
    // wrong jobs, runs on a runner that is not the listed macOS image or on Linux, or checks out
    // another commit than the release's.
    let test = head(TEST_JOB);
    let device = head(DEVICE_JOB);
    for (from, to, want) in [
        (
            test.clone(),
            test.replace("macos-15", "macos-14"),
            format!("release.yml: job {TEST_JOB} must run on {RUNNER}"),
        ),
        (
            test.clone(),
            test.replace("macos-15", "macos-latest"),
            format!("release.yml: job {TEST_JOB} must run on {RUNNER}"),
        ),
        (
            device.clone(),
            device.replace("macos-15", "ubuntu-24.04"),
            format!("release.yml: job {DEVICE_JOB} must run on {RUNNER}"),
        ),
        (
            device.clone(),
            device.replace("[plan, newest]", "[plan, newest, testing-pkg-upload]"),
            format!("release.yml: job {DEVICE_JOB} must need exactly [\"newest\", \"plan\"]"),
        ),
        (
            test.clone(),
            test.replace(RELEASE_IF, "${{ true }}"),
            format!("release.yml: job {TEST_JOB} needs the exact `if: {RELEASE_IF}`"),
        ),
    ] {
        let found = stage(&release_with(&from, &to));
        assert_eq!(found, [want], "{to}");
    }
    let step = format!(
        "          ref: {CHECKOUT_REF}\n          persist-credentials: false\n      # The system the tests run on"
    );
    let other = release_with(
        &step,
        "          persist-credentials: false\n      # The system the tests run on",
    );
    assert_eq!(
        stage(&other),
        [format!(
            "release.yml: job {TEST_JOB} must check out the commit of the release once (`ref: {CHECKOUT_REF}`)"
        )]
    );
}

#[test]
fn a_stage_job_with_a_write_scope_an_environment_or_a_secret_is_refused() {
    // Catches: the Mac's jobs, which run the repository's code on a macOS runner, given the rights
    // of a publishing job: a write scope, an environment (its secrets and cloud role), a secret or
    // the token.
    let test = head(TEST_JOB);
    let permissions = format!("{test}    permissions:\n      contents: read\n");
    for (added, want) in [
        (
            "    permissions:\n      contents: read\n      id-token: write\n",
            "must hold `contents: read` alone",
        ),
        (
            "    permissions:\n      contents: write\n",
            "must hold `contents: read` alone",
        ),
        (
            "    permissions:\n      contents: read\n      actions: read\n",
            "must hold `contents: read` alone",
        ),
        (
            "    permissions:\n      contents: read\n    environment: apt-publish\n",
            "must not have `environment`",
        ),
    ] {
        let found = stage(&release_with(&permissions, &format!("{test}{added}")));
        assert_eq!(
            found,
            [format!("release.yml: job {TEST_JOB} {want}")],
            "{added}"
        );
    }
    for token in ["${{ secrets.APT_GPG_PRIVATE_KEY }}", "${{ github.token }}"] {
        let found = stage(&release_with(
            &permissions,
            &format!("{permissions}    env:\n      KEY: {token}\n"),
        ));
        assert_eq!(
            found,
            [format!(
                "release.yml: job {TEST_JOB} names a secret or the token (`{token}`)"
            )],
            "{token}"
        );
    }
}

#[test]
fn root_in_release_yml_is_the_device_job_alone() {
    // Catches: the root exception of the device job reaching its neighbour: a `sudo` in the test
    // job, or in any other job of the release, is refused as root outside privileged.yml.
    let run = "        run: cargo test --locked --no-fail-fast -p hello";
    let text = release_with(run, &run.replace("run: cargo", "run: sudo cargo"));
    let found = lint(FILE, &text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with(&format!(
            "job {TEST_JOB}: privileged outside privileged.yml and net-install.yml (`sudo cargo test"
        )),
        "{found:?}"
    );
    let other = release_with(
        "      - name: Compile xtask\n        run: cargo xtask --help\n      - name: Decide whether the release in testing is promoted\n",
        "      - name: Compile xtask\n        run: sudo cargo xtask --help\n      - name: Decide whether the release in testing is promoted\n",
    );
    let found = lint(FILE, &other);
    assert!(
        found
            .iter()
            .any(|p| p.starts_with("job promote-check: privileged outside privileged.yml")),
        "{found:?}"
    );
}

#[test]
fn only_promote_pkg_waits_for_the_macos_stage() {
    // Catches: the Mac holding the release hostage: a job other than promote-pkg that waits for a
    // stage job (so a Mac that is red or a macOS runner that never comes would hold the apt and
    // rpm promotions, the coordinator or the desired version), and promote-pkg that stops waiting
    // for the stage or runs when it did not succeed.
    let check = "    needs: [plan, newest, testing-apt-write, testing-apt-write-2, testing-apt-write-3, testing-rpm-write, testing-rpm-write-2, testing-rpm-write-3, testing-pkg-upload, check-apt, check-rpm, check-pkg]\n";
    let waiting = check.replace("check-pkg]", "check-pkg, macos-test]");
    assert_eq!(
        stage(&release_with(check, &waiting)),
        [
            "release.yml: job promote-check waits for macos-test; only promote-pkg may wait for the macOS stage"
        ]
    );
    let promote =
        "    needs: [plan, newest, promote-check, testing-pkg-upload, macos-test, macos-device]\n";
    let found = release_rules(
        FILE,
        &release_with(
            promote,
            "    needs: [plan, newest, promote-check, testing-pkg-upload]\n",
        ),
    );
    assert!(
        found
            .iter()
            .any(|p| p.starts_with("release.yml: job promote-pkg needs")),
        "{found:?}"
    );
    let only_one =
        "needs.macos-test.result == 'success' && needs.macos-device.result == 'success' }}";
    let found = release_rules(
        FILE,
        &release_with(only_one, "needs.macos-test.result == 'success' }}"),
    );
    assert!(
        found
            .iter()
            .any(|p| p.starts_with("release.yml: job promote-pkg needs the exact `if:")),
        "{found:?}"
    );
    let ignored = release_with(only_one, "true }}");
    let found = release_rules(FILE, &ignored);
    assert!(
        found
            .iter()
            .any(|p| p.starts_with("release.yml: job promote-pkg needs the exact `if:")),
        "{found:?}"
    );
}

#[test]
fn macos_test_must_test_exactly_the_macs_crates() {
    // Catches: a crate of the Mac left out of the build or the run, a crate added that does not
    // build for macOS (the MDM server), `--workspace` or `--exclude` in place of the list, a run
    // that stops at the first failure, and a build or run step missing or doubled.
    let crates = mac_crates();
    let release = release();
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
            crate_problems(&with_in(&release, from, to), &crates),
            wants,
            "{to}"
        );
    }
    // The run without `--no-fail-fast` is no run step; a second build step is one too many.
    assert_eq!(
        crate_problems(
            &with_in(
                &release,
                "--locked --no-fail-fast -p hello ",
                "--locked -p hello "
            ),
            &crates
        ),
        [format!("job macos-test: 0 steps `{run} ...`, not one")]
    );
    let step = "      - name: Run the tests of the Mac's crates\n";
    let doubled = with_in(
        &release,
        step,
        &format!("      - run: {build} -p hello\n{step}"),
    );
    assert_eq!(
        crate_problems(&doubled, &crates),
        [
            format!("job macos-test: 2 steps `{build} ...`, not one"),
            "job macos-test: 3 `cargo test` steps, not two".to_owned(),
        ]
    );
    // Without the job, or with a file that does not parse.
    let renamed = release.replace("\n  macos-test:\n", "\n  macos-gone:\n");
    assert_eq!(crate_problems(&renamed, &crates).len(), 3);
    assert_eq!(crate_problems("on: [unclosed\n", &crates).len(), 1);
}
