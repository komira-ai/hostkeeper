//! The check that a Mac's daemon keeps its own launchd job enabled.
//!
//! launchd keeps an override database of the system domain: a job marked disabled there is not
//! loaded at boot, so a Mac whose daemon's job (`dev.hostkeeper.hostkeeper`, [`Label::daemon`])
//! is marked disabled comes back from a reboot with no daemon and nothing on the host able to
//! say why. An installer, an operator's `launchctl disable`, or an earlier release's
//! self-update can leave that mark; it is not in the job's plist and no restart clears it.
//!
//! [`OwnJob::check`] reads the database (`launchctl print-disabled system`,
//! [`hostkeeper_launchd::launchctl::parse_disabled`]) and, when the daemon's job is marked
//! disabled, runs `launchctl enable system/dev.hostkeeper.hostkeeper`. A label the database does
//! not list is not disabled, and nothing is run for it. [`Ctx::check_own_job`] runs it and says
//! what it found: nothing at all when the job is enabled, and otherwise one alert line
//! ([`crate::boot::alert_line`], error level, with its `alert=` field), the same channel as
//! [`crate::own_package::ALERT_OWN_PACKAGE`]; the coordinator is not told:
//!
//! - [`ALERT_OWN_JOB_DISABLED`]: the job was marked disabled; the detail says whether it was
//!   enabled again or `launchctl enable` failed.
//! - [`ALERT_OWN_JOB_CHECK`]: the database could not be read (`launchctl` did not run to an exit,
//!   exited non-zero, or printed text that is not UTF-8), so nothing is known of the job.
//!
//! A job that is enabled again stays enabled, so the alert is raised once per mark and the next
//! check says nothing; a check that fails says so again at every check that fails.
//!
//! The daemon runs it once at its start ([`crate::start`]) and after each heartbeat
//! ([`crate::links::Links::start`]), on the heartbeat loop's worker. A check never ends the
//! start or the loop: each `launchctl` call is bounded by the deadline of its port, and every
//! failure is an alert and a return. Only a Mac's adapters provide an [`OwnJob`]
//! ([`crate::adapters::Adapters::own_job`]); on Linux, under systemd or OpenRC, none runs.

use std::sync::Arc;

use hostkeeper_launchd::launchctl::{enable_args, parse_disabled, print_disabled_args};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlOutput};

use crate::executor::Ctx;

/// The alert of a daemon's job found marked disabled in launchd's override database.
pub const ALERT_OWN_JOB_DISABLED: &str = "own_job_disabled";
/// The alert of a check that could not read launchd's override database.
pub const ALERT_OWN_JOB_CHECK: &str = "own_job_check";

/// What one check of the daemon's own job found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnJobCheck {
    /// The job is not marked disabled (enabled, or not listed): nothing was run but the read.
    Enabled,
    /// The job was marked disabled, and `launchctl enable` cleared the mark.
    EnabledAgain,
    /// The job was marked disabled, and it still is: `launchctl enable` failed, with this.
    EnableFailed(String),
    /// The database could not be read, with this.
    NotRead(String),
}

/// The daemon's own launchd job, checked through a `launchctl` port.
pub struct OwnJob {
    launchctl: Arc<dyn Launchctl>,
    label: Label,
}

impl OwnJob {
    /// The check of [`Label::daemon`] through `launchctl`, whose commands the caller bounds with
    /// a deadline ([`hostkeeper_launchd::LaunchctlProcess`]).
    pub fn new(launchctl: Arc<dyn Launchctl>) -> OwnJob {
        OwnJob {
            launchctl,
            label: Label::daemon(),
        }
    }

    /// The job's label.
    pub fn label(&self) -> &Label {
        &self.label
    }

    /// Reads launchd's override database, and enables the job when it is marked disabled there.
    pub fn check(&self) -> OwnJobCheck {
        let printed = match self.launchctl.run(&print_disabled_args()) {
            Ok(out) if out.code == Some(0) => String::from_utf8(out.stdout)
                .map_err(|_| "launchctl print-disabled wrote output that is not UTF-8".to_owned()),
            Ok(out) => Err(failure("launchctl print-disabled", &out)),
            Err(error) => Err(error.to_string()),
        };
        let printed = match printed {
            Ok(text) => text,
            Err(why) => return OwnJobCheck::NotRead(why),
        };
        if parse_disabled(&printed).get(self.label.as_str()) != Some(&true) {
            return OwnJobCheck::Enabled;
        }
        match self.launchctl.run(&enable_args(&self.label)) {
            Ok(out) if out.code == Some(0) => OwnJobCheck::EnabledAgain,
            Ok(out) => OwnJobCheck::EnableFailed(failure("launchctl enable", &out)),
            Err(error) => OwnJobCheck::EnableFailed(error.to_string()),
        }
    }
}

/// How `command` ended, for an exit that is not 0.
fn failure(command: &str, out: &LaunchctlOutput) -> String {
    let said = out.stderr_text();
    let said = said.trim();
    match (out.code, said.is_empty()) {
        (Some(code), true) => format!("{command} exited with {code}"),
        (Some(code), false) => format!("{command} exited with {code}: {said}"),
        (None, true) => format!("{command} was ended by a signal"),
        (None, false) => format!("{command} was ended by a signal: {said}"),
    }
}

impl Ctx {
    /// Checks the daemon's own launchd job ([`OwnJob::check`]) and alerts what it found: nothing
    /// for an enabled job; [`ALERT_OWN_JOB_DISABLED`] for one that was marked disabled;
    /// [`ALERT_OWN_JOB_CHECK`] for a database that could not be read. `None` where the host has
    /// no such check (Linux), and then nothing is run.
    pub fn check_own_job(&self) -> Option<OwnJobCheck> {
        let job = self.own_job.as_ref()?;
        let found = job.check();
        let label = job.label();
        match &found {
            OwnJobCheck::Enabled => {}
            OwnJobCheck::EnabledAgain => self.alert(
                ALERT_OWN_JOB_DISABLED,
                &format!(
                    "the launchd job {label} was marked disabled, so it would not have started \
                     after a reboot; it was enabled again"
                ),
            ),
            OwnJobCheck::EnableFailed(why) => self.alert(
                ALERT_OWN_JOB_DISABLED,
                &format!(
                    "the launchd job {label} is marked disabled, so it will not start after a \
                     reboot, and it could not be enabled: {why}"
                ),
            ),
            OwnJobCheck::NotRead(why) => self.alert(
                ALERT_OWN_JOB_CHECK,
                &format!(
                    "launchd's override database could not be read, so it is not known whether \
                     the job {label} is disabled: {why}"
                ),
            ),
        }
        Some(found)
    }
}
