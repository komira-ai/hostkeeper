//! The job `targets` of the repository's `release.yml`, run for real: each test reads the
//! committed job, runs its `run:` steps in order with `bash -eo pipefail` (the shell GitHub runs
//! a `run:` step with), and reads the outputs the job gives `set-desired`. Nothing is
//! substituted in a command. The step `env` expressions are given the values a run of main gives
//! them, and an expression the test does not know fails the test; a step with an `if:` fails it
//! too (the job has none). The actions of the job are done by the test: the checkout of the
//! newest release's commit (any other `ref` fails the test); the test runs in this repository's
//! own build, so after that checkout a call of xtask runs only if the command line of the oldest
//! release that commit can be ([`super::release_ref::FLOOR`]) accepts it, and the step fails as
//! that binary would otherwise; and the download of the run's own `release-<tag>` artifact, whose
//! files it copies into the step's `path`. Each artifact holds what `release targets` reads of a
//! release: its native packages (a `.deb` per Linux architecture and the installer pkg), each with
//! its bundle; no index and no archive.
//!
//! `gh`, `cargo` and `rustup` on the `PATH` are the test program `tests/bin/gh_fixture.rs`,
//! which answers from scratch files, so no test reaches GitHub; as `cargo` it runs this crate's
//! real `xtask` binary, so the step that writes the targets runs the real `release targets`.
//! `jq` is the runner's own, as in the workflow. No `curl` is on the `PATH`: the job reads no
//! release channel, so a step that did would fail.
//!
//! What is asserted is what decides whether hostkeeper's desired version is set, and with what:
//! which step fails, and the outputs `version`, `targets` and `newest`. `set-desired` runs only
//! when the job succeeds and `newest` is `true` (its `if:`, [`SET_IF`], which the lint holds).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::desired::{SET_IF, TARGETS_JOB};
use super::lint::{entries, entry};
use super::release_files::{self, REPO, sorted};
use super::release_ref::{self, SURFACE_FILE, floor_problem};
use super::testdir::TestDir;
use super::{workflows, yaml};

const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");
const XTASK: &str = env!("CARGO_BIN_EXE_xtask");

/// The commit the release `version` of the plan's matrix is built from: one of its own for each
/// version, its digits padded to 40.
fn commit_of(version: &str) -> String {
    format!("{:0>40}", version.replace('.', ""))
}

/// The files of hostkeeper's release `version` that the job reads, by name: its native packages
/// (its `.deb`s and its installer) with their bundles.
fn hostkeeper_files(version: &str) -> BTreeMap<String, Vec<u8>> {
    release_files::native_files("hostkeeper", version)
}

/// The targets the job must give for hostkeeper's release `version`: each Linux platform's
/// native entry alone (docs/design/packages-native.md section 10.5, A24).
fn targets(version: &str) -> String {
    release_files::targets("hostkeeper", version, &hostkeeper_files(version))
}

/// One run of the job: the scratch files, and what the run is given.
struct Site {
    root: TestDir,
    /// The plan's matrix, oldest first: `(version, tag)`.
    matrix: Vec<(String, String)>,
}

impl Site {
    /// A run whose plan released `versions`, oldest first, while `published` are the tags of
    /// the repository's published releases (the run's own among them).
    fn new(name: &str, versions: &[&str], published: &[&str]) -> Site {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("targets-run");
        let root = TestDir::under(&base, name);
        for dir in ["bin/releases", "work", "temp", "artifacts"] {
            std::fs::create_dir_all(root.join(dir)).expect("make a scratch directory");
        }
        // Hard links, not copies (as in hello_run_tests.rs): the fake finds its data beside
        // the name it was started by, and acts as cargo or rustup when started by that name. As
        // cargo it runs the real xtask linked beside it.
        for name in ["gh", "cargo", "rustup"] {
            std::fs::hard_link(GH, root.join("bin").join(name)).expect("install a fake");
        }
        std::fs::hard_link(XTASK, root.join("bin/xtask")).expect("install xtask");
        let list: Vec<serde_json::Value> = published
            .iter()
            .map(|tag| serde_json::json!({ "tagName": tag }))
            .collect();
        let releases = root.join("bin/releases").join(REPO);
        std::fs::create_dir_all(&releases).expect("make the releases directory");
        std::fs::write(
            releases.join("release-list.json"),
            serde_json::to_vec(&list).expect("list"),
        )
        .expect("write the release list");
        let matrix = versions
            .iter()
            .map(|v| ((*v).to_owned(), format!("v{v}")))
            .collect();
        let site = Site { root, matrix };
        for v in versions {
            site.publish(v);
        }
        site
    }

    /// The run's artifact `release-v<version>` holds the files of that release.
    fn publish(&self, version: &str) {
        let dir = self
            .root
            .join("artifacts")
            .join(format!("release-v{version}"));
        std::fs::create_dir_all(&dir).expect("make the artifact");
        for (name, bytes) in hostkeeper_files(version) {
            std::fs::write(dir.join(name), bytes).expect("write a file of the release");
        }
    }

    /// Takes `name` out of the artifact of `version`, as a release missing that file.
    fn withhold(&self, version: &str, name: &str) {
        let path = self
            .root
            .join("artifacts")
            .join(format!("release-v{version}"))
            .join(name);
        std::fs::remove_file(path).expect("remove a file of the artifact");
    }

    fn matrix_json(&self) -> String {
        let include: Vec<serde_json::Value> = self
            .matrix
            .iter()
            .map(|(version, tag)| {
                json!({"version": version, "tag": tag, "commit": commit_of(version)})
            })
            .collect();
        json!({ "include": include }).to_string()
    }
}

/// How a run of the job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Every step passed: the job's outputs.
    Passed(BTreeMap<String, String>),
    /// The step of this name failed; the steps after it did not run.
    Failed(String),
}

impl Ended {
    /// Whether `set-desired` runs after this: the job passed and said `newest=true`.
    fn sets_desired(&self) -> bool {
        matches!(self, Ended::Passed(outputs) if outputs.get("newest").map(String::as_str) == Some("true"))
    }
}

/// The value of the expression `${{ expr }}` in a run of `site`, given the outputs of the steps
/// so far.
fn evaluate(
    site: &Site,
    steps: &BTreeMap<String, BTreeMap<String, String>>,
    value: &str,
) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find("${{ ") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let end = after.find(" }}").expect("an expression ends with ` }}`");
        let expr = &after[..end];
        let resolved = match expr.split('.').collect::<Vec<_>>().as_slice() {
            ["needs", "plan", "outputs", "matrix"] => site.matrix_json(),
            ["github", "token"] => "fixture-token".to_owned(),
            ["steps", id, "outputs", key] => steps
                .get(*id)
                .and_then(|o| o.get(*key))
                .cloned()
                .unwrap_or_default(),
            _ => panic!("the test does not know the expression `{expr}`"),
        };
        out.push_str(&resolved);
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    out
}

/// The `key=value` lines a step appended to its output file.
fn read_outputs(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The repository's `release.yml`.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// Runs the committed job `targets` against `site`.
fn run(site: &Site) -> Ended {
    run_text(site, &release_yml())
}

/// Runs the job `targets` of the workflow `text` against `site`.
fn run_text(site: &Site, text: &str) -> Ended {
    let doc = yaml::load(text).expect("release.yml parses");
    let job = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, TARGETS_JOB))
        .expect("release.yml has the job targets");
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(yaml::Value::Seq(items)) => items,
        _ => panic!("the job targets has no steps"),
    };
    let work = site.root.join("work");
    let path = format!("{}:/usr/bin:/bin", site.root.join("bin").display());
    let mut outputs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // Whether the release's commit was checked out.
    let mut release_checkout = false;
    for (at, step) in steps.iter().enumerate() {
        let text_of = |node: &yaml::Node, key: &str| {
            entry(node, key)
                .and_then(yaml::Node::as_str)
                .map(str::to_owned)
        };
        let name = text_of(step, "name").unwrap_or_else(|| format!("step {}", at + 1));
        if let Some(condition) = text_of(step, "if") {
            panic!("the test does not know the condition `{condition}` of {name}");
        }
        if let Some(uses) = text_of(step, "uses") {
            let with = entry(step, "with").expect("an action of the job has `with`");
            if uses.starts_with("actions/checkout@") {
                let r = text_of(with, "ref").expect("a checkout of the release's commit");
                let (newest, _) = site.matrix.last().expect("a release");
                assert_eq!(
                    evaluate(site, &outputs, &r),
                    commit_of(newest),
                    "a checkout of a ref is of the newest release's commit"
                );
                release_checkout = true;
                continue;
            }
            assert!(uses.starts_with("actions/download-artifact@"), "{uses}");
            let artifact = evaluate(site, &outputs, &text_of(with, "name").expect("name"));
            let into = work.join(text_of(with, "path").expect("path"));
            std::fs::create_dir_all(&into).expect("make the download directory");
            let from = site.root.join("artifacts").join(&artifact);
            let Ok(listing) = std::fs::read_dir(&from) else {
                return Ended::Failed(format!("download {artifact}"));
            };
            for file in listing {
                let file = file.expect("list the artifact");
                std::fs::copy(file.path(), into.join(file.file_name())).expect("copy a file");
            }
            continue;
        }
        let script = text_of(step, "run").expect("a step runs or uses");
        let refused = release_ref::calls(step)
            .into_iter()
            .any(|(_, args)| floor_problem(SURFACE_FILE, &args).is_some());
        if release_checkout && refused {
            return Ended::Failed(name);
        }
        let output_file: PathBuf = site.root.join("temp").join(format!("output-{at}"));
        let mut command = Command::new("bash");
        command
            .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", &script])
            .current_dir(&work)
            .env_clear()
            .env("PATH", &path)
            .env("GITHUB_OUTPUT", &output_file)
            .env("GITHUB_REPOSITORY", REPO)
            .env("RUNNER_TEMP", site.root.join("temp"))
            // What xtask prints is also in the output file, which is what is read.
            .stdout(std::process::Stdio::null());
        for (k, v) in entry(step, "env").map_or(&[][..], entries) {
            let (k, v) = (k.as_str().expect("env key"), v.as_str().expect("env value"));
            command.env(k, evaluate(site, &outputs, v));
        }
        let status = command.status().expect("run bash");
        if !status.success() {
            return Ended::Failed(name);
        }
        if let Some(id) = text_of(step, "id") {
            outputs.insert(id, read_outputs(&output_file));
        }
    }
    let job_outputs = entry(job, "outputs")
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| {
            let name = k.as_str().expect("output name").to_owned();
            let mut value = evaluate(site, &outputs, v.as_str().expect("output value"));
            if name == "targets" {
                // The JSON's value, not the order its writer put the keys in.
                let json = serde_json::from_str(&value).expect("targets are JSON");
                value = sorted(json).to_string();
            }
            (name, value)
        })
        .collect();
    Ended::Passed(job_outputs)
}

/// The outputs of a run that passed for `version`, with `newest`.
fn passed(version: &str, newest: bool) -> Ended {
    Ended::Passed(BTreeMap::from([
        ("newest".to_owned(), newest.to_string()),
        ("targets".to_owned(), targets(version)),
        ("version".to_owned(), version.to_owned()),
    ]))
}

#[test]
fn the_set_desired_job_runs_only_when_the_targets_job_says_newest() {
    // Catches: this test's reading of `set-desired`'s `if:` drifting from the workflow's.
    assert!(SET_IF.contains("needs.targets.outputs.newest == 'true'"));
    assert!(!Ended::Failed("x".to_owned()).sets_desired());
    assert!(!passed("1.2.9", false).sets_desired());
    assert!(passed("1.2.9", true).sets_desired());
}

#[test]
fn a_release_is_set_with_its_native_targets_alone_and_no_channel_is_read() {
    // Catches: targets that are not, per Linux platform of the release's own files,
    // hostkeeper's `.deb` from the testing suite alone (a list for macOS, another digest); an index or its digest still given to `set-desired`; and a
    // step that reads the release channel (no `curl` is on the runner's `PATH` here, so it
    // would fail).
    let site = Site::new("native", &["1.2.10"], &["v1.2.9", "v1.2.10"]);
    let ended = run(&site);
    assert_eq!(ended, passed("1.2.10", true));
    assert!(ended.sets_desired());
}

#[test]
fn a_release_older_than_one_already_published_is_not_set() {
    // Catches: the newest picked by text, where `1.2.9` sorts after `1.2.10`, and a late run of
    // an older commit setting its version after a newer one was published.
    let site = Site::new("older", &["1.2.9"], &["v1.2.9", "v1.2.10"]);
    let ended = run(&site);
    assert_eq!(ended, passed("1.2.9", false));
    assert!(!ended.sets_desired());
}

#[test]
fn of_several_releases_in_one_run_only_the_newest_is_set() {
    // Catches: the first entry of the matrix (the oldest) taken for the newest.
    let site = Site::new("several", &["1.2.8", "1.2.9"], &["v1.2.8", "v1.2.9"]);
    assert_eq!(run(&site), passed("1.2.9", true));
}

#[test]
fn tags_that_are_not_a_release_version_do_not_count_as_newer() {
    // Catches: a tag such as `v1.3.0-rc1` or `latest` that is not `vX.Y.Z` either taken as the
    // newest release or breaking the comparison.
    let site = Site::new(
        "tags",
        &["1.2.10"],
        &["latest", "v1.3.0-rc1", "v1.2.10", "v1.2.9"],
    );
    assert_eq!(run(&site), passed("1.2.10", true));
}

#[test]
fn of_several_releases_the_targets_are_the_newest_ones() {
    // Catches: targets written from another release of the run than the version set, so that
    // the set names one version and orders another's packages.
    let site = Site::new(
        "several-targets",
        &["1.2.8", "1.2.9"],
        &["v1.2.8", "v1.2.9"],
    );
    let Ended::Passed(outputs) = run(&site) else {
        panic!("the job passes");
    };
    assert_eq!(outputs["targets"], targets("1.2.9"));
    assert_ne!(outputs["targets"], targets("1.2.8"));
}

#[test]
fn a_release_missing_a_deb_its_pkg_or_a_bundle_stops_the_job_before_the_set() {
    // Catches: targets written for a release whose files do not all check out (one of
    // hostkeeper's `.deb`s, its installer `.pkg`, or a bundle, whose entry would pin bytes no
    // store holds), or a failure of the targets step that the job goes on after: the set must
    // not run with no targets.
    for (n, withheld) in [
        "hostkeeper_1.2.10_amd64.deb",
        "hostkeeper_1.2.10_arm64.deb.sigstore.json",
        "hostkeeper-1.2.10-macos-arm64.pkg",
        "hostkeeper-1.2.10-macos-arm64.pkg.sigstore.json",
    ]
    .into_iter()
    .enumerate()
    {
        let site = Site::new(&format!("withheld-{n}"), &["1.2.10"], &["v1.2.10"]);
        site.withhold("1.2.10", withheld);
        let ended = run(&site);
        assert_eq!(
            ended,
            Ended::Failed("The targets of the release".to_owned()),
            "{withheld}"
        );
        assert!(!ended.sets_desired());
    }
}

#[test]
fn a_wait_for_the_release_channel_put_back_into_the_job_fails() {
    // Catches: the wait for the release channel back in the job (here the runner has no `curl`
    // and the run is given no channel), which would hold every release on a channel no host
    // reads any more; `run_text` refuses its `if:` steps and unknown expressions, so the test
    // puts back the wait alone.
    let text = release_yml();
    let wait = "      - name: Wait for the release channel\n        run: curl --fail --output \"$RUNNER_TEMP/served-index.json\" https://channel.example/releases/hostkeeper/index.json\n";
    let at = "      - name: The targets of the release\n";
    assert_eq!(text.matches(at).count(), 1);
    let changed = text.replacen(at, &format!("{wait}{at}"), 1);
    let site = Site::new("channel-wait", &["1.2.10"], &["v1.2.10"]);
    assert_eq!(
        run_text(&site, &changed),
        Ended::Failed("Wait for the release channel".to_owned())
    );
}
