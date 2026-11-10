//! The rollback of hostkeeper below an installer of what the host holds (packages.md section 9.1,
//! "An earlier binary on a host's disk"; docs/design/packages-native.md, section 10.6): a release
//! of the service `hostkeeper` older than the binary that runs is refused, before anything is
//! staged or switched, `Failed`, `release`, [`REASON_ROLLBACK_BELOW_INSTALLER`], when it is older
//! than the first release that has an installer of a version the host holds, or than the first
//! release that updates hostkeeper itself through a native installer, on a host where hostkeeper
//! itself moved to one.
//!
//! The daemon knows what its disk holds: every service `placed.json` records with an installer
//! other than `hostkeeper-archive` (its `installed` entries), hostkeeper's own among them. The
//! release each installer arrived in is compiled in ([`INSTALLER_FLOORS`]); an installer with no
//! row is one a newer binary installed (a fallback put this one back), and every rollback is
//! below it. An operator who must go back further uninstalls those services first; hostkeeper
//! itself cannot be.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_placed::PlacedRecord;
use hostkeeper_release::Installer;

use crate::verbs::force::SELF_SERVICE;

/// The reason of the `Failed` of a rollback of hostkeeper below an installer of what the host
/// holds.
pub const REASON_ROLLBACK_BELOW_INSTALLER: &str = "rollback_below_installer";

/// The first release of hostkeeper whose daemon runs the steps of each installer: a binary
/// before it cannot stage, activate, roll back or remove a version of it. `hostkeeper-archive`
/// has none: every release reads it.
pub const INSTALLER_FLOORS: [(Installer, HostkeeperVersion); 2] = [
    (
        Installer::Apt,
        HostkeeperVersion {
            major: 0,
            minor: 1,
            patch: 211,
        },
    ),
    (
        Installer::MacosPkg,
        HostkeeperVersion {
            major: 0,
            minor: 1,
            patch: 223,
        },
    ),
];

/// The first release of hostkeeper that updates itself through a native installer: its new
/// binary installs its own package after it confirmed itself, and it runs the move and its
/// rollback (docs/design/packages-native.md, sections 10.4 and 10.6). A host where hostkeeper
/// itself runs from its native package is never handed back to a binary before it.
pub const SELF_NATIVE_FLOOR: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 212,
};

/// The first release that has `installer`; `None` for an installer this binary has no row of.
pub fn floor_of(installer: Installer) -> Option<HostkeeperVersion> {
    match installer {
        Installer::HostkeeperArchive => Some(HostkeeperVersion {
            major: 0,
            minor: 0,
            patch: 0,
        }),
        other => INSTALLER_FLOORS
            .iter()
            .find(|(row, _)| *row == other)
            .map(|(_, floor)| *floor),
    }
}

/// Why a release of hostkeeper is below what the host holds, in the words of the log; `None`
/// when it is not, or when it is no rollback: a release that is not older than `running`, or
/// whose version is not `major.minor.patch` (which the backward rule refuses on its own).
pub fn below(record: &PlacedRecord, running: HostkeeperVersion, target: &str) -> Option<String> {
    let target = HostkeeperVersion::parse(target).ok()?;
    if target >= running {
        return None;
    }
    if record
        .installer_of(SELF_SERVICE)
        .and_then(|name| name.parse::<Installer>().ok())
        .is_some_and(Installer::is_native)
        & (target < SELF_NATIVE_FLOOR)
    {
        return Some(format!(
            "{SELF_SERVICE} runs from its native package here, which no release before \
             {SELF_NATIVE_FLOOR} updates; {target} is older"
        ));
    }
    record.installed.iter().find_map(|(service, entry)| {
        let floor = entry.installer.parse::<Installer>().ok().and_then(floor_of);
        match floor {
            Some(floor) if target >= floor => None,
            Some(floor) => Some(format!(
                "{service} is installed by {}, which no release before {floor} has; {target} \
                 is older",
                entry.installer
            )),
            None => Some(format!(
                "{service} is installed by {:?}, which this release has no steps of; {target} \
                 is older than this release",
                entry.installer
            )),
        }
    })
}

#[cfg(test)]
#[path = "rollback_floor_tests.rs"]
mod tests;
