//! A Mac installs macOS updates only in a drained maintenance run: at its start a Mac's daemon
//! turns off macOS's own automatic install of macOS updates, as a built-in host setting with no
//! config key (every hostkeeper Mac's macOS updates are a maintenance run's, so there is no Mac
//! on which it should stay on).
//!
//! With `AutomaticallyInstallMacOSUpdates` of the Mac's own Software Update preferences
//! ([`DOMAIN`]) on, which is macOS's default, macOS installs an update and restarts the Mac by
//! itself, without a drain, in the middle of a build. The start reads the key with
//! `defaults read` ([`read_args`]); when the command exits 0 and prints exactly `0`, nothing more
//! is done. Otherwise (another value, a key that is not there, a read that failed) it runs
//! `defaults write` ([`write_args`]) to set the key to false, and when that succeeds says one
//! line ([`line`]) with the value the read gave (`unset` when it gave none). A read or a write
//! that cannot be run, is killed at its deadline or exits non-zero is logged as an error (only a
//! read that cannot be run, not one that exits non-zero, which is how a missing key answers); a
//! failed read does not stop the write, and a failed write leaves the start going on with the
//! host as it was.
//!
//! The Mac's daemon runs as root, which is all `defaults` needs to write the system preferences.
//! The key is set at each start of the daemon (a restart for an update included), not watched in
//! between; a managed value of the same key (device management's) is not touched.

use std::time::Duration;

use hostkeeper_launchd::{
    Launchctl, LaunchctlError, LaunchctlOutput, LaunchctlProcess, PlatformKind,
};

/// The tool that reads and writes the preference.
pub const DEFAULTS: &str = "/usr/bin/defaults";
/// The preferences domain of the Mac's own Software Update settings, as a path without `.plist`.
pub const DOMAIN: &str = "/Library/Preferences/com.apple.SoftwareUpdate";
/// The key that makes macOS install its updates by itself.
pub const KEY: &str = "AutomaticallyInstallMacOSUpdates";
/// How long `defaults` may run before it is killed.
pub const DEFAULTS_DEADLINE: Duration = Duration::from_secs(30);

/// The arguments of `defaults` that read the key.
pub fn read_args() -> Vec<String> {
    ["read", DOMAIN, KEY].map(str::to_owned).to_vec()
}

/// The arguments of `defaults` that set the key to false.
pub fn write_args() -> Vec<String> {
    ["write", DOMAIN, KEY, "-bool", "false"]
        .map(str::to_owned)
        .to_vec()
}

/// What the start changed: the key was not `0` and is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The value the read printed, trimmed; `unset` when the read exited non-zero or did not run.
    pub from: String,
}

/// The log line of a change.
pub fn line(change: &Change) -> String {
    format!(
        "level=info auto_install: {KEY} was {}, set to 0",
        change.from
    )
}

/// Why `defaults` made no change, in words for the log.
fn failure(what: &str, output: &LaunchctlOutput) -> String {
    format!(
        "level=error auto_install: defaults {what} ended with {:?}: {}",
        output.code,
        output.stderr_text().trim()
    )
}

/// Why `defaults` did not run to an exit, in words for the log.
fn not_run(what: &str, error: &LaunchctlError) -> String {
    let why = match error {
        LaunchctlError::Spawn { why, .. } => why.clone(),
        LaunchctlError::TimedOut { .. } => format!(
            "killed at its deadline of {} seconds",
            DEFAULTS_DEADLINE.as_secs()
        ),
    };
    format!("level=error auto_install: defaults {what} did not run: {why}")
}

/// The start's step: on a Mac, turns the key off unless it reads `0` and says what it did
/// through `say`; on any other host does nothing. Returns the change that was made, none when the
/// key was off already or the write failed.
pub fn at_start(
    kind: PlatformKind,
    defaults: &dyn Launchctl,
    say: &dyn Fn(&str),
) -> Option<Change> {
    if kind != PlatformKind::Macos {
        return None;
    }
    let from = match defaults.run(&read_args()) {
        Ok(output) if output.code == Some(0) => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if value == "0" {
                return None;
            }
            value
        }
        Ok(_) => "unset".to_owned(),
        Err(error) => {
            say(&not_run(&read_args().join(" "), &error));
            "unset".to_owned()
        }
    };
    let command = write_args();
    match defaults.run(&command) {
        Ok(output) if output.code == Some(0) => {
            let change = Change { from };
            say(&line(&change));
            Some(change)
        }
        Ok(output) => {
            say(&failure(&command.join(" "), &output));
            None
        }
        Err(error) => {
            say(&not_run(&command.join(" "), &error));
            None
        }
    }
}

/// [`at_start`] on the host's own `defaults`, saying through the daemon's stderr.
pub fn at_host_start(kind: PlatformKind) -> Option<Change> {
    let defaults = LaunchctlProcess::new(std::path::Path::new(DEFAULTS), DEFAULTS_DEADLINE);
    at_start(kind, &defaults, &|line| {
        eprintln!("hostkeeper daemon: {line}");
    })
}
