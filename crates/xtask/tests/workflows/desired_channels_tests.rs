//! The package channels hostkeeper's own deploy in `release.yml` waits for
//! ([`super::desired::CHANNEL_WRITES`]): `set-desired` sets the release by its targets, and a host
//! defers an entry whose channel does not list it yet (`not_in_repository`), so the job must not
//! run before every channel the targets point at was written.
//!
//! The targets are the ones the real `release targets` writes for hostkeeper's release (this
//! crate's own `xtask` binary, on the native packages of [`release_files::native_files`]), and
//! what is asserted is that `set-desired` needs, directly or through the jobs it needs, every job
//! that writes the channel of each of their entries: an entry whose channel has no row, and a row
//! that no entry points at, are refused too. Then the `if:` of `set-desired`, evaluated against
//! each outcome of those writes, runs the job only once each channel holds the release.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::{Value as Json, json};

use super::desired::{CHANNEL_WRITES, FILE, JOB, SET_DESIRED_IF, job, needs};
use super::expr::{Context, holds};
use super::release_files::{self, REPO};
use super::testdir::TestDir;
use super::yaml::{self, Node};
use super::{with_in, workflows};

const XTASK: &str = env!("CARGO_BIN_EXE_xtask");

/// The release the targets are written for.
const VERSION: &str = "0.1.251";

/// The `needs:` line of `set-desired` in the repository's `release.yml`.
const NEEDS_LINE: &str = "    needs: [plan, targets, coordinator, testing-apt-write, testing-apt-write-2, testing-apt-write-3, testing-pkg-upload, no-downgrade]\n    if: ${{ !cancelled()";

fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == FILE)
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// The repository's `release.yml` with `set-desired` needing `list` (the text of a YAML list).
fn needing(list: &str) -> String {
    with_in(
        &release_yml(),
        NEEDS_LINE,
        &format!("    needs: {list}\n    if: ${{{{ !cancelled()"),
    )
}

/// The targets the real `release targets` writes for hostkeeper's release [`VERSION`], as JSON.
fn release_targets() -> Json {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("desired-channels");
    let dir = TestDir::under(&base, "release");
    for (name, bytes) in release_files::native_files("hostkeeper", VERSION) {
        std::fs::write(dir.join(name), bytes).expect("write a file of the release");
    }
    let out = Command::new(XTASK)
        .args(["release", "targets", "--version", VERSION, "--dir"])
        .arg(&*dir)
        .args(["--repo", REPO])
        .output()
        .expect("run xtask release targets");
    assert!(
        out.status.success(),
        "release targets failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("the answer is text");
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("targets="))
        .expect("release targets answers `targets=`");
    serde_json::from_str(line).expect("the targets are JSON")
}

/// Every job `name` of `doc` needs, directly or through the jobs it needs.
fn waited_for(doc: &Node, name: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut next = vec![name.to_owned()];
    while let Some(at) = next.pop() {
        for need in needs(job(doc, &at)) {
            if found.insert(need.to_owned()) {
                next.push(need.to_owned());
            }
        }
    }
    found
}

/// The `(installer, repository or store)` of each entry of `targets`, sorted, once each.
fn channels_of(targets: &Json) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for list in targets.as_object().into_iter().flat_map(|l| l.values()) {
        for entry in list["entries"].as_array().into_iter().flatten() {
            let installer = entry["installer"].as_str().unwrap_or_default().to_owned();
            let mut places: Vec<&str> = entry["locations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["store"].as_str())
                .collect();
            places.extend(entry["repository"].as_str());
            if places.is_empty() {
                places.push("");
            }
            for place in places {
                found.insert((installer.clone(), place.to_owned()));
            }
        }
    }
    found
}

/// One line per channel of `targets` that job `set-desired` of the workflow `text` does not wait
/// for, per entry whose channel no row of [`CHANNEL_WRITES`] names, and per row no entry points at.
fn channel_problems(text: &str, targets: &Json) -> Vec<String> {
    let doc = yaml::load(text).expect("release.yml parses");
    let waited = waited_for(&doc, JOB);
    let mut problems = Vec::new();
    let mut used = BTreeSet::new();
    for (installer, place) in channels_of(targets) {
        let Some((_, _, jobs)) = CHANNEL_WRITES
            .iter()
            .find(|(i, p, _)| *i == installer && *p == place)
        else {
            problems.push(format!(
                "the targets have an {installer} entry in {place:?}, whose channel write no row of desired::CHANNEL_WRITES names"
            ));
            continue;
        };
        used.insert((installer.clone(), place.clone()));
        for write in jobs.iter().filter(|w| !waited.contains(**w)) {
            problems.push(format!(
                "{FILE}: job {JOB} does not wait for {write}, which writes {place} for the {installer} entries of the targets"
            ));
        }
    }
    for (installer, place, _) in CHANNEL_WRITES {
        if !used.contains(&(installer.to_owned(), place.to_owned())) {
            problems.push(format!(
                "desired::CHANNEL_WRITES names {installer} in {place}, which no entry of the targets points at"
            ));
        }
    }
    problems
}

#[test]
fn set_desired_waits_for_every_channel_the_release_targets_point_at() {
    // Catches: set-desired running before the testing apt write (or the pkg upload) lands, so
    // every host defers the release with `not_in_repository` until it does.
    let targets = release_targets();
    assert_eq!(
        channels_of(&targets),
        BTreeSet::from([
            ("apt".to_owned(), "komira-testing".to_owned()),
            ("macos-pkg".to_owned(), "komira-testing".to_owned()),
        ])
    );
    assert_eq!(
        channel_problems(&release_yml(), &targets),
        Vec::<String>::new()
    );
}

#[test]
fn set_desired_that_needs_only_the_coordinator_is_refused_for_each_channel_write() {
    // Catches: the job as it was, needing `coordinator` alone, which finishes minutes before the
    // channel writes.
    let text = needing("[plan, targets, coordinator]");
    assert_eq!(
        channel_problems(&text, &release_targets()),
        [
            "release.yml: job set-desired does not wait for testing-apt-write, which writes komira-testing for the apt entries of the targets",
            "release.yml: job set-desired does not wait for testing-apt-write-2, which writes komira-testing for the apt entries of the targets",
            "release.yml: job set-desired does not wait for testing-apt-write-3, which writes komira-testing for the apt entries of the targets",
            "release.yml: job set-desired does not wait for testing-pkg-upload, which writes komira-testing for the macos-pkg entries of the targets",
        ]
    );
}

#[test]
fn set_desired_that_does_not_wait_for_the_pkg_upload_is_refused() {
    // Catches: the Linux channel waited for and the Mac's installer directory not, so a Mac is
    // ordered a .pkg the store does not hold yet.
    let text = needing(
        "[plan, targets, coordinator, testing-apt-write, testing-apt-write-2, testing-apt-write-3]",
    );
    assert_eq!(
        channel_problems(&text, &release_targets()),
        [
            "release.yml: job set-desired does not wait for testing-pkg-upload, which writes komira-testing for the macos-pkg entries of the targets"
        ]
    );
}

#[test]
fn a_write_needed_through_a_later_attempt_counts_as_waited_for() {
    // Catches: the closure read as the direct needs alone; attempt 3 needs attempt 2's read,
    // which needs attempt 2, and so on back to attempt 1.
    let text = needing("[plan, targets, coordinator, testing-apt-write-3, testing-pkg-upload]");
    let doc = yaml::load(&text).expect("release.yml parses");
    let waited = waited_for(&doc, JOB);
    for write in [
        "testing-apt-write",
        "testing-apt-write-2",
        "testing-apt-write-3",
    ] {
        assert!(waited.contains(write), "{write} in {waited:?}");
    }
    assert_eq!(
        channel_problems(&text, &release_targets()),
        Vec::<String>::new()
    );
}

#[test]
fn an_entry_of_a_channel_with_no_row_is_refused() {
    // Catches: the targets gaining an entry in a channel (an rpm repository, the failing build's
    // suite) that no job set-desired needs writes, passing because the rule knows only its rows.
    let mut targets = release_targets();
    let entries = targets["linux-x86_64"]["entries"]
        .as_array_mut()
        .expect("a Linux list");
    entries.push(json!({"installer": "rpm", "repository": "komira-testing"}));
    entries.push(json!({"installer": "apt", "repository": "komira-testing-fail"}));
    entries.push(json!({"installer": "apt"}));
    assert_eq!(
        channel_problems(&release_yml(), &targets),
        [
            "the targets have an apt entry in \"\", whose channel write no row of desired::CHANNEL_WRITES names",
            "the targets have an apt entry in \"komira-testing-fail\", whose channel write no row of desired::CHANNEL_WRITES names",
            "the targets have an rpm entry in \"komira-testing\", whose channel write no row of desired::CHANNEL_WRITES names",
        ]
    );
}

#[test]
fn a_row_no_entry_points_at_is_refused() {
    // Catches: a row left behind after its entries are gone, holding set-desired back for a
    // channel the release no longer uses.
    let mut targets = release_targets();
    targets
        .as_object_mut()
        .expect("the targets by platform")
        .remove("macos-arm64")
        .expect("a macOS list");
    assert_eq!(
        channel_problems(&release_yml(), &targets),
        [
            "desired::CHANNEL_WRITES names macos-pkg in komira-testing, which no entry of the targets points at"
        ]
    );
}

/// How the run stands when `set-desired`'s `if:` is read: going on, a job it needs failed (at
/// any depth: `failure()`), or cancelled.
#[derive(Clone, Copy)]
enum Run {
    Going,
    Failed,
    Cancelled,
}

/// What `set-desired`'s `if:` reads, for a run of main with the deploy on whose release is the
/// newest published and whose coordinator wait succeeded, with `outcome` laid over it.
fn context(outcome: &[(&str, &str)], run: Run) -> Context {
    let mut names: BTreeMap<String, String> = [
        ("needs.plan.outputs.release", "true"),
        ("github.ref", "refs/heads/main"),
        ("vars.RELEASE_SETS_DESIRED", "true"),
        ("needs.targets.outputs.newest", "true"),
        ("needs.coordinator.result", "success"),
        ("needs.testing-pkg-upload.outputs.enabled", "true"),
        ("needs.no-downgrade.outputs.set", "true"),
        ("needs.no-downgrade.outputs.attempt", "1"),
        ("github.run_attempt", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (name, value) in outcome {
        names.insert((*name).to_owned(), (*value).to_owned());
    }
    Context {
        names,
        cancelled: matches!(run, Run::Cancelled),
        failed: matches!(run, Run::Failed),
    }
}

const W1: &str = "needs.testing-apt-write.outputs.conflict";
const W2: &str = "needs.testing-apt-write-2.outputs.conflict";
const W3: &str = "needs.testing-apt-write-3.outputs.conflict";
const PKG: &str = "needs.testing-pkg-upload.outputs.enabled";
const COORDINATOR: &str = "needs.coordinator.result";

/// One outcome of the jobs `set-desired` needs, and whether it runs.
struct Case {
    name: &'static str,
    outcome: &'static [(&'static str, &'static str)],
    run: Run,
    runs: bool,
}

const fn case(
    name: &'static str,
    outcome: &'static [(&'static str, &'static str)],
    run: Run,
    runs: bool,
) -> Case {
    Case {
        name,
        outcome,
        run,
        runs,
    }
}

#[test]
fn set_desired_runs_once_each_channel_holds_the_release_and_not_otherwise() {
    // Catches: a condition that runs set-desired with no apt write landed (each attempt lost the
    // race, or the channel's settings are absent), with the pkg upload gated off, after a failed
    // write or coordinator wait, on a cancelled run; or one that skips it when attempt 1 landed
    // and attempts 2 and 3 were skipped (the implicit `success()` would).
    let cases = [
        case("attempt 1 landed", &[(W1, "false")], Run::Going, true),
        case(
            "attempt 2 landed",
            &[(W1, "true"), (W2, "false")],
            Run::Going,
            true,
        ),
        case(
            "attempt 3 landed",
            &[(W1, "true"), (W2, "true"), (W3, "false")],
            Run::Going,
            true,
        ),
        case("no apt write ran with its settings", &[], Run::Going, false),
        case(
            "attempt 1 lost the race and nothing followed",
            &[(W1, "true")],
            Run::Going,
            false,
        ),
        case(
            "attempt 3 lost the race too, and failed",
            &[(W1, "true"), (W2, "true"), (W3, "true")],
            Run::Failed,
            false,
        ),
        case(
            "the pkg upload had no settings",
            &[(W1, "false"), (PKG, "false")],
            Run::Going,
            false,
        ),
        case(
            "the pkg upload was skipped",
            &[(W1, "false"), (PKG, "")],
            Run::Going,
            false,
        ),
        case(
            "the pkg upload failed",
            &[(W1, "false")],
            Run::Failed,
            false,
        ),
        case(
            "the coordinator wait was skipped",
            &[(W1, "false"), (COORDINATOR, "skipped")],
            Run::Going,
            false,
        ),
        case(
            "the coordinator wait failed",
            &[(W1, "false"), (COORDINATOR, "failure")],
            Run::Failed,
            false,
        ),
        case(
            "a newer release is published",
            &[(W1, "false"), ("needs.targets.outputs.newest", "false")],
            Run::Going,
            false,
        ),
        case(
            "the deploy is off",
            &[(W1, "false"), ("vars.RELEASE_SETS_DESIRED", "")],
            Run::Going,
            false,
        ),
        case(
            "the run was cancelled",
            &[(W1, "false")],
            Run::Cancelled,
            false,
        ),
    ];
    for c in cases {
        let got = holds(SET_DESIRED_IF, &context(c.outcome, c.run));
        assert_eq!(got, Ok(c.runs), "{}", c.name);
    }
}
