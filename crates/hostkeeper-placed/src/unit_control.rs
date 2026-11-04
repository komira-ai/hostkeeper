//! The write side of the service manager that linking needs: `daemon-reload` after links change,
//! and the stop of a unit a version no longer lists. It is not the read-only service-manager
//! port that observes units, and this module depends on nothing but `std` and `thiserror`, so a
//! launchd implementation can implement [`UnitControl`] alone.
//!
//! [`Systemctl`] is the systemd implementation: one `systemctl` command per call, run through a
//! [`CommandLauncher`] the daemon gives it.

/// Why a call to the service manager failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{action} failed: {why}")]
pub struct UnitControlError {
    /// What was asked: `systemctl daemon-reload`, `systemctl stop <unit>`.
    pub action: String,
    pub why: String,
}

/// What linking asks of the service manager.
pub trait UnitControl {
    /// Makes the service manager read the unit directory again (`systemctl daemon-reload`).
    fn daemon_reload(&self) -> Result<(), UnitControlError>;

    /// Stops the unit `unit` and waits until it has stopped; a unit that is not running is no
    /// error.
    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError>;
}

/// Runs one command, given as its argument vector, to its end. An exit that is not 0, or a
/// command that cannot start, is an error saying what happened.
pub trait CommandLauncher {
    fn run(&self, argv: &[&str]) -> Result<(), String>;
}

/// [`UnitControl`] by `systemctl`, each call one command through the launcher.
#[derive(Debug)]
pub struct Systemctl<L> {
    launcher: L,
}

impl<L: CommandLauncher> Systemctl<L> {
    pub fn new(launcher: L) -> Systemctl<L> {
        Systemctl { launcher }
    }

    /// The launcher the commands run through.
    pub fn launcher(&self) -> &L {
        &self.launcher
    }

    fn run(&self, argv: &[&str]) -> Result<(), UnitControlError> {
        self.launcher.run(argv).map_err(|why| UnitControlError {
            action: argv.join(" "),
            why,
        })
    }
}

impl<L: CommandLauncher> UnitControl for Systemctl<L> {
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        self.run(&["systemctl", "daemon-reload"])
    }

    /// `systemctl stop -- <unit>`.
    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.run(&["systemctl", "stop", "--", unit])
    }
}
