//! [`RcService`]: the [`UnitControl`] of an OpenRC host, one `rc-service` command per call, run
//! through a [`CommandLauncher`] the daemon gives it, as `hostkeeper_placed::Systemctl` runs
//! `systemctl`.
//!
//! - [`UnitControl::daemon_reload`] runs nothing: OpenRC reads an init script each time it runs
//!   it, and its dependency cache is made again by the next `rc-service` that finds a script newer
//!   than the cache.
//! - [`UnitControl::stop_unit`] is `rc-service <service> stop`, which returns once
//!   supervise-daemon (or start-stop-daemon) has seen the process end; a service that is stopped
//!   already exits 0.
//! - [`UnitControl::start_unit`] is `rc-service <service> start`; a service that is started
//!   already exits 0.
//! - [`UnitControl::is_active`] is `rc-service <service> status`: exit 0 is started. Any other
//!   end, a command that could not run included, reads as not active, as for `Systemctl`.
//!
//! The service is the init script's name, which `rc-service` takes as a word of its command line
//! with no `--` to end its options: a name that is not one [`service_name`] accepts is refused
//! before anything runs.

use hostkeeper_placed::{CommandLauncher, UnitControl, UnitControlError};

/// Where `rc-service` is on an OpenRC host.
pub const RC_SERVICE: &str = "/sbin/rc-service";
/// The longest service name: an init script's file name.
pub const MAX_SERVICE_NAME: usize = 255;

/// A unit that is not the name of an OpenRC service ([`service_name`]).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not the name of an OpenRC service")]
pub struct NotAServiceName(pub String);

/// `Ok` with `unit` when it is a name of an init script that `rc-service` reads as a service and
/// never as an option or a path: 1 to [`MAX_SERVICE_NAME`] bytes of ASCII letters, digits, `.`,
/// `_` and `-`, the first a letter or a digit.
pub fn service_name(unit: &str) -> Result<&str, NotAServiceName> {
    let first_ok = unit
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric());
    let rest_ok = unit
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    match first_ok && rest_ok && unit.len() <= MAX_SERVICE_NAME {
        true => Ok(unit),
        false => Err(NotAServiceName(unit.to_owned())),
    }
}

/// [`UnitControl`] by `rc-service`, each call one command through the launcher.
#[derive(Debug)]
pub struct RcService<L> {
    launcher: L,
}

impl<L: CommandLauncher> RcService<L> {
    pub fn new(launcher: L) -> RcService<L> {
        RcService { launcher }
    }

    /// The launcher the commands run through.
    pub fn launcher(&self) -> &L {
        &self.launcher
    }

    /// Runs `rc-service <unit> <verb>`, refusing a unit that is no service name.
    fn run(&self, unit: &str, verb: &str) -> Result<(), UnitControlError> {
        let argv = [RC_SERVICE, unit, verb];
        let error = |why| UnitControlError {
            action: argv.join(" "),
            why,
        };
        service_name(unit).map_err(|e| error(e.to_string()))?;
        self.launcher.run(&argv).map_err(|e| error(e.to_string()))
    }
}

impl<L: CommandLauncher> UnitControl for RcService<L> {
    /// Nothing to run: OpenRC has no unit cache to reload (the module's documentation).
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Ok(())
    }

    /// `rc-service <unit> stop`.
    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.run(unit, "stop")
    }

    /// `rc-service <unit> start`.
    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.run(unit, "start")
    }

    /// `rc-service <unit> status`: exit 0 is started; anything else is not active.
    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        Ok(self.run(unit, "status").is_ok())
    }
}
