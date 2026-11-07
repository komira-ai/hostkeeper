//! `pr-jobs --changed <file> --output <file>`: which jobs of `.github/workflows/pr-device.yml`
//! a pull request needs, from the paths it changes.
//!
//! That workflow runs on every pull request. Its job `changes` writes the paths the pull
//! request changes, one per entry (`git diff --name-only --no-renames -z` of the merge commit
//! against its first parent, the base), into `--changed`, and this verb reads them and appends
//! two outputs to `--output` (on Actions, the file `GITHUB_OUTPUT` names):
//!
//! - `mac=true` when a changed path is on [`MAC`], the Mac and service-manager surface: the
//!   jobs `macos-test` (the workspace tests on macOS) and `macos-device` (the device tests as
//!   root under launchd) run; `mac=false` otherwise, and they are skipped.
//! - `linux-privileged=true` when a changed path is on [`LINUX_PRIVILEGED`], the step runner, the
//!   systemd adapter and self-update: the job `privileged` (the privileged tests as root against
//!   systemd) runs; `linux-privileged=false` otherwise.
//!
//! An entry of a list ending in `/` is a directory and matches every path under it; any other
//! entry matches a path that starts with it (`crates/xtask/src/release/macos_pkg` is the file
//! `macos_pkg.rs`, the directory `macos_pkg/` and `macos_pkg_verb_tests.rs`). A path changed by a
//! rename is listed under its old name and its new one (`--no-renames`), so a file moved off a
//! list still selects the jobs. Entries are separated by NUL or a newline; empty ones are
//! skipped. Nothing here reads git, the network or the environment.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The Mac and service-manager surface: a change here runs the macOS jobs. The launchd adapters
/// and the Mac's platform names, the daemon (the step runner, the executor, self-update and the
/// boot pass), the step launcher, placement, the package check, the observer and the enrollment
/// claims, the `hostkeeper` binary (the pkg's postinstall), hello (the device test's package),
/// the privileged tests, the Mac packaging, the workflows that build or check the Mac, this
/// filter, and the build's own configuration (the cargo config and the toolchain).
pub(crate) const MAC: &[&str] = &[
    ".cargo/",
    ".github/workflows/artifacts.yml",
    ".github/workflows/pr-device.yml",
    ".github/workflows/privileged.yml",
    "crates/hostkeeper-config/src/platform.rs",
    "crates/hostkeeper-daemon/",
    "crates/hostkeeper-enroll/",
    "crates/hostkeeper-launchd/",
    "crates/hostkeeper-observe/",
    "crates/hostkeeper-placed/",
    "crates/hostkeeper-privileged/",
    "crates/hostkeeper-steps/",
    "crates/hostkeeper-unpack/",
    "crates/hostkeeper/",
    "crates/xtask/src/pr_jobs",
    "crates/xtask/src/release/macos_pkg",
    "examples/hello/",
    "rust-toolchain.toml",
];

/// The step runner, the systemd adapter and self-update: a change here runs the privileged tests
/// against systemd. The daemon, the step launcher (and its systemd adapter), placement, the
/// observer's systemctl and `/proc` adapters, the privileged tests, the Linux units of the
/// package (self-update's unit, drop-in and fallback), the workflows that run them, this filter,
/// and the build's own configuration.
pub(crate) const LINUX_PRIVILEGED: &[&str] = &[
    ".cargo/",
    ".github/workflows/pr-device.yml",
    ".github/workflows/privileged.yml",
    "crates/hostkeeper-daemon/",
    "crates/hostkeeper-observe/",
    "crates/hostkeeper-placed/",
    "crates/hostkeeper-privileged/",
    "crates/hostkeeper-steps/",
    "crates/xtask/src/pr_jobs",
    "packaging/linux/",
    "rust-toolchain.toml",
];

/// Why the verb failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    #[error("read the changed paths {path:?}: {why}")]
    Read { path: PathBuf, why: String },
    #[error("append the outputs to {path:?}: {why}")]
    Write { path: PathBuf, why: String },
}

/// The changed paths of `changed` on `list`, in the order they were given.
pub(crate) fn on_list<'a>(list: &[&str], changed: &'a str) -> Vec<&'a str> {
    changed
        .split(['\0', '\n'])
        .filter(|path| !path.is_empty())
        .filter(|path| list.iter().any(|entry| path.starts_with(entry)))
        .collect()
}

/// The output `name` for `found` (the paths that select its jobs), and the line that says why.
fn decision(name: &str, found: &[&str]) -> (String, String) {
    let selected = !found.is_empty();
    let why = if selected {
        format!("{name}: true, for {}", found.join(", "))
    } else {
        format!("{name}: false, no changed path is on its list")
    };
    (format!("{name}={selected}\n"), why)
}

/// Reads the changed paths in `changed`, appends `mac=` and `linux-privileged=` to `output`, and
/// returns one line per output saying why.
pub(crate) fn pr_jobs(changed: &Path, output: &Path) -> Result<Vec<String>, Error> {
    let text = std::fs::read_to_string(changed).map_err(|e| Error::Read {
        path: changed.to_owned(),
        why: e.to_string(),
    })?;
    let mut outputs = String::new();
    let mut lines = Vec::new();
    for (name, list) in [("mac", MAC), ("linux-privileged", LINUX_PRIVILEGED)] {
        let (out, why) = decision(name, &on_list(list, &text));
        outputs.push_str(&out);
        lines.push(why);
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(output)
        .and_then(|mut file| file.write_all(outputs.as_bytes()))
        .map_err(|e| Error::Write {
            path: output.to_owned(),
            why: e.to_string(),
        })?;
    Ok(lines)
}

#[cfg(test)]
mod tests;
