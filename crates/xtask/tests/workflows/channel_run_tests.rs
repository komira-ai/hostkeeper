//! The job `channel` of the repository's `release.yml`, run for real: each test reads the
//! committed job, runs its `run:` steps in order with `bash -eo pipefail` (the shell GitHub runs
//! a `run:` step with), and reads the outputs the job gives `set-desired`. Nothing is
//! substituted in a command. The step `env` expressions are given the values a run of main gives
//! them, and an expression the test does not know fails the test. The two actions of the job are
//! done by the test: the checkout of the newest release's commit (the test runs in this
//! repository's own build, so it checks only that the commit is the newest release's), and the
//! download of the run's own `release-<tag>` artifact, whose files it copies into the step's
//! `path`. Each artifact holds what `release targets` reads of a release: hostkeeper's index, an
//! archive per platform holding its `manifest.toml`, and each archive's bundle.
//!
//! `gh`, `curl`, `cargo` and `rustup` on the `PATH` are the test program
//! `tests/bin/gh_fixture.rs`, which answers from scratch files, so no test reaches GitHub or a
//! channel; as `cargo` it runs this crate's real `xtask` binary, so the step that writes the
//! targets runs the real `release targets`. `jq`, `cmp`, `sha256sum` and `cut` are the runner's
//! own, as in the workflow.
//!
//! What is asserted is what decides whether hostkeeper's desired version is set, and with what:
//! which step fails, and the outputs `version`, `release_index`, `release_index_digest`,
//! `targets` and `newest`. `set-desired` runs only when the job succeeds and `newest` is `true`
//! (its `if:`, [`SET_IF`], which the lint holds).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::desired::{CHANNEL_JOB, SET_IF};
use super::lint::{entries, entry};
use super::release_files::{self, REPO, package_files, sha256, sorted};
use super::testdir::TestDir;
use super::{workflows, yaml};

const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");
const XTASK: &str = env!("CARGO_BIN_EXE_xtask");

/// The channel's base URL the runs are given (`vars.HOSTKEEPER_RELEASE_CHANNEL`); the fake
/// `curl` serves `channel/channel.example/releases/...`.
const CHANNEL: &str = "https://channel.example/releases";

/// The commit the release `version` of the plan's matrix is built from: one of its own for each
/// version, its digits padded to 40.
fn commit_of(version: &str) -> String {
    format!("{:0>40}", version.replace('.', ""))
}

/// The files of hostkeeper's release `version` that the job reads, by name.
fn hostkeeper_files(version: &str) -> BTreeMap<String, Vec<u8>> {
    package_files("hostkeeper", version, "index.json")
}

/// The canonical release index of hostkeeper at `version`: one line, no final line break.
fn index(version: &str) -> String {
    let files = hostkeeper_files(version);
    String::from_utf8(files["index.json"].clone()).expect("UTF-8")
}

/// The targets the job must give for hostkeeper's release `version`.
fn targets(version: &str) -> String {
    release_files::targets("hostkeeper", version, version, &hostkeeper_files(version))
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
        let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("channel-run");
        let root = TestDir::under(&base, name);
        for dir in ["bin/releases", "work", "temp", "artifacts"] {
            std::fs::create_dir_all(root.join(dir)).expect("make a scratch directory");
        }
        // Hard links, not copies (as in tests/channel_sync.rs): the fake finds its data beside
        // the name it was started by, and acts as curl, cargo or rustup when started by that
        // name. As cargo it runs the real xtask linked beside it.
        for name in ["gh", "curl", "cargo", "rustup"] {
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

    /// The channel serves `index` as hostkeeper's index of `version`.
    fn serve(&self, version: &str, index: &str) {
        let dir = self
            .root
            .join("bin/channel/channel.example/releases/hostkeeper")
            .join(version);
        std::fs::create_dir_all(&dir).expect("make the channel's version directory");
        std::fs::write(dir.join("index.json"), index).expect("write the served index");
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
            ["vars", "HOSTKEEPER_RELEASE_CHANNEL"] => CHANNEL.to_owned(),
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

/// Runs the committed job `channel` against `site`.
fn run(site: &Site) -> Ended {
    let text = workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists");
    let doc = yaml::load(&text).expect("release.yml parses");
    let job = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, CHANNEL_JOB))
        .expect("release.yml has the job channel");
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(yaml::Value::Seq(items)) => items,
        _ => panic!("the job channel has no steps"),
    };
    let work = site.root.join("work");
    let path = format!("{}:/usr/bin:/bin", site.root.join("bin").display());
    let mut outputs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (at, step) in steps.iter().enumerate() {
        let text_of = |node: &yaml::Node, key: &str| {
            entry(node, key)
                .and_then(yaml::Node::as_str)
                .map(str::to_owned)
        };
        let name = text_of(step, "name").unwrap_or_else(|| format!("step {}", at + 1));
        if let Some(uses) = text_of(step, "uses") {
            let with = entry(step, "with").expect("an action of the job has `with`");
            if uses.starts_with("actions/checkout@") {
                let commit = evaluate(site, &outputs, &text_of(with, "ref").expect("ref"));
                let (newest, _) = site.matrix.last().expect("a release");
                assert_eq!(
                    commit,
                    commit_of(newest),
                    "the checkout is of the newest release's commit"
                );
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
    let index = index(version);
    Ended::Passed(BTreeMap::from([
        ("newest".to_owned(), newest.to_string()),
        ("release_index_digest".to_owned(), sha256(index.as_bytes())),
        ("release_index".to_owned(), index),
        ("targets".to_owned(), targets(version)),
        ("version".to_owned(), version.to_owned()),
    ]))
}

#[test]
fn the_set_desired_job_runs_only_when_the_channel_job_says_newest() {
    // Catches: this test's reading of `set-desired`'s `if:` drifting from the workflow's.
    assert!(SET_IF.contains("needs.channel.outputs.newest == 'true'"));
    assert!(!Ended::Failed("x".to_owned()).sets_desired());
    assert!(!passed("1.2.9", false).sets_desired());
    assert!(passed("1.2.9", true).sets_desired());
}

#[test]
fn a_release_the_channel_serves_byte_for_byte_is_set_with_its_index_digest_and_targets() {
    // Catches: a job that does not pass on the published index and its SHA-256 exactly, or that
    // reads the channel at another path than `<channel>/hostkeeper/<version>/index.json`; and
    // targets that are not one archive entry per platform of the release's own files (another
    // store order, another asset path, another size or digest, no bundle, no floor).
    let site = Site::new("served", &["1.2.10"], &["v1.2.9", "v1.2.10"]);
    site.serve("1.2.10", &index("1.2.10"));
    let ended = run(&site);
    assert_eq!(ended, passed("1.2.10", true));
    assert!(ended.sets_desired());
}

#[test]
fn a_release_older_than_one_already_published_is_not_set() {
    // Catches: the newest picked by text, where `1.2.9` sorts after `1.2.10`, and a late run of
    // an older commit setting its version after a newer one was published.
    let site = Site::new("older", &["1.2.9"], &["v1.2.9", "v1.2.10"]);
    site.serve("1.2.9", &index("1.2.9"));
    let ended = run(&site);
    assert_eq!(ended, passed("1.2.9", false));
    assert!(!ended.sets_desired());
}

#[test]
fn of_several_releases_in_one_run_only_the_newest_is_set() {
    // Catches: the first entry of the matrix (the oldest) taken for the newest.
    let site = Site::new("several", &["1.2.8", "1.2.9"], &["v1.2.8", "v1.2.9"]);
    site.serve("1.2.9", &index("1.2.9"));
    let ended = run(&site);
    assert_eq!(ended, passed("1.2.9", true));
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
    site.serve("1.2.10", &index("1.2.10"));
    assert_eq!(run(&site), passed("1.2.10", true));
}

#[test]
fn a_channel_that_serves_other_bytes_stops_the_job_before_any_output() {
    // Catches: a set made from the published index while the channel serves another index of
    // the same version, which it never replaces.
    let site = Site::new("other-bytes", &["1.2.10"], &["v1.2.10"]);
    site.serve("1.2.10", &format!("{}\n", index("1.2.10")));
    let ended = run(&site);
    assert_eq!(
        ended,
        Ended::Failed("The channel serves the published index, byte for byte".to_owned())
    );
    assert!(!ended.sets_desired());
}

#[test]
fn a_channel_that_does_not_serve_the_version_stops_the_job() {
    // Catches: a job that goes on when the channel answers 404, because the sync has not staged
    // the release (here: it never does); and a channel serving only another version.
    let site = Site::new("missing", &["1.2.10"], &["v1.2.10"]);
    site.serve("1.2.9", &index("1.2.9"));
    let ended = run(&site);
    assert_eq!(
        ended,
        Ended::Failed("Wait until the release channel serves the version's index".to_owned())
    );
    assert!(!ended.sets_desired());
}

#[test]
fn of_several_releases_the_targets_are_the_newest_ones() {
    // Catches: targets written from another release of the run than the version set, so that
    // the set names one version and orders another's archives (the coordinator refuses the
    // pair only when it holds the index too).
    let site = Site::new(
        "several-targets",
        &["1.2.8", "1.2.9"],
        &["v1.2.8", "v1.2.9"],
    );
    site.serve("1.2.9", &index("1.2.9"));
    let Ended::Passed(outputs) = run(&site) else {
        panic!("the job passes");
    };
    assert_eq!(outputs["targets"], targets("1.2.9"));
    assert_ne!(outputs["targets"], targets("1.2.8"));
}

#[test]
fn a_release_missing_an_archive_bundle_stops_the_job_before_the_set() {
    // Catches: targets written for a release whose files do not all check out, or a failure of
    // the targets step that the job goes on after: the set must not run with no targets.
    for (n, withheld) in [
        "hostkeeper-1.2.10-macos-arm64.tar.zst.sigstore.json",
        "hostkeeper-1.2.10-linux-arm64.tar.zst",
    ]
    .into_iter()
    .enumerate()
    {
        let site = Site::new(&format!("withheld-{n}"), &["1.2.10"], &["v1.2.10"]);
        site.withhold("1.2.10", withheld);
        site.serve("1.2.10", &index("1.2.10"));
        let ended = run(&site);
        assert_eq!(
            ended,
            Ended::Failed("The targets of the release".to_owned()),
            "{withheld}"
        );
        assert!(!ended.sets_desired());
    }
}
