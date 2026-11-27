//! The two checks of the farm lane (`.github/workflows/farm.yml`, docs/farm.md) that are not
//! builds. The builds themselves, of the `buildrules` command line and of `//:ci`, are steps of
//! komira's buck2-farm action, which runs buck2, ends and runs again a run that hangs on a dead
//! connection to the farm or fails on a cut one, and fails a run whose summary does not say
//! `local: 0`.
//!
//! - `farm-workers --buck2 <path>` ([`workers`]): `buck2 audit config
//!   buildrules_re.linux_x86_64_properties --style json` must give the key a value that is not
//!   blank: the worker property set farm-connect wrote, which `.buckconfig` hands to buildrules.
//!   Without one buck2 has no execution platform, and this says so before anything is built. It
//!   reads buck2's configuration and builds nothing.
//! - `farm-table --buck2 <path> --buildrules <path> --patch <path> --summary <path> --repo
//!   <owner/name> --run-id <id>` ([`table::ensure_fresh`]): the generated build table must be
//!   fresh, by `buildrules check` of the command line the step before built. When it calls the
//!   table stale, the job runs `buildrules sync`, writes what sync changed as a patch to
//!   `--patch` (the workflow uploads it as the artifact [`table::PATCH_ARTIFACT`]) and the stale
//!   inputs, the changed files and the one command that applies the patch to `--summary` (the
//!   job summary), and fails without building.
//!
//! Neither stops buck2's daemon: the job's last step does, however the job went.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub(crate) mod table;

pub(crate) use table::Handoff;

/// The key of the worker property set buildrules reads.
pub(crate) const PROPERTIES_KEY: &str = "buildrules_re.linux_x86_64_properties";

/// What `farm-workers` prints when the key is set.
pub(crate) const WORKERS_SET: &str =
    "buildrules_re.linux_x86_64_properties is set: buck2 has the farm's workers";

/// What `farm-table` prints when the table is fresh.
pub(crate) const TABLE_FRESH: &str = "the generated build table is fresh";

/// Why a check of the farm lane failed.
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
    #[error(transparent)]
    Table(#[from] table::Error),
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

/// The error for a program that could not be started or waited for.
fn start_error(program: &Path) -> impl FnOnce(std::io::Error) -> Error {
    move |e| Error::Start {
        program: program.to_owned(),
        why: e.to_string(),
    }
}

/// `program <args>` run to its end, its standard input closed.
fn output(program: &Path, args: &[&str]) -> Result<Output, Error> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(start_error(program))
}

/// Every line `from` gives, written to `log` as it comes.
fn tee(from: impl std::io::Read, log: &mut dyn Write) {
    let mut lines = BufReader::new(from);
    let mut line = Vec::new();
    // A read that fails ends the log; the status of the command is still its own.
    while lines.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
        // A log that cannot be written loses the command's words, not the verdict.
        let _ = log.write_all(&line);
        line.clear();
    }
}

/// Fails unless buck2 at `buck2` has the farm's worker property set: [`WORKERS_SET`], or why not.
pub(crate) fn workers(buck2: &Path) -> Result<String, Error> {
    let audit = output(
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
    Ok(WORKERS_SET.to_owned())
}

/// Fails unless the generated build table is fresh by the `buildrules` command line at `tool`,
/// leaving its fix where `handoff` says when it is stale; what `buildrules sync` prints goes to
/// `log`. [`TABLE_FRESH`] when it is fresh.
pub(crate) fn fresh_table(
    tool: &Path,
    buck2: &Path,
    handoff: &Handoff,
    log: &mut dyn Write,
) -> Result<String, Error> {
    table::ensure_fresh(tool, buck2, handoff, log)?;
    Ok(TABLE_FRESH.to_owned())
}

#[cfg(test)]
mod tests;
