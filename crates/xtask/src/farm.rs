//! `farm-build --buck2 <path> --target <label>`: the farm lane's build (`.github/workflows/farm.yml`,
//! docs/farm.md), one command. In this order:
//!
//! 1. `buck2 audit config buildrules_re.linux_x86_64_properties --style json` must give the key a
//!    value that is not blank: the worker property set farm-connect wrote, which `.buckconfig`
//!    hands to buildrules. Without one buck2 has no execution platform, and this says so first.
//! 2. `buck2 build <label>`. What buck2 prints on its standard error (its progress and its summary)
//!    is written to this program's standard error once buck2 exits; a failed build is a failure.
//! 3. The build's summary line, `Commands: N (cached: N, remote: N, local: N)`, must be there and
//!    say `local: 0`: every action ran on the farm or came from its cache, none on this machine.
//!    The line is what the verb prints.
//!
//! Whatever happened, it then runs `buck2 kill`, so no daemon outlives the job; that command's own
//! outcome changes nothing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The key of the worker property set buildrules reads.
pub(crate) const PROPERTIES_KEY: &str = "buildrules_re.linux_x86_64_properties";

/// Why a farm build failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    #[error("cannot run {program:?}: {why}")]
    Start { program: PathBuf, why: String },
    #[error("`buck2 audit config` failed ({status}): {stderr}")]
    Audit { status: String, stderr: String },
    #[error(
        "buildrules_re.linux_x86_64_properties is empty: farm-connect wrote no [komira_re] \
         linux_x86_64_properties, so buck2 has no farm to build on"
    )]
    NoWorkers,
    #[error("`buck2 build {target}` failed ({status})")]
    Build { target: String, status: String },
    #[error("the build printed no summary line (`Commands: N (cached: N, remote: N, local: N)`)")]
    NoSummary,
    #[error("`{0}`: actions ran on this machine, not on the farm (local must be 0)")]
    Local(String),
}

/// Whether `json`, what `buck2 audit config <key> --style json` printed, gives
/// [`PROPERTIES_KEY`] a value that is not blank.
pub(crate) fn has_value(json: &str) -> bool {
    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
        .ok()
        .and_then(|config| config.get(PROPERTIES_KEY).cloned())
        .and_then(|value| value.as_str().map(|v| !v.trim().is_empty()))
        .unwrap_or(false)
}

/// The last summary line of a build's output, from `Commands: `, and the number after its
/// `local: `; `None` when there is none.
pub(crate) fn summary(log: &str) -> Option<(&str, u64)> {
    log.lines().rev().find_map(|line| {
        let line = &line[line.find("Commands: ")?..];
        let (_, after) = line.split_once("local: ")?;
        let digits = after
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        Some((line, digits.parse().ok()?))
    })
}

/// Builds `target` with the buck2 at `buck2`, writing buck2's standard error to `log`, and
/// stops buck2's daemon after: the summary line, or why the build is not a farm build.
pub(crate) fn build(buck2: &Path, target: &str, log: &mut dyn Write) -> Result<String, Error> {
    let result = build_and_check(buck2, target, log);
    // The daemon outlives the command: stop it however the build ended.
    let _ = Command::new(buck2)
        .arg("kill")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    result
}

/// `buck2 <args>` run to its end, its standard input closed.
fn buck2_output(buck2: &Path, args: &[&str]) -> Result<Output, Error> {
    Command::new(buck2)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Start {
            program: buck2.to_owned(),
            why: e.to_string(),
        })
}

fn build_and_check(buck2: &Path, target: &str, log: &mut dyn Write) -> Result<String, Error> {
    let audit = buck2_output(
        buck2,
        &["audit", "config", PROPERTIES_KEY, "--style", "json"],
    )?;
    if !audit.status.success() {
        return Err(Error::Audit {
            status: audit.status.to_string(),
            stderr: String::from_utf8_lossy(&audit.stderr).trim().to_owned(),
        });
    }
    if !has_value(&String::from_utf8_lossy(&audit.stdout)) {
        return Err(Error::NoWorkers);
    }
    let built = buck2_output(buck2, &["build", target])?;
    // A log that cannot be written loses buck2's words, not the verdict.
    let _ = log.write_all(&built.stdout);
    let _ = log.write_all(&built.stderr);
    if !built.status.success() {
        return Err(Error::Build {
            target: target.to_owned(),
            status: built.status.to_string(),
        });
    }
    let text = String::from_utf8_lossy(&built.stderr);
    let (line, local) = summary(&text).ok_or(Error::NoSummary)?;
    if local != 0 {
        return Err(Error::Local(line.to_owned()));
    }
    Ok(line.to_owned())
}

#[cfg(test)]
mod tests;
