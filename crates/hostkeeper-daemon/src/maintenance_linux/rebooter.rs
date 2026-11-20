//! The reboot of a Linux host ([`HostRebooter`]): `systemctl reboot` under systemd, OpenRC's
//! `reboot` under OpenRC. The program runs as root through a `Launchctl` port (an empty
//! environment, no standard input, a deadline, capped output). It returns once the host has been
//! asked to reboot; the hook then ends, and the next `cutover` runs in the next boot.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};

use crate::login_account::run_ok;
use crate::maintenance_hook::{RebootError, Rebooter};

/// The program that reboots a host under systemd.
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// The program that reboots a host under OpenRC.
pub const REBOOT: &str = "/sbin/reboot";

/// How long the program that asks for the reboot is given.
pub const REBOOT_DEADLINE: Duration = Duration::from_secs(60);

/// The step's name in a failed reboot's reason, under systemd.
const SYSTEMD_STEP: &str = "systemctl reboot";

/// The step's name in a failed reboot's reason, under OpenRC.
const OPENRC_STEP: &str = "reboot";

/// The arguments of `systemctl` that reboot the host.
const SYSTEMD_ARGS: &[&str] = &["reboot"];

/// The arguments of OpenRC's `reboot`: none.
const OPENRC_ARGS: &[&str] = &[];

/// The reboot of a Linux host: one program, run with fixed arguments.
pub struct HostRebooter {
    tool: Box<dyn Launchctl>,
    what: &'static str,
    args: &'static [&'static str],
}

impl HostRebooter {
    /// The reboot through `tool`, a `systemctl`, run with `reboot`.
    #[must_use]
    pub fn systemd(tool: Box<dyn Launchctl>) -> HostRebooter {
        HostRebooter {
            tool,
            what: SYSTEMD_STEP,
            args: SYSTEMD_ARGS,
        }
    }

    /// The reboot through `tool`, OpenRC's `reboot`, run with no arguments.
    #[must_use]
    pub fn openrc(tool: Box<dyn Launchctl>) -> HostRebooter {
        HostRebooter {
            tool,
            what: OPENRC_STEP,
            args: OPENRC_ARGS,
        }
    }

    /// The reboot of this host's `platform`, as root, bounded by [`REBOOT_DEADLINE`]: [`SYSTEMCTL`]
    /// under systemd, [`REBOOT`] under OpenRC; none on a Mac, which reboots through its own
    /// install.
    #[must_use]
    pub fn for_platform(platform: PlatformKind) -> Option<HostRebooter> {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), REBOOT_DEADLINE))
        };
        match platform {
            PlatformKind::Linux => Some(HostRebooter::systemd(tool(SYSTEMCTL))),
            PlatformKind::Openrc => Some(HostRebooter::openrc(tool(REBOOT))),
            PlatformKind::Macos => None,
        }
    }
}

impl Rebooter for HostRebooter {
    /// Runs the program; exit 0 is the reboot asked for. Any other end is
    /// [`RebootError::Failed`], naming the program and its exit and the first line of its
    /// standard error, or why it did not run.
    fn reboot(&self) -> Result<(), RebootError> {
        run_ok(self.tool.as_ref(), self.what, self.args)
            .map(|_| ())
            .map_err(RebootError::Failed)
    }
}

#[cfg(test)]
#[path = "rebooter_tests.rs"]
mod tests;
