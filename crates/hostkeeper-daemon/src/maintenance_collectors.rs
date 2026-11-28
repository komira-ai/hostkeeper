//! The collectors of a maintenance look, by platform ([`crate::maintenance_collect::Collector`]):
//! each platform lists them in the order its look reads them.
//!
//! A Linux host, under systemd or OpenRC, reads the host's facts ([`HostFacts`]), whether it
//! needs a reboot ([`RebootNeed`]) and the updates its package manager lists: `apt-get` when the
//! host has it, else `dnf`, else `apk`; a host with none of the three has no package collector. A
//! Mac reads what it runs and which macOS updates are due ([`Macos`]), its boot id and boot time
//! ([`MacBoot`]), what an update needs of it
//! ([`MacReadiness`]), its Xcodes ([`Xcodes`]), its update account ([`UpdateAccountFacts`]) and the
//! device-management settings that conflict with a run ([`MdmConflicts`]).

use hostkeeper_launchd::PlatformKind;

use crate::maintenance_account::UpdateAccountFacts;
use crate::maintenance_apk::{self, Apk};
use crate::maintenance_apt::{self, Apt};
use crate::maintenance_boot::MacBoot;
use crate::maintenance_collect::{Collector, HostRoot};
use crate::maintenance_dnf::{self, Dnf};
use crate::maintenance_host::HostFacts;
use crate::maintenance_mac::MacReadiness;
use crate::maintenance_macos::Macos;
use crate::maintenance_mdm::MdmConflicts;
use crate::maintenance_reboot::RebootNeed;
use crate::maintenance_xcode::Xcodes;

/// The collectors of a host of `kind`, over the host's own programs and files, in the order its
/// look reads them; a Linux host's package manager waiting for a held lock for at most
/// `lock_wait_secs`.
#[must_use]
pub fn at_host(kind: PlatformKind, lock_wait_secs: u64) -> Vec<Box<dyn Collector>> {
    match kind {
        PlatformKind::Linux | PlatformKind::Openrc => {
            linux_under(&HostRoot::host(), lock_wait_secs)
        }
        PlatformKind::Macos => mac(),
    }
}

/// The collectors of a Mac, over its own programs and files: `macos`, its boot facts, `mac`, then
/// its Xcodes, its update account and its device-management conflicts.
#[must_use]
pub fn mac() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(Macos::at_host()),
        Box::new(MacBoot::at_host()),
        Box::new(MacReadiness::at_host()),
        Box::new(Xcodes::at_host()),
        Box::new(UpdateAccountFacts::at_host()),
        Box::new(MdmConflicts::at_host()),
    ]
}

/// The collectors of a Linux host: the host's facts, the reboot need, and the package manager
/// that is under `root` ([`package_manager`]), waiting for its lock for at most `lock_wait_secs`.
#[must_use]
pub fn linux_under(root: &HostRoot, lock_wait_secs: u64) -> Vec<Box<dyn Collector>> {
    let mut collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(HostFacts::at_host()),
        Box::new(RebootNeed::at_host()),
    ];
    collectors.extend(package_manager(root, lock_wait_secs));
    collectors
}

/// The collector of the package manager whose program is a file under `root`: `apt-get`, else
/// `dnf`, else `apk`; none when there is none of them. It waits for a held lock for at most
/// `lock_wait_secs` ([`crate::maintenance_collect::LockWait`]).
#[must_use]
pub fn package_manager(root: &HostRoot, lock_wait_secs: u64) -> Option<Box<dyn Collector>> {
    if root.is_file(maintenance_apt::APT_GET) {
        Some(Box::new(Apt::at_host(lock_wait_secs)))
    } else if root.is_file(maintenance_dnf::DNF) {
        Some(Box::new(Dnf::at_host(lock_wait_secs)))
    } else if root.is_file(maintenance_apk::APK) {
        Some(Box::new(Apk::at_host(lock_wait_secs)))
    } else {
        None
    }
}

#[cfg(test)]
#[path = "maintenance_collectors_tests.rs"]
mod tests;
