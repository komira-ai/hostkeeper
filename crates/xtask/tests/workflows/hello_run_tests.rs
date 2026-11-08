//! The job `release` of the repository's `hello-desired.yml`, run for real, and what it hands
//! the job `set-desired`: each test reads the committed workflow, runs the job's `run:` steps in
//! order with `bash -eo pipefail` under the job's and each step's `env`, then evaluates
//! `set-desired`'s `with` from the job's outputs. Nothing is substituted in a command. The
//! expressions are evaluated as GitHub does for the forms the workflow uses (an input, a quoted
//! text, `format('{0}...', ...)`, a comparison with a text, `a && b || c`, a step's or a job's
//! output); any other form fails the test. The one action of the job, the checkout, is done by
//! the test: it runs in this repository's own build.
//!
//! `curl`, `cargo` and `rustup` on the `PATH` are the test program `tests/bin/gh_fixture.rs`.
//! As `curl` it serves the release's assets from scratch files (no test reaches GitHub); as
//! `cargo` it runs this crate's real `xtask` binary, so the step that writes the targets runs the
//! real `release targets --service hello`. `sha256sum` and `cut` are the runner's own.
//!
//! Each release published here carries both variants, hello's build and its failing build, so a
//! job that read the other variant's index or archives would set the wrong one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::callers::{HELLO_FILE, HELLO_JOB, HELLO_RELEASE_JOB};
use super::lint::{entries, entry};
use super::release_files::{REPO, package_files, sha256, sorted, targets};
use super::testdir::TestDir;
use super::{workflows, yaml};

const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");
const XTASK: &str = env!("CARGO_BIN_EXE_xtask");

/// The release the dispatches name.
const RELEASE: &str = "1.2.3";

/// The package version and the index name of a variant of the release `release`.
fn variant_of(release: &str, variant: &str) -> (String, &'static str) {
    match variant {
        "hello" => (release.to_owned(), "hello-index.json"),
        "fail-ready" => (
            format!("{release}-fail-ready"),
            "hello-fail-ready-index.json",
        ),
        other => panic!("no variant {other}"),
    }
}

/// The files of one variant of hello's release `release`.
fn variant_files(release: &str, variant: &str) -> BTreeMap<String, Vec<u8>> {
    let (version, index) = variant_of(release, variant);
    package_files("hello", &version, index)
}

/// A scratch site: the fakes on `bin/`, the release assets the fake curl serves.
struct Site {
    root: TestDir,
}

impl Site {
    /// A site whose repository has published the release [`RELEASE`] with both variants.
    fn new(name: &str) -> Site {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("hello-run");
        let root = TestDir::under(&base, name);
        for dir in ["bin", "work", "temp"] {
            std::fs::create_dir_all(root.join(dir)).expect("make a scratch directory");
        }
        // Hard links, not copies (as in channel_run_tests.rs): the fake finds its data beside
        // the name it was started by.
        for name in ["curl", "cargo", "rustup"] {
            std::fs::hard_link(GH, root.join("bin").join(name)).expect("install a fake");
        }
        std::fs::hard_link(XTASK, root.join("bin/xtask")).expect("install xtask");
        let site = Site { root };
        for variant in ["hello", "fail-ready"] {
            for (name, bytes) in variant_files(RELEASE, variant) {
                std::fs::write(site.asset(&name), bytes).expect("write an asset");
            }
        }
        site
    }

    /// Where the fake curl serves the asset `name` of the release [`RELEASE`].
    fn asset(&self, name: &str) -> PathBuf {
        let dir = self
            .root
            .join("bin/channel/github.com")
            .join(REPO)
            .join("releases/download")
            .join(format!("v{RELEASE}"));
        std::fs::create_dir_all(&dir).expect("make the release's asset directory");
        dir.join(name)
    }
}

/// What a dispatch's run of the job gave `set-desired`, or the step that failed.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Every step of the job passed: `set-desired`'s inputs from it, by name.
    Set(BTreeMap<String, String>),
    /// The step of this name failed; nothing after it ran, and `set-desired` did not run.
    Failed(String),
}

/// The values a run is given: the dispatch's inputs, and the outputs so far.
struct Scope<'a> {
    inputs: &'a BTreeMap<&'a str, &'a str>,
    steps: &'a BTreeMap<String, BTreeMap<String, String>>,
    jobs: &'a BTreeMap<String, String>,
}

impl Scope<'_> {
    /// The value of one operand: a quoted text, `format('{0}...', <operand>)`, an input, or an
    /// output of a step or of the job `release`.
    fn operand(&self, term: &str) -> String {
        let term = term.trim();
        if let Some(text) = term.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
            return text.to_owned();
        }
        if let Some(args) = term
            .strip_prefix("format('")
            .and_then(|t| t.strip_suffix(')'))
        {
            let (pattern, arg) = args.split_once("', ").expect("format('<text>', <operand>)");
            return pattern.replace("{0}", &self.operand(arg));
        }
        match term.split('.').collect::<Vec<_>>().as_slice() {
            ["inputs", name] => self
                .inputs
                .get(name)
                .copied()
                .unwrap_or_default()
                .to_owned(),
            ["steps", id, "outputs", key] => self
                .steps
                .get(*id)
                .and_then(|o| o.get(*key))
                .cloned()
                .unwrap_or_default(),
            ["needs", job, "outputs", key] if *job == HELLO_RELEASE_JOB => {
                self.jobs.get(*key).cloned().unwrap_or_default()
            }
            _ => panic!("the test does not know the expression `{term}`"),
        }
    }

    /// The value of `expr`: `<a> == '<text>'`, `<a> && <b> || <c>` (GitHub's: `b` when `a`
    /// holds and `b` is not empty, else `c`), or an operand.
    fn expression(&self, expr: &str) -> String {
        if let Some((left, otherwise)) = expr.split_once(" || ") {
            let (condition, then) = left.split_once(" && ").expect("`a && b || c`");
            let value = self.condition(condition).then(|| self.operand(then));
            return value
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| self.operand(otherwise));
        }
        self.operand(expr)
    }

    fn condition(&self, condition: &str) -> bool {
        let (left, right) = condition.split_once(" == ").expect("`<a> == '<text>'`");
        self.operand(left) == self.operand(right)
    }

    /// `value` with each `${{ ... }}` evaluated.
    fn evaluate(&self, value: &str) -> String {
        let mut out = String::new();
        let mut rest = value;
        while let Some(at) = rest.find("${{ ") {
            out.push_str(&rest[..at]);
            let after = &rest[at + 4..];
            let end = after.find(" }}").expect("an expression ends with ` }}`");
            out.push_str(&self.expression(&after[..end]));
            rest = &after[end + 3..];
        }
        out.push_str(rest);
        out
    }
}

/// The `(key, value)` text pairs of the mapping `key` of `node`.
fn pairs<'a>(node: &'a yaml::Node, key: &str) -> Vec<(&'a str, &'a str)> {
    entry(node, key)
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().expect("a key"),
                v.as_str().expect("a text value"),
            )
        })
        .collect()
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

/// Runs the committed job `release` of hello-desired.yml for a dispatch with `version` and
/// `variant`, then evaluates the inputs `set-desired` is given from its outputs.
fn dispatch(site: &Site, version: &str, variant: &str) -> Ended {
    let text = workflows()
        .into_iter()
        .find(|(file, _)| file == HELLO_FILE)
        .map(|(_, text)| text)
        .expect("hello-desired.yml exists");
    let doc = yaml::load(&text).expect("hello-desired.yml parses");
    let jobs = entry(&doc, "jobs").expect("the workflow has jobs");
    let job = entry(jobs, HELLO_RELEASE_JOB).expect("the job release");
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(yaml::Value::Seq(items)) => items,
        _ => panic!("the job release has no steps"),
    };
    let inputs = BTreeMap::from([("version", version), ("variant", variant)]);
    let mut outputs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let none = BTreeMap::new();
    let path = format!("{}:/usr/bin:/bin", site.root.join("bin").display());
    for (at, step) in steps.iter().enumerate() {
        let scope = Scope {
            inputs: &inputs,
            steps: &outputs,
            jobs: &none,
        };
        let text_of = |key: &str| entry(step, key).and_then(yaml::Node::as_str);
        let name = text_of("name").map_or_else(|| format!("step {}", at + 1), str::to_owned);
        if let Some(uses) = text_of("uses") {
            assert!(uses.starts_with("actions/checkout@"), "{uses}");
            continue;
        }
        let script = text_of("run").expect("a step runs or uses");
        let output_file = site.root.join("temp").join(format!("output-{at}"));
        let mut command = Command::new("bash");
        command
            .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", script])
            .current_dir(site.root.join("work"))
            .env_clear()
            .env("PATH", &path)
            .env("GITHUB_OUTPUT", &output_file)
            .env("GITHUB_REPOSITORY", REPO)
            .env("RUNNER_TEMP", site.root.join("temp"))
            // What xtask prints is also in the output file, which is what is read.
            .stdout(std::process::Stdio::null());
        for (k, v) in pairs(job, "env").into_iter().chain(pairs(step, "env")) {
            command.env(k, scope.evaluate(v));
        }
        if !command.status().expect("run bash").success() {
            return Ended::Failed(name);
        }
        if let Some(id) = text_of("id") {
            outputs.insert(id.to_owned(), read_outputs(&output_file));
        }
    }
    let scope = Scope {
        inputs: &inputs,
        steps: &outputs,
        jobs: &none,
    };
    let job_outputs: BTreeMap<String, String> = pairs(job, "outputs")
        .into_iter()
        .map(|(k, v)| (k.to_owned(), scope.evaluate(v)))
        .collect();
    let scope = Scope {
        inputs: &inputs,
        steps: &outputs,
        jobs: &job_outputs,
    };
    let caller = entry(jobs, HELLO_JOB).expect("the job set-desired");
    let given = pairs(caller, "with")
        .into_iter()
        .filter(|(_, v)| v.contains(&format!("needs.{HELLO_RELEASE_JOB}.outputs.")))
        .map(|(k, v)| {
            let mut value = scope.evaluate(v);
            if k == "targets" {
                // The JSON's value, not the order its writer put the keys in.
                value = sorted(serde_json::from_str(&value).expect("targets are JSON")).to_string();
            }
            (k.to_owned(), value)
        })
        .collect();
    Ended::Set(given)
}

/// What `set-desired` must be given for `variant` of the release [`RELEASE`]: the variant's
/// version, its index's exact text and SHA-256, and the targets of its own archives.
fn set_with(variant: &str) -> Ended {
    let (version, index_name) = variant_of(RELEASE, variant);
    let files = variant_files(RELEASE, variant);
    let index = String::from_utf8(files[index_name].clone()).expect("UTF-8");
    Ended::Set(BTreeMap::from([
        ("release_index_digest".to_owned(), sha256(index.as_bytes())),
        ("release_index".to_owned(), index),
        (
            "targets".to_owned(),
            targets("hello", &version, RELEASE, &files),
        ),
        ("version".to_owned(), version),
    ]))
}

#[test]
fn a_dispatch_of_hello_sets_its_version_index_digest_and_the_targets_of_its_archives() {
    // Catches: the job reading the failing build's index or archives for `hello`, an index
    // passed on with other bytes than the release's or a digest not of those bytes, targets not
    // one archive entry per platform of the variant's own files, and the job's outputs not the
    // ones set-desired is given.
    let site = Site::new("hello");
    assert_eq!(dispatch(&site, RELEASE, "hello"), set_with("hello"));
}

#[test]
fn a_dispatch_of_fail_ready_sets_the_failing_builds_version_index_digest_and_targets() {
    // Catches: `--fail-ready` not given to the verb (it would write hello's targets for the
    // failing build's version), the release's own version set instead of
    // `<version>-fail-ready`, and hello's index or archives read for the failing build.
    let site = Site::new("fail-ready");
    let set = dispatch(&site, RELEASE, "fail-ready");
    assert_eq!(set, set_with("fail-ready"));
    assert_ne!(set, set_with("hello"));
}

#[test]
fn a_version_that_is_not_a_release_stops_the_job_before_any_download() {
    // Catches: a dispatch's text reaching the download URL or the outputs unchecked, a
    // version with more or fewer parts, and one with a suffix of its own.
    for (n, version) in ["1.2", "1.2.3.x", "1.2.3-fail-ready", "v1.2.3", "1.2.3 x"]
        .into_iter()
        .enumerate()
    {
        let site = Site::new(&format!("version-{n}"));
        assert_eq!(
            dispatch(&site, version, "hello"),
            Ended::Failed("Refuse a version that is not X.Y.Z".to_owned()),
            "{version:?}"
        );
    }
}

#[test]
fn a_release_without_a_variants_bundle_or_index_stops_at_the_download() {
    // Catches: a download that goes on past a missing file (curl without `--fail-early` ends
    // with the last transfer's status), and so a set made without that platform's bundle or
    // without the index.
    for (n, missing) in [
        "hello-1.2.3-linux-arm64.tar.zst.sigstore.json",
        "hello-1.2.3-macos-arm64.tar.zst",
        "hello-index.json",
    ]
    .into_iter()
    .enumerate()
    {
        let site = Site::new(&format!("missing-{n}"));
        std::fs::remove_file(site.asset(missing)).expect("remove an asset");
        assert_eq!(
            dispatch(&site, RELEASE, "hello"),
            Ended::Failed("Download the variant's index, archives and bundles".to_owned()),
            "{missing}"
        );
    }
}

#[test]
fn an_archive_that_is_not_the_one_its_index_lists_stops_at_the_targets() {
    // Catches: targets written for an archive whose bytes the index does not list (a release
    // asset replaced after its index was written), which the daemon would refuse on every host.
    let site = Site::new("replaced");
    std::fs::write(
        site.asset("hello-1.2.3-linux-x86_64.tar.zst"),
        b"other bytes",
    )
    .expect("replace an archive");
    assert_eq!(
        dispatch(&site, RELEASE, "hello"),
        Ended::Failed("The targets of the variant's package".to_owned())
    );
}
