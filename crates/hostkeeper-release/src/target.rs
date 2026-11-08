//! What the coordinator checks of the targets of a `SetDesired` (packages.md section 3.4), every
//! refusal `target_invalid`: the platforms, each list, and each entry with its manifest by the
//! rules that need no host and no knowledge of the entry's installer ([`crate::manifest::check`]).
//!
//! The coordinator does not check that it knows an installer: a name of the grammar no daemon
//! lists is never picked. What the coordinator's state decides (a set with `package_digest` too,
//! an index that disagrees, a version set before with other targets) is the coordinator's own.
//!
//! Platforms are checked in the order of their spellings and entries in list order, so a set
//! that breaks several rules is always refused for the same one.

use std::collections::BTreeMap;

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::{Platform, UnknownPlatform};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Target, TargetList};
use prost::Message as _;

use crate::grammar::{LocatorError, check_locator, is_installer_name, is_sha256, is_store_name};
use crate::installer::Installer;
use crate::manifest::{self, ManifestError};
use crate::{
    MAX_ATTESTATION_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_LOCATIONS, MAX_LOCATOR_BYTES_PER_ENTRY,
};

/// The service hostkeeper updates itself as: its release is one `hostkeeper-archive` entry per
/// platform, so every daemon, the oldest included, can fetch the next one.
pub const SELF_SERVICE: &str = "hostkeeper";

/// Why a set's targets are refused (`target_invalid`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    /// No platform has a list.
    #[error("the targets name no platform")]
    NoPlatform,
    /// A key is not one of the three platforms.
    #[error("the targets key {0}")]
    UnknownPlatform(UnknownPlatform),
    /// A list has no entry, or more than [`MAX_ENTRIES`].
    #[error("the list of {platform} has {count} entries, not 1 to 8")]
    Entries {
        /// The platform.
        platform: Platform,
        /// How many entries it has.
        count: usize,
    },
    /// Two entries of one list name one installer: the second could never be picked.
    #[error("the list of {platform} has two entries of the installer {installer:?}")]
    DuplicateInstaller {
        /// The platform.
        platform: Platform,
        /// The installer.
        installer: String,
    },
    /// The service `hostkeeper` with a list but one `hostkeeper-archive` entry.
    #[error(
        "the list of {platform} for the service hostkeeper is not one hostkeeper-archive entry"
    )]
    SelfNotArchive {
        /// The platform.
        platform: Platform,
    },
    /// An entry breaks a rule.
    #[error("entry {at} of {platform}: {why}")]
    Entry {
        /// The platform.
        platform: Platform,
        /// The entry's place in the list, from 0.
        at: usize,
        /// The rule it breaks.
        why: EntryError,
    },
}

/// Why one entry is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryError {
    /// Over [`MAX_ENTRY_BYTES`] encoded.
    #[error("the entry is {0} bytes encoded, more than 24576")]
    TooLarge(usize),
    /// The installer is not 1 to 32 bytes of `[a-z0-9-]`.
    #[error("the installer {0:?} is not 1 to 32 bytes of a-z, 0-9 and `-`")]
    Installer(String),
    /// The artifact's or the attestation's digest is not 64 lower-case hex characters.
    #[error("the {of} digest {digest:?} is not 64 lower-case hex characters")]
    Digest {
        /// `artifact` or `attestation`.
        of: &'static str,
        /// The digest.
        digest: String,
    },
    /// The artifact's size is 0.
    #[error("the artifact's bytes is 0")]
    ZeroBytes,
    /// No attestation.
    #[error("the entry has no attestation")]
    NoAttestation,
    /// The attestation's size is 0 or over [`MAX_ATTESTATION_BYTES`].
    #[error("the attestation's bytes is {0}, not 1 to 1048576")]
    AttestationBytes(u64),
    /// The artifact or the attestation has no location, or more than [`MAX_LOCATIONS`].
    #[error("the {of} has {count} locations, not 1 to 8")]
    Locations {
        /// `artifact` or `attestation`.
        of: &'static str,
        /// How many it has.
        count: usize,
    },
    /// A location's store is not a store's name.
    #[error("the store {0:?} is not 1 to 32 bytes of a-z, 0-9 and `-`")]
    Store(String),
    /// A location's locator breaks the grammar of section 7.1.
    #[error("the locator {locator:?} {why}")]
    Locator {
        /// The locator.
        locator: String,
        /// The rule it breaks.
        why: LocatorError,
    },
    /// The locators of the entry, the artifact's and the attestation's, over
    /// [`MAX_LOCATOR_BYTES_PER_ENTRY`] bytes in all.
    #[error("the entry's locators are {0} bytes in all, more than 2048")]
    LocatorBytes(usize),
    /// No manifest.
    #[error("the entry has no manifest")]
    NoManifest,
    /// The manifest breaks a rule.
    #[error("{0}")]
    Manifest(ManifestError),
}

/// Checks the targets of a set for `service`: at least one platform, each key a platform, each
/// list 1 to 8 entries with no installer twice, every entry by [`check_entry`], and for the
/// service `hostkeeper` one `hostkeeper-archive` entry per list.
pub fn check<'a>(
    service: &ServiceName,
    targets: impl IntoIterator<Item = (&'a String, &'a TargetList)>,
) -> Result<(), TargetError> {
    let mut lists = BTreeMap::new();
    for (key, list) in targets {
        let platform = key
            .parse::<Platform>()
            .map_err(TargetError::UnknownPlatform)?;
        lists.insert(platform, list);
    }
    if lists.is_empty() {
        return Err(TargetError::NoPlatform);
    }
    for (platform, list) in lists {
        check_list(service, platform, &list.entries)?;
    }
    Ok(())
}

fn check_list(
    service: &ServiceName,
    platform: Platform,
    entries: &[Target],
) -> Result<(), TargetError> {
    if !(1..=MAX_ENTRIES).contains(&entries.len()) {
        return Err(TargetError::Entries {
            platform,
            count: entries.len(),
        });
    }
    for (at, entry) in entries.iter().enumerate() {
        check_entry(service, entry).map_err(|why| TargetError::Entry { platform, at, why })?;
        if entries[..at].iter().any(|e| e.installer == entry.installer) {
            return Err(TargetError::DuplicateInstaller {
                platform,
                installer: entry.installer.clone(),
            });
        }
    }
    let archive = Installer::HostkeeperArchive.as_str();
    let only_archive = matches!(entries, [one] if one.installer == archive);
    if (service.as_str() == SELF_SERVICE) & !only_archive {
        return Err(TargetError::SelfNotArchive { platform });
    }
    Ok(())
}

/// Checks one entry: at most 24 KiB encoded; its installer's name; the artifact's digest,
/// size and 1 to 8 locations; an attestation of 1 byte to 1 MiB with its digest and 1 to 8
/// locations; every location's store and locator; at most 2 KiB of locators in all; and its
/// manifest by [`manifest::check`].
pub fn check_entry(service: &ServiceName, entry: &Target) -> Result<(), EntryError> {
    let bytes = entry.encoded_len();
    if bytes > MAX_ENTRY_BYTES {
        return Err(EntryError::TooLarge(bytes));
    }
    if !is_installer_name(&entry.installer) {
        return Err(EntryError::Installer(entry.installer.clone()));
    }
    digest("artifact", &entry.digest)?;
    if entry.bytes == 0 {
        return Err(EntryError::ZeroBytes);
    }
    locations("artifact", &entry.locations)?;
    let attestation = entry
        .attestation
        .as_ref()
        .ok_or(EntryError::NoAttestation)?;
    check_attestation(attestation)?;
    let total: usize = entry
        .locations
        .iter()
        .chain(&attestation.locations)
        .map(|location| location.locator.len())
        .sum();
    if total > MAX_LOCATOR_BYTES_PER_ENTRY {
        return Err(EntryError::LocatorBytes(total));
    }
    let manifest = entry.manifest.as_ref().ok_or(EntryError::NoManifest)?;
    manifest::check(service, manifest).map_err(EntryError::Manifest)
}

fn check_attestation(attestation: &Blob) -> Result<(), EntryError> {
    digest("attestation", &attestation.digest)?;
    if !(1..=MAX_ATTESTATION_BYTES).contains(&attestation.bytes) {
        return Err(EntryError::AttestationBytes(attestation.bytes));
    }
    locations("attestation", &attestation.locations)
}

fn digest(of: &'static str, digest: &str) -> Result<(), EntryError> {
    if is_sha256(digest) {
        Ok(())
    } else {
        Err(EntryError::Digest {
            of,
            digest: digest.to_owned(),
        })
    }
}

fn locations(of: &'static str, locations: &[Location]) -> Result<(), EntryError> {
    if !(1..=MAX_LOCATIONS).contains(&locations.len()) {
        return Err(EntryError::Locations {
            of,
            count: locations.len(),
        });
    }
    for location in locations {
        if !is_store_name(&location.store) {
            return Err(EntryError::Store(location.store.clone()));
        }
        check_locator(&location.locator).map_err(|why| EntryError::Locator {
            locator: location.locator.clone(),
            why,
        })?;
    }
    Ok(())
}
