//! The checks of a release (docs/design/packages.md, build item A1): the targets CI sets for a
//! version, a list of artifacts per platform, one per installer, and the manifest each entry
//! carries (`hostkeeper.release.v1`, generated in `hostkeeper-proto`). The coordinator and the
//! daemon run the same code, so a release the coordinator accepts is judged by the daemon by the
//! same rules, and the daemon trusts no check the coordinator made.
//!
//! - [`target`]: what the coordinator checks of a set with targets (section 3.4): the platforms,
//!   each list's entries, digests, sizes, attestation, locations and size, and each manifest by
//!   the rules that need no host and no knowledge of the entry's installer; and of a repository
//!   entry (`Target.repository` set, packages-repositories.md section 2.2), its repository,
//!   package version, pin and package name, and that it names no artifact.
//! - [`manifest`]: the manifest's rules (section 4.4), those every manifest keeps
//!   ([`manifest::check`]) and those the entry's installer adds ([`manifest::check_for`]).
//! - [`receipt`]: what the daemon checks of an entry before it fetches anything: its version
//!   first (`daemon_too_old`), then the installer it lists (`installer_not_listed`), then the
//!   manifest (`manifest_invalid`).
//! - [`staged`]: what the daemon checks of a manifest against the tree its installer staged:
//!   the hook program and every `place` source, reached through no link.
//! - [`registry`]: the package repositories the coordinator keeps and pushes to every host: a
//!   repository's rules, the files a host holds for a set, their digest, and a keyring's fingerprints.
//! - [`installer`] the installers a manifest's rules know and which are repository backends, and
//!   [`grammar`] the digest, name and locator grammars (section 7.1) and a repository entry's.
//!
//! This is a pure crate: no I/O, no process, no clock, no randomness and no hashed collection. A
//! check takes a message and answers the first rule it breaks, in a fixed order, so the same
//! message is always refused with the same error.

pub mod grammar;
pub mod installer;
pub mod manifest;
pub mod receipt;
pub mod registry;
pub mod staged;
pub mod target;

pub use installer::Installer;
pub use manifest::ManifestError;
pub use receipt::{Daemon, ReceiptError};
pub use target::{EntryError, TargetError};

/// The most entries of one platform's list.
pub const MAX_ENTRIES: usize = 8;
/// The most locations of an artifact, and of an attestation.
pub const MAX_LOCATIONS: usize = 8;
/// The most bytes of all the locators of one entry, the artifact's and the attestation's.
pub const MAX_LOCATOR_BYTES_PER_ENTRY: usize = 2048;
/// The most bytes of one entry, encoded.
pub const MAX_ENTRY_BYTES: usize = 24 * 1024;
/// The most bytes of an attestation bundle.
pub const MAX_ATTESTATION_BYTES: u64 = 1024 * 1024;
/// The most bytes of one manifest, encoded.
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;
/// The most `run` entries of a manifest.
pub const MAX_RUN: usize = 64;
/// The most `place` entries of a manifest.
pub const MAX_PLACE: usize = 256;
/// The most params of a manifest.
pub const MAX_PARAMS: usize = 64;
/// The most containers of a manifest.
pub const MAX_CONTAINERS: usize = 32;
/// The longest an installer step may be bounded, in seconds.
pub const MAX_INSTALLER_SECS: u64 = 3600;
/// The lowest host port a container may publish: no privileged port.
pub const MIN_HOST_PORT: u32 = 1024;
