//! The write side of the service manager that linking needs: `daemon-reload` after links change,
//! and the stop of a unit a version no longer lists; and what a hookless `force` needs besides
//! (design section 5.6, steps 9, 14 and 15): the start of a unit and whether it is active. It is
//! not the service observer that reports units, and this module depends on nothing but `std` and
//! `thiserror`, so a launchd implementation can implement [`UnitControl`] alone.
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

    /// Starts the unit `unit`; a unit that is already active is no error.
    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError>;

    /// Whether the unit `unit` is active now.
    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError>;
}

/// Why a command a [`CommandLauncher`] ran did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// The command ran to its end and exited with this code, which is not 0.
    #[error("exit status {0}")]
    Exit(i32),
    /// The command did not exit with a code: it could not start, passed its deadline, or was
    /// ended by a signal; the text says which.
    #[error("{0}")]
    Failed(String),
}

/// Runs one command, given as its argument vector, to its end. An exit that is not 0 is
/// [`LaunchError::Exit`] with its code, which a caller may act on; a command that cannot start,
/// passes its deadline or is ended by a signal is [`LaunchError::Failed`] saying which.
pub trait CommandLauncher {
    fn run(&self, argv: &[&str]) -> Result<(), LaunchError>;
}

/// The exit code of `systemctl stop` for a unit systemd has not loaded, so that nothing of it
/// runs: "Unit ... not loaded", systemd's `EXIT_NOTINSTALLED` for its bus error `NoSuchUnit`.
/// Every other failure of a stop has another code (4 for no permission, 1 for the rest).
const SYSTEMCTL_NOT_LOADED: i32 = 5;

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
        self.launcher
            .run(argv)
            .map_err(|error| failure(argv, &error))
    }
}

/// The error of the command `argv`, which the launcher ended with `error`.
fn failure(argv: &[&str], error: &LaunchError) -> UnitControlError {
    UnitControlError {
        action: argv.join(" "),
        why: error.to_string(),
    }
}

impl<L: CommandLauncher> UnitControl for Systemctl<L> {
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        self.run(&["systemctl", "daemon-reload"])
    }

    /// `systemctl stop -- <unit>`. Its exit 5 is no error: systemd has not loaded the unit (the
    /// run unit of a first install whose package was never installed), so it is not running.
    /// Every other failure is.
    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        let argv = ["systemctl", "stop", "--", unit];
        match self.launcher.run(&argv) {
            Ok(()) | Err(LaunchError::Exit(SYSTEMCTL_NOT_LOADED)) => Ok(()),
            Err(error) => Err(failure(&argv, &error)),
        }
    }

    /// `systemctl start -- <unit>`.
    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.run(&["systemctl", "start", "--", unit])
    }

    /// `systemctl is-active --quiet -- <unit>`: exit 0 is active. Any other end, a command that
    /// could not run included, reads as not active: the launcher does not tell the two apart,
    /// and a caller that waits for a unit keeps asking until its own bound.
    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        Ok(self
            .run(&["systemctl", "is-active", "--quiet", "--", unit])
            .is_ok())
    }
}
