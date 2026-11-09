//! The entry an order carries and the checks the daemon makes of it on receipt, before it asks
//! the package port for a byte (packages.md sections 3.3 and 4.4).
//!
//! The daemon decides the installers it announces once, at its start
//! ([`crate::executor::Ports::announcement`], [`hostkeeper_install::announce`]): the heartbeat
//! lists them and the receipt check below judges an entry against the same list, so the two
//! never differ. [`check`] runs in this order:
//!
//! | Check                                                                        | Answer                                                                     |
//! |------------------------------------------------------------------------------|----------------------------------------------------------------------------|
//! | the entry's digest is not the order's `package_digest`                       | refused `order_invalid`                                                    |
//! | a location of the artifact or the attestation breaks the grammar of a store name or of a locator | refused `order_invalid`                                        |
//! | the manifest needs a newer daemon (`min_hostkeeper`)                         | refused `daemon_too_old`                                                   |
//! | the installer is not one the daemon announces                                | `Deferred` for [`NOT_LISTED_RETRY_SECS`], reason `installer_not_listed`    |
//! | the manifest breaks a rule of its installer, or there is none                | refused `manifest_invalid`                                                 |
//!
//! A refusal is reported `failed` with its token; `order_invalid` and `manifest_invalid` are
//! reported of class `release`, before the commit point
//! ([`crate::link_adapters::blamed`]). A missing attestation is not `order_invalid`: the fetch judges it (`not_attested`). Nothing is
//! fetched and nothing is written for any answer here.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use hostkeeper_release::grammar::{check_locator, is_store_name};
use hostkeeper_release::installer::Installer;
use hostkeeper_release::receipt::{self, Daemon, ReceiptError};

use crate::executor::Reply;
use crate::refusal::Refusal;
use crate::verb::PackageDigest;

/// The seconds an order for an installer the daemon does not list is deferred for. One second keeps
/// the daemon's own deferral book from declining the same version's next entry, which the
/// coordinator picks again at the next beat.
pub const NOT_LISTED_RETRY_SECS: u64 = 1;

/// Why a location breaks the grammar, or `None` when it keeps it.
fn location_defect(location: &Location) -> Option<String> {
    if !is_store_name(&location.store) {
        return Some(format!(
            "the store name {:?} is not a store name",
            location.store
        ));
    }
    check_locator(&location.locator)
        .err()
        .map(|why| format!("the locator {:?} {why}", location.locator))
}

/// Checks the entry `entry` of an order for `service` whose package digest is `digest`, for a
/// daemon of version `running` that announces `installers`, as the table of the module says.
/// `Err` is the reply to give, with nothing fetched or written.
pub fn check(
    running: HostkeeperVersion,
    installers: &[Installer],
    service: &ServiceName,
    digest: &PackageDigest,
    entry: &Target,
) -> Result<(), Reply> {
    let invalid = |detail: String| Reply::refused(Refusal::OrderInvalid, detail);
    if entry.digest != digest.as_str() {
        return Err(invalid(format!(
            "the entry's digest {:?} is not the order's package_digest {digest}",
            entry.digest
        )));
    }
    let attestation = entry.attestation.iter().flat_map(|blob| &blob.locations);
    let defect = entry
        .locations
        .iter()
        .chain(attestation)
        .find_map(location_defect);
    if let Some(defect) = defect {
        return Err(invalid(format!(
            "the entry names a location where {defect}"
        )));
    }
    let daemon = Daemon {
        platform: Platform::current(),
        running,
        installers,
    };
    receipt::check(service, entry, daemon)
        .map(drop)
        .map_err(|error| match error {
            ReceiptError::DaemonTooOld { .. } => Reply::refused(Refusal::DaemonTooOld, error),
            ReceiptError::InstallerNotListed(_) => Reply::Finished {
                verdict: Verdict::Deferred {
                    retry_after_secs: Some(NOT_LISTED_RETRY_SECS),
                },
                reason: Some(Refusal::InstallerNotListed.token().to_owned()),
                resent: false,
            },
            ReceiptError::NoManifest | ReceiptError::Invalid(_) => {
                Reply::refused(Refusal::ManifestInvalid, error)
            }
        })
}
