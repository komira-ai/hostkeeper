//! What the daemon checks of the entry an order carries, on receipt and before it fetches a byte
//! (packages.md sections 4.4 and 3.3), in this order:
//!
//! 1. **Version.** `min_hostkeeper` is read first: a daemon older than it answers `Deferred`,
//!    `environment`, `daemon_too_old`, whatever else the manifest holds. A manifest a newer
//!    daemon reads may use a field or an installer this one does not know, so this one judges
//!    nothing else of it.
//! 2. **Installer.** The entry's installer is one this daemon lists, else `Deferred`,
//!    `environment`, `installer_not_listed`: the coordinator's copy of the list can be one
//!    restart stale, and nothing is wrong with the release.
//! 3. **Manifest.** Every other rule ([`crate::manifest::check_for`]): `Failed`, `release`,
//!    `manifest_invalid`.
//!
//! A manifest whose `min_hostkeeper` does not parse is `manifest_invalid`: there is no version
//! to be too old for.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::installer::Installer;
use crate::manifest::{self, ManifestError};

/// What the daemon brings to the check: its own version and the installers it announces.
#[derive(Clone, Copy, Debug)]
pub struct Daemon<'a> {
    /// The running daemon's version.
    pub running: HostkeeperVersion,
    /// The installers it lists in its heartbeat, decided once at its start.
    pub installers: &'a [Installer],
}

/// Why the daemon does not take an entry. [`ReceiptError::reason`] is the outcome's reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    /// The manifest needs a newer daemon: `Deferred`, `environment`.
    #[error("the manifest needs hostkeeper {needs}; this daemon is {running}")]
    DaemonTooOld {
        /// The manifest's `min_hostkeeper`.
        needs: HostkeeperVersion,
        /// The running daemon's version.
        running: HostkeeperVersion,
    },
    /// The entry's installer is not one the daemon lists: `Deferred`, `environment`.
    #[error("the entry's installer {0:?} is not one this daemon lists")]
    InstallerNotListed(String),
    /// The entry has no manifest: `Failed`, `release`.
    #[error("the entry has no manifest")]
    NoManifest,
    /// The manifest breaks a rule: `Failed`, `release`.
    #[error("{0}")]
    Invalid(ManifestError),
}

impl ReceiptError {
    /// The reason token of the outcome: `daemon_too_old`, `installer_not_listed` or
    /// `manifest_invalid`.
    pub const fn reason(&self) -> &'static str {
        match self {
            ReceiptError::DaemonTooOld { .. } => "daemon_too_old",
            ReceiptError::InstallerNotListed(_) => "installer_not_listed",
            ReceiptError::NoManifest | ReceiptError::Invalid(_) => "manifest_invalid",
        }
    }

    /// Whether the order is deferred, to be tried again, rather than failed: the entry is
    /// sound, and the daemon cannot take it now.
    pub const fn is_deferred(&self) -> bool {
        matches!(
            self,
            ReceiptError::DaemonTooOld { .. } | ReceiptError::InstallerNotListed(_)
        )
    }
}

/// Checks the entry `target` of an order for `service` against `daemon`, and answers its
/// installer.
pub fn check(
    service: &ServiceName,
    target: &Target,
    daemon: Daemon<'_>,
) -> Result<Installer, ReceiptError> {
    let manifest = target.manifest.as_ref().ok_or(ReceiptError::NoManifest)?;
    let needs = manifest::min_hostkeeper(manifest).map_err(ReceiptError::Invalid)?;
    if needs > daemon.running {
        return Err(ReceiptError::DaemonTooOld {
            needs,
            running: daemon.running,
        });
    }
    let installer = target
        .installer
        .parse::<Installer>()
        .ok()
        .filter(|installer| daemon.installers.contains(installer))
        .ok_or_else(|| ReceiptError::InstallerNotListed(target.installer.clone()))?;
    manifest::check_for(service, installer, manifest).map_err(ReceiptError::Invalid)?;
    Ok(installer)
}
