//! The update hook of the `pxe` package (docs/design/two-programs.md, section 6.1): the steps
//! the host updater runs as `bin/pxe <step> [<staged release>] --params <file>`.
//!
//! The package's version directory is sealed read-only by the daemon, so the boot sets live
//! beside it, in the package param `boot_dir` (`/var/lib/pxe`): `<boot_dir>/<version>/` is what
//! [`crate::fetch`] writes and `serve` serves, and `<boot_dir>/current` is a symlink to the
//! version served, which the unit's `serve --dir` names. Each step except `verify` holds an
//! exclusive, non-blocking `flock` on `boot_dir` for its whole run; a held lock fails the step
//! with the reason `locked`. Every step can be run again.
//!
//! - `prepare` ([`prepare`]): checks the settings file, the daemon config and the whole `serve`
//!   command line they make, then fetches and seals `<boot_dir>/<version>`. A sealed directory
//!   of that version with a boot set for every platform asked for is left as it is. Nothing a
//!   client sees changes.
//! - `cutover`: points `current` at the version (a new symlink renamed over the old) and
//!   restarts the unit. The unit is `Type=notify` and `serve` says it is ready only after its
//!   whole start, so the restart returns once `serve` is serving or has refused.
//! - `verify`: the unit is active, or the step is deferred (exit 75, `retry_after_secs` 2).
//! - `rollback`: with a previous version whose directory is there, points `current` back at it
//!   and restarts the unit; otherwise (a first install) stops the unit (`systemctl stop` exits 5
//!   for a unit that is not loaded, which a first install's rollback meets, and 5 is accepted)
//!   and removes `current`. The target's directory is then removed unless it is the previous.
//! - `cleanup`: removes every entry of `boot_dir` but `current` and the target.
//! - `uninstall`: removes `boot_dir` whole (the daemon has stopped the unit and removes its
//!   links; the settings file is the operator's and stays).
//!
//! `prepare` is also given the staged release directory; it is parsed and not otherwise used, since
//! the boot sets are not part of the package.
//!
//! The package params are `release`, `settings`, `boot_dir` and `unit` ([`params`]); the
//! updater's `updater.target_version` and `updater.previous_version` name the versions.
//!
//! A step that fails or is deferred prints `pxe <step>: <text>` on standard error and one JSON
//! report as the last line of standard output ([`hostkeeper_hook::Report`]); one that succeeds
//! prints nothing on standard output. Exit 0 is success, 1 failure, 75 deferred.
//!
//! `systemctl` is run as `/usr/bin/systemctl` with an empty environment, through the
//! [`Systemctl`] port.

mod boot;
mod lock;
mod params;
pub(crate) mod prepare;
mod steps;
mod systemctl;

use std::io::Write;
use std::path::Path;

use hostkeeper_hook::{Class, Outcome, Params, Report, Step};

pub(crate) use prepare::{FetchJob, fetch_with};
pub(crate) use systemctl::{HostSystemctl, Systemctl};

use params::HookParams;

/// What the steps take from their host: the user the settings file, the daemon config, `gh` and
/// the trusted root must belong to (root), `systemctl`, and the download of the boot sets (run
/// as that user's files require).
pub(crate) struct Host<'a> {
    pub(crate) owner: u32,
    pub(crate) systemctl: &'a dyn Systemctl,
    pub(crate) fetch: &'a dyn Fn(&FetchJob, u32) -> Result<crate::seal::Seal, String>,
}

/// How a step ended other than in success: the exit status, the report and the text for the
/// operator.
#[derive(Debug)]
pub(crate) struct Failure {
    outcome: Outcome,
    report: Report,
    text: String,
}

impl Failure {
    fn new(outcome: Outcome, class: Class, reason: &str, text: String) -> Failure {
        Failure {
            outcome,
            report: Report {
                class,
                reason: Some(reason.to_owned()),
                detail: Some(text.clone()),
                ..Report::default()
            },
            text,
        }
    }

    /// A failure of the host or its settings, not of the release.
    fn environment(reason: &str, text: String) -> Failure {
        Failure::new(Outcome::Failure, Class::Environment, reason, text)
    }

    /// A filesystem error at `path`: the reason `io_failed`, of class `environment`.
    fn io(path: &Path, error: &std::io::Error) -> Failure {
        Failure::environment("io_failed", format!("{}: {error}", path.display()))
    }

    /// A failure of the release.
    fn release(reason: &str, text: String) -> Failure {
        Failure::new(Outcome::Failure, Class::Release, reason, text)
    }

    /// A step to run again later.
    fn deferred(reason: &str, retry_after_secs: u64, text: String) -> Failure {
        let mut failure = Failure::new(Outcome::Deferred, Class::Environment, reason, text);
        failure.report.retry_after_secs = Some(retry_after_secs);
        failure
    }
}

/// Runs `step` as the params file `params` says; returns the exit status. Human text goes to `err`, the JSON report of a failed or
/// deferred step to `out`.
pub(crate) fn run(
    host: &Host<'_>,
    step: Step,
    params: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match execute(host, step, params, err) {
        Ok(()) => 0,
        Err(failure) => {
            drop(writeln!(err, "pxe {step}: {}", failure.text));
            drop(failure.report.write_to(out));
            u8::try_from(failure.outcome.code()).unwrap_or(1)
        }
    }
}

/// Reads the params, then runs the step.
fn execute(
    host: &Host<'_>,
    step: Step,
    params_file: &Path,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let read = Params::read(params_file).map_err(|e| {
        Failure::environment("bad_params", format!("{}: {e}", params_file.display()))
    })?;
    let params = HookParams::of(&read, step)?;
    match step {
        Step::Prepare => prepare::prepare(host, &params, err),
        Step::Cutover => steps::cutover(host, &params),
        Step::Verify => steps::verify(host, &params),
        Step::Rollback => steps::rollback(host, &params),
        Step::Cleanup => steps::cleanup(&params),
        Step::Uninstall => steps::uninstall(&params),
    }
}

#[cfg(test)]
pub(crate) mod tests;
