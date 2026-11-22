//! The tests the farm leaves to the Mac. The farm lane (`farm.yml`, docs/farm.md) builds and tests
//! every farm-tested member for Linux x86_64 alone, so code under `cfg(target_os = "macos")`, and
//! every test of it or arm of a test behind it, is compiled out there: it is neither run nor failed.
//! A test that only makes sense on macOS (one that needs APFS, launchd or a Mac's own programs) is
//! written that way, and runs in release.yml's job `macos-test` ([`super::release_macos::TEST_JOB`])
//! on a Mac.
//!
//! [`problems`] holds the build files to saying so:
//! - every farm-tested member (no `ci = ["fmt", "lint"]` in its `[package.metadata.buildrules.actions]`)
//!   with such code has the line [`MARK`] in its BUCK file;
//! - every such member is one whose tests `macos-test` runs (its `cargo test --locked
//!   --no-fail-fast` names it with `-p`), so its Mac tests run somewhere;
//! - no other BUCK file has the line.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::lint::entry;
use super::release_macos::TEST_JOB;
use super::yaml::{self, Node, Value};

/// The line the BUCK file of a farm-tested member with macOS-only code holds.
pub const MARK: &str = "# The Linux farm compiles out this member's code and tests under cfg(target_os = \"macos\"); release.yml's job macos-test runs them on a Mac (docs/farm.md).";

/// What the lint knows of one workspace member.
pub struct Member {
    /// Its package name.
    pub name: String,
    /// Its BUCK file, relative to the repository (for the problems' words).
    pub buck_path: String,
    /// Whether `//:ci` runs its tests on the farm.
    pub farm_tested: bool,
    /// Whether any of its Rust files has code under `cfg(target_os = "macos")`.
    pub mac_only: bool,
    /// The text of its BUCK file.
    pub buck: String,
}

/// Whether the Rust text `text` has an item, a statement or a block that only a macOS build
/// compiles: a one-line `cfg` attribute that requires `target_os = "macos"` (spaces or none),
/// alone or in an `all(...)`, and neither negates it nor offers another platform in an `any(...)`.
/// An attribute split over lines is not read; every one in the tree is on one line, and
/// `the_committed_build_files_say_which_members_leave_tests_to_the_mac` pins the members found.
pub fn mac_only(text: &str) -> bool {
    text.lines().any(|line| {
        let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        (line.starts_with("#[cfg(") || line.starts_with("#![cfg("))
            && line.contains("target_os=\"macos\"")
            && !line.contains("not(")
            && !line.contains("any(")
    })
}

/// The packages the step of [`TEST_JOB`] in `release` (release.yml's text) that runs the tests
/// names with `-p`.
pub fn macos_tested(release: &str) -> BTreeSet<String> {
    let Ok(doc) = yaml::load(release) else {
        return BTreeSet::new();
    };
    let steps = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, TEST_JOB))
        .and_then(|job| entry(job, "steps"))
        .map(|s| &s.value);
    let Some(Value::Seq(steps)) = steps else {
        return BTreeSet::new();
    };
    let Some(run) = steps
        .iter()
        .filter_map(|step| entry(step, "run").and_then(Node::as_str))
        .find(|run| run.starts_with("cargo test --locked --no-fail-fast"))
    else {
        return BTreeSet::new();
    };
    let words: Vec<&str> = run.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[0] == "-p")
        .map(|pair| pair[1].to_owned())
        .collect()
}

/// What is wrong with `members` against the packages `tested` that `macos-test` runs.
pub fn problems(members: &[Member], tested: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for m in members {
        let marked = m.buck.lines().any(|line| line.trim() == MARK);
        let owes = m.farm_tested && m.mac_only;
        if owes && !marked {
            problems.push(format!(
                "{}: {} has code under cfg(target_os = \"macos\") that the Linux farm compiles out, \
                 and its BUCK file does not say so: add the line {MARK}",
                m.buck_path, m.name
            ));
        }
        if owes && !tested.contains(&m.name) {
            problems.push(format!(
                "{}: the Linux farm compiles out its macOS tests and release.yml's job {TEST_JOB} \
                 does not test it, so they run nowhere",
                m.name
            ));
        }
        if marked && !owes {
            problems.push(format!(
                "{}: says the farm leaves macOS tests of {} to the Mac, but the farm does not test \
                 it or it has no code under cfg(target_os = \"macos\")",
                m.buck_path, m.name
            ));
        }
    }
    problems
}

/// The `.rs` files under `dir`, but its `target/`.
fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                rust_files(&path, found);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// The members of the workspace at `root` (`crates/*` and `examples/*` that hold a
/// `Cargo.toml`), as the lint reads them.
pub fn members(root: &Path) -> Vec<Member> {
    let mut found = Vec::new();
    for parent in ["crates", "examples"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join(parent))
            .expect("list the members")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("a manifest");
            let table: toml::Table = manifest.parse().expect("a manifest that parses");
            let name = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
                .expect("a package name")
                .to_owned();
            let ci = table
                .get("package")
                .and_then(|p| p.get("metadata"))
                .and_then(|m| m.get("buildrules"))
                .and_then(|b| b.get("actions"))
                .and_then(|a| a.get("ci"))
                .and_then(toml::Value::as_array);
            let farm_tested = ci.is_none_or(|ci| ci.iter().any(|a| a.as_str() == Some("test")));
            let mut files = Vec::new();
            rust_files(&dir, &mut files);
            let mac_only = files
                .iter()
                .any(|f| mac_only(&std::fs::read_to_string(f).unwrap_or_default()));
            let rel = dir.strip_prefix(root).expect("under the root");
            found.push(Member {
                name,
                buck_path: format!("{}/BUCK", rel.display()),
                farm_tested,
                mac_only,
                buck: std::fs::read_to_string(dir.join("BUCK")).unwrap_or_default(),
            });
        }
    }
    found
}
