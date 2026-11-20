//! A group's maintenance settings as a run of one host reads them
//! (`docs/operations/hostgroups.md`, Maintenance): every key of the group's `maintenance` table
//! ([`crate::maintenance_policy`]) with its default resolved, the platform's `return_secs`
//! included, so that a group with no table and a group that names every default read the same.
//!
//! The maintenance runs are its callers; until they are built only its tests
//! call it.

use hostkeeper_wire::Platform;

use crate::host_groups::HostGroup;
use crate::maintenance_policy::{LinuxScope, XcodeDefault};

/// How long a rebooting Linux host may be silent when its group names no `return_secs`.
pub(crate) const LINUX_RETURN_SECS: u64 = 1200;

/// How long a rebooting Mac may be silent when its group names no `return_secs`.
pub(crate) const MAC_RETURN_SECS: u64 = 3600;

/// A group's maintenance settings for a host of one platform, every default resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Effective {
    /// How many of the newest Xcode releases the host keeps; 0 leaves Xcode alone.
    pub(crate) xcode_keep: u32,
    /// The Xcode `xcode-select` points at.
    pub(crate) xcode_default: XcodeDefault,
    /// Whether a new major macOS counts as due.
    pub(crate) macos_major: bool,
    /// Which Linux updates count as due.
    pub(crate) linux_scope: LinuxScope,
    /// Hosts of the group in maintenance at once, unless a run says otherwise.
    pub(crate) batch: u32,
    /// Hosts of the group that must stay serving.
    pub(crate) min_serving: u32,
    /// How long a drain may wait, in seconds; none: no deadline.
    pub(crate) drain_deadline_secs: Option<u64>,
    /// How long the host may be silent while it reboots, in seconds.
    pub(crate) return_secs: u64,
}

/// The maintenance settings of `group` for a host of `platform`: the group's table, each key it
/// leaves out at its default, a drain deadline of 0 as none, and a `return_secs` of 0 as
/// [`MAC_RETURN_SECS`] on a Mac and [`LINUX_RETURN_SECS`] on Linux.
pub(crate) fn effective(group: &HostGroup, platform: Platform) -> Effective {
    let table = &group.policy.maintenance;
    let platform_default = match platform {
        Platform::DarwinArm64 => MAC_RETURN_SECS,
        Platform::LinuxX86_64 | Platform::LinuxArm64 => LINUX_RETURN_SECS,
    };
    Effective {
        xcode_keep: table.xcode_keep,
        xcode_default: table.xcode_default.clone(),
        macos_major: table.macos_major,
        linux_scope: table.linux_scope,
        batch: table.batch,
        min_serving: table.min_serving,
        drain_deadline_secs: (table.drain_deadline_secs != 0).then_some(table.drain_deadline_secs),
        return_secs: if table.return_secs == 0 {
            platform_default
        } else {
            table.return_secs
        },
    }
}

#[cfg(test)]
#[path = "maintenance_effective_tests.rs"]
mod tests;
