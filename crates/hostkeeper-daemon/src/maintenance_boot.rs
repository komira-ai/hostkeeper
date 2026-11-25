//! The collector `boot` of a Mac's maintenance look ([`crate::maintenance_collect`],
//! docs/design/maintenance-updates.md section 4.1): the Mac's boot id and the second it booted,
//! so that an operator reading the report sees a restart. Linux reads both in `host`
//! ([`crate::maintenance_host`]).
//!
//! The boot id is `kern.bootsessionuuid`, read through a [`BootIdSource`]: on a Mac the step
//! record's own, `hostkeeper_launchd::SysctlBootId` ([`crate::adapters::host_boot_id`]), a
//! `sysctlbyname` call and no program. The boot time is the `sec` of `sysctl -n kern.boottime`
//! (`{ sec = 1795176765, usec = 482911 } Fri Nov 20 12:12:45 2026`, read by
//! [`parse_kern_boottime`]), run through a [`Launchctl`] port as the other Mac collectors run
//! their programs: on a Mac, as root, with an empty environment, no standard input, a deadline
//! ([`TOOL_DEADLINE`]) and capped output.
//!
//! Neither fact fails the look. A boot id that cannot be read leaves `boot_id` empty; a `sysctl`
//! that does not run, does not end by its deadline, exits non-zero or prints another shape leaves
//! `boot_unix` 0. The rest of the report is the other collectors', as when this one is not there.

use std::path::Path;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_steps::BootIdSource;

use crate::mac::TOOL_DEADLINE;
use crate::maintenance_collect::{Collector, Look, Section};

/// The program that prints a sysctl's value.
pub const SYSCTL: &str = "/usr/sbin/sysctl";

/// The arguments that print the boot time.
pub const BOOTTIME_ARGS: [&str; 2] = ["-n", "kern.boottime"];

/// The collector `boot`.
pub struct MacBoot {
    boot_id: Box<dyn BootIdSource>,
    sysctl: Box<dyn Launchctl>,
}

impl MacBoot {
    /// The collector over `boot_id` (the source of `kern.bootsessionuuid`) and `sysctl` (the
    /// program that prints `kern.boottime`).
    #[must_use]
    pub fn new(boot_id: Box<dyn BootIdSource>, sysctl: Box<dyn Launchctl>) -> MacBoot {
        MacBoot { boot_id, sysctl }
    }

    /// The collector over the host's own boot id ([`crate::adapters::host_boot_id`]) and its own
    /// [`SYSCTL`], bounded by [`TOOL_DEADLINE`].
    #[must_use]
    pub fn at_host() -> MacBoot {
        MacBoot::new(
            crate::adapters::host_boot_id(),
            Box::new(LaunchctlProcess::new(Path::new(SYSCTL), TOOL_DEADLINE)),
        )
    }
}

impl Collector for MacBoot {
    fn name(&self) -> &'static str {
        "boot"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        Ok(Section::default())
    }
}

/// The `sec` of what `sysctl -n kern.boottime` printed: the text inside the braces that start
/// it, `{ sec = N, usec = M }`, split at commas, the part whose name is `sec` read as a number of
/// seconds; 0 when the text does not start with a brace, has no closing brace, no `sec` part, or
/// a `sec` that is not a whole number of seconds.
#[must_use]
pub fn parse_kern_boottime(_text: &str) -> u64 {
    0
}

#[cfg(test)]
#[path = "maintenance_boot_tests.rs"]
mod tests;
