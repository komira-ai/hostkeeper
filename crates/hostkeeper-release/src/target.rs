//! What the coordinator checks of the targets of a `SetDesired` (packages.md section 3.4), every
//! refusal `target_invalid`: the platforms, each list, and each entry with its manifest by the
//! rules that need no host and no knowledge of the entry's installer ([`crate::manifest::check`]).
//!
//! An entry is a repository entry when its `repository` is set, and an artifact entry when it is
//! not ([`is_repository_entry`]); its kind is told by that field alone, never by the installer's
//! name, so a later repository backend needs no change here (packages-repositories.md section
//! 2.1). An artifact entry is checked as packages.md section 3.4 has it, and must not carry a
//! `package_version`; a repository entry by section 2.2 of packages-repositories.md: its
//! repository, its package version, no artifact field, a required pin, and the package's name in
//! its manifest.
//!
//! A native entry ([`is_native_entry`], packages-native.md section 10) is a repository entry, or
//! an artifact entry whose installer is native (`macos-pkg`): its `hook.command` is empty or its program an
//! absolute path of a file the package installs, a `macos-pkg` entry carries no attestation, and the service `hostkeeper` is set with one native
//! entry per list and at most one `hostkeeper-archive` entry after it, or with that archive entry
//! alone until its own release lists the native one (section 10.4).
//!
//! The coordinator does not check that it knows an installer: a name of the grammar no daemon
//! lists is never picked, and its artifact entry is not native. What the coordinator's state decides (a set with `package_digest` too,
//! an index that disagrees, a version set before with other targets) is the coordinator's own.
//!
//! Platforms are checked in the order of their spellings and entries in list order, so a set
//! that breaks several rules is always refused for the same one.

use std::collections::BTreeMap;

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::{Platform, UnknownPlatform};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Target, TargetList};
use prost::Message as _;

use crate::grammar::{
    LocatorError, check_locator, is_installer_name, is_package_name, is_package_version,
    is_repository_name, is_sha256, is_store_name,
};
use crate::installer::Installer;
use crate::manifest::{self, ManifestError};
use crate::{
    MAX_ATTESTATION_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_LOCATIONS, MAX_LOCATOR_BYTES_PER_ENTRY,
};

/// The service hostkeeper updates itself as: its release is, per platform, one native entry with
/// at most one `hostkeeper-archive` entry after it, or that archive entry alone, so every daemon,
/// the oldest included, can fetch the next one.
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
    /// The service `hostkeeper` with a list that is neither one native entry with at most one
    /// `hostkeeper-archive` entry after it, nor one `hostkeeper-archive` entry: the archive entry
    /// first, two native entries, or an installer of another format.
    #[error(
        "the list of {platform} for the service hostkeeper is not one native entry with at most one hostkeeper-archive entry after it, nor one hostkeeper-archive entry"
    )]
    SelfList {
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
    /// The artifact's, the attestation's or a repository entry's package's digest is not 64
    /// lower-case hex characters (an absent one is empty).
    #[error("the {of} digest {digest:?} is not 64 lower-case hex characters")]
    Digest {
        /// `artifact`, `attestation` or `package`.
        of: &'static str,
        /// The digest.
        digest: String,
    },
    /// The artifact's size is 0.
    #[error("the artifact's bytes is 0")]
    ZeroBytes,
    /// No attestation, on an artifact entry that is not of `macos-pkg`.
    #[error("the entry has no attestation")]
    NoAttestation,
    /// A `macos-pkg` entry with an attestation: the package's signature and the pin are its
    /// trust (packages-native.md section 10.3), and an attestation nobody checks must not look
    /// checked.
    #[error(
        "the macos-pkg entry has an attestation; a package's signature and the pin are its trust, and no daemon checks one"
    )]
    MacosPkgAttestation,
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
    /// An artifact entry (no `repository`) with a `package_version`.
    #[error("the entry has a package_version and no repository")]
    VersionWithoutRepository,
    /// A repository entry whose `repository` is not 1 to 32 bytes of `[a-z0-9-]`.
    #[error("the repository {0:?} is not 1 to 32 bytes of a-z, 0-9 and `-`")]
    Repository(String),
    /// A repository entry with no `package_version`.
    #[error("the repository entry has no package_version")]
    NoPackageVersion,
    /// A repository entry whose `package_version` breaks its grammar.
    #[error(
        "the package_version {0:?} is not 1 to 128 bytes of A-Z, a-z, 0-9 and `.+~:_-` with a digit first"
    )]
    PackageVersion(String),
    /// A repository entry with a field of an artifact: it names no artifact, and an attestation
    /// nobody checks must not look checked.
    #[error("the repository entry sets {0}, a field of an artifact; it names no artifact")]
    ArtifactField(&'static str),
    /// A repository entry whose installer is `hostkeeper-archive`, which installs only an
    /// attested archive: the coordinator orders and indexes its entries as archives.
    #[error(
        "the repository entry has the installer hostkeeper-archive, which installs archives only"
    )]
    ArchiveFromRepository,
    /// A repository entry whose manifest has no `package`.
    #[error("the manifest of a repository entry needs package")]
    NoPackage,
    /// A repository entry whose `manifest.package` breaks its grammar.
    #[error(
        "the package {0:?} is not 1 to 128 bytes of A-Z, a-z, 0-9 and `+._@-` with a letter or digit first"
    )]
    Package(String),
}

/// Whether `entry` is a repository entry: its `repository` is set, whatever it holds and
/// whatever its installer. Every other entry is an artifact entry.
pub fn is_repository_entry(entry: &Target) -> bool {
    entry.repository.is_some()
}

/// Whether `entry` is a native entry: a repository entry, whatever its installer (every backend
/// of the repository installer installs the platform's own package, so a later one needs no
/// change here), or an entry whose installer is one of [`Installer::is_native`]'s, by its exact
/// spelling. An artifact entry of an installer this crate does not know is not native.
pub fn is_native_entry(entry: &Target) -> bool {
    is_repository_entry(entry) || installer_of(entry).is_some_and(Installer::is_native)
}

/// The installer `entry` names, when this crate knows it.
fn installer_of(entry: &Target) -> Option<Installer> {
    entry.installer.parse().ok()
}

/// Checks the targets of a set for `service`: at least one platform, each key a platform, each
/// list 1 to 8 entries with no installer twice, every entry by [`check_entry`], and for the
/// service `hostkeeper` each list by [`is_self_list`].
pub fn check<'a>(
    service: &ServiceName,
    targets: impl IntoIterator<Item = (&'a String, &'a TargetList)>,
) -> Result<(), TargetError> {
    // Sorted by spelling before any is judged, so a map that hashes its keys gives the same
    // answer on every run.
    let keyed: BTreeMap<&String, &TargetList> = targets.into_iter().collect();
    let mut lists = BTreeMap::new();
    for (key, list) in keyed {
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
        check_entry(service, platform, entry).map_err(|why| TargetError::Entry {
            platform,
            at,
            why,
        })?;
        if entries[..at].iter().any(|e| e.installer == entry.installer) {
            return Err(TargetError::DuplicateInstaller {
                platform,
                installer: entry.installer.clone(),
            });
        }
    }
    if (service.as_str() == SELF_SERVICE) & !is_self_list(entries) {
        return Err(TargetError::SelfList { platform });
    }
    Ok(())
}

/// Whether `entries`, each passing [`check_entry`], are a list the service `hostkeeper` may be
/// set with (packages-native.md section 10.4): one native entry, alone or with one
/// `hostkeeper-archive` entry after it (the move), or one `hostkeeper-archive` entry alone (until
/// hostkeeper's own release lists its native entry). An archive entry is never a repository entry
/// ([`EntryError::ArchiveFromRepository`]).
fn is_self_list(entries: &[Target]) -> bool {
    let archive = |entry: &Target| entry.installer == Installer::HostkeeperArchive.as_str();
    match entries {
        [one] => is_native_entry(one) | archive(one),
        [native, after] => is_native_entry(native) & archive(after),
        _ => false,
    }
}

/// Checks one entry: at most 24 KiB encoded and its installer's name; then a repository entry
/// by [`check_repository_entry`], and an artifact entry by no `package_version`, the artifact's
/// digest, size and 1 to 8 locations, an attestation of 1 byte to 1 MiB with its digest and 1
/// to 8 locations (none on a `macos-pkg` entry), every location's store and locator, at most 2
/// KiB of locators in all, and its manifest by [`manifest::check`] for the platform of its list
/// and by [`manifest::check_hook_of`] for its installer.
pub fn check_entry(
    service: &ServiceName,
    platform: Platform,
    entry: &Target,
) -> Result<(), EntryError> {
    let bytes = entry.encoded_len();
    if bytes > MAX_ENTRY_BYTES {
        return Err(EntryError::TooLarge(bytes));
    }
    if !is_installer_name(&entry.installer) {
        return Err(EntryError::Installer(entry.installer.clone()));
    }
    if is_repository_entry(entry) {
        return check_repository_entry(service, platform, entry);
    }
    if entry.package_version.is_some() {
        return Err(EntryError::VersionWithoutRepository);
    }
    digest("artifact", &entry.digest)?;
    if entry.bytes == 0 {
        return Err(EntryError::ZeroBytes);
    }
    locations("artifact", &entry.locations)?;
    let attested: &[Location] = if installer_of(entry) == Some(Installer::MacosPkg) {
        if entry.attestation.is_some() {
            return Err(EntryError::MacosPkgAttestation);
        }
        &[]
    } else {
        let attestation = entry
            .attestation
            .as_ref()
            .ok_or(EntryError::NoAttestation)?;
        check_attestation(attestation)?;
        &attestation.locations
    };
    let total: usize = entry
        .locations
        .iter()
        .chain(attested)
        .map(|location| location.locator.len())
        .sum();
    if total > MAX_LOCATOR_BYTES_PER_ENTRY {
        return Err(EntryError::LocatorBytes(total));
    }
    check_manifest(service, platform, entry)
}

/// Checks the manifest of `entry`, present, by [`manifest::check`] and then by
/// [`manifest::check_hook_of`] for the entry's installer.
fn check_manifest(
    service: &ServiceName,
    platform: Platform,
    entry: &Target,
) -> Result<(), EntryError> {
    let manifest = entry.manifest.as_ref().ok_or(EntryError::NoManifest)?;
    manifest::check(service, platform, manifest).map_err(EntryError::Manifest)?;
    manifest::check_hook_of(is_native_entry(entry), manifest).map_err(EntryError::Manifest)
}

/// Checks a repository entry (packages-repositories.md section 2.2), past its size and its
/// installer's name, in this order: its `repository`; its `package_version`, present and of its
/// grammar; no location, `bytes` 0 and no attestation; an installer other than
/// `hostkeeper-archive`; the pin, the package file's SHA-256; a manifest whose `package` is
/// present and of its grammar; and that manifest by [`manifest::check`] and
/// [`manifest::check_hook_of`].
fn check_repository_entry(
    service: &ServiceName,
    platform: Platform,
    entry: &Target,
) -> Result<(), EntryError> {
    let repository = entry.repository.as_deref().unwrap_or_default();
    if !is_repository_name(repository) {
        return Err(EntryError::Repository(repository.to_owned()));
    }
    let version = entry
        .package_version
        .as_deref()
        .ok_or(EntryError::NoPackageVersion)?;
    if !is_package_version(version) {
        return Err(EntryError::PackageVersion(version.to_owned()));
    }
    let artifact = [
        ("locations", !entry.locations.is_empty()),
        ("bytes", entry.bytes != 0),
        ("attestation", entry.attestation.is_some()),
    ];
    if let Some((field, _)) = artifact.into_iter().find(|(_, has)| *has) {
        return Err(EntryError::ArtifactField(field));
    }
    if entry.installer == Installer::HostkeeperArchive.as_str() {
        return Err(EntryError::ArchiveFromRepository);
    }
    digest("package", &entry.digest)?;
    let manifest = entry.manifest.as_ref().ok_or(EntryError::NoManifest)?;
    let package = manifest.package.as_deref().ok_or(EntryError::NoPackage)?;
    if !is_package_name(package) {
        return Err(EntryError::Package(package.to_owned()));
    }
    check_manifest(service, platform, entry)
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
