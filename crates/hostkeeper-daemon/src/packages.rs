//! The package port: what the daemon asks of whatever fetches, verifies and unpacks packages
//! (design section 5.5).
//!
//! The daemon asks for the version directory of the package an order names, under the service's
//! lock and outside the step lock: a fetch has its own deadline and is not bounded by the
//! dead-man. The port answers with the unpacked, sealed directory, or with one error of a closed
//! set. The daemon then reads the directory's `manifest.toml` itself, with the manifest's loader,
//! for the order's service and version, so that a store cannot hand it a package of another name
//! or version.

use std::fmt;
use std::path::Path;

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::Staged;

use crate::refusal::Refusal;
use crate::trusted_root::RootReport;
use crate::verb::PackageDigest;

/// The entry an order carries: the one artifact of the release the coordinator picked for the
/// host, with its locations, attestation and manifest.
pub type Entry = Target;

/// The unpacked version directory of one package, sealed: nothing from it runs unless the tree
/// still has the digest it had when it was sealed ([`Staged::check`]).
#[derive(Clone, Debug, PartialEq)]
pub struct VersionDir {
    staged: Staged,
    manifest: Option<Manifest>,
}

impl VersionDir {
    /// A sealed directory whose manifest the store did not keep.
    pub fn new(staged: Staged) -> VersionDir {
        VersionDir {
            staged,
            manifest: None,
        }
    }

    /// A sealed directory with the manifest the store parsed from it.
    pub fn with_manifest(staged: Staged, manifest: Manifest) -> VersionDir {
        VersionDir {
            staged,
            manifest: Some(manifest),
        }
    }

    /// The manifest the store parsed from the directory, when it kept it.
    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        self.staged.dir()
    }

    /// The seal.
    pub fn staged(&self) -> &Staged {
        &self.staged
    }
}

/// Why a package could not be had. Each carries a text for the log; the daemon reports the
/// variant as its token ([`PackageError::refusal`]) and writes no step record.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    /// The release channel has no package of that service and version.
    #[error("the package is not in the release channel: {0}")]
    NotFound(String),
    /// The package's bytes, or its attestation, are not the digest the order names.
    #[error("the package is not the digest the order names: {0}")]
    DigestMismatch(String),
    /// The disk is full; nothing was staged.
    #[error("there is no space to stage the package: {0}")]
    NoSpace(String),
    /// Any other failure to fetch, verify or unpack.
    #[error("the package could not be had: {0}")]
    Other(String),
    /// A failure with its own token, outcome and class, as the real store reports them.
    #[error("{0}")]
    Refused(PackageRefusal),
}

/// A refusal of the real package store: the token, the outcome and the class of the failure
/// (`Deferred` with `Environment` when nothing changed on the host and the order may be tried
/// again, `Failure` with `Release` when the package is bad) and a text for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageRefusal {
    pub refusal: Refusal,
    pub outcome: Outcome,
    pub class: Class,
    pub detail: String,
}

impl fmt::Display for PackageRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.detail, self.refusal.token())
    }
}

impl PackageError {
    /// The token the daemon reports.
    pub fn refusal(&self) -> Refusal {
        match self {
            PackageError::NotFound(_) => Refusal::PackageNotFound,
            PackageError::DigestMismatch(_) => Refusal::DigestMismatch,
            PackageError::NoSpace(_) => Refusal::NoSpace,
            PackageError::Other(_) => Refusal::PackageFailed,
            PackageError::Refused(refused) => refused.refusal,
        }
    }

    /// `Failure` for a package that is bad, `Deferred` for what the host must fix or wait out.
    pub fn outcome(&self) -> Outcome {
        match self {
            PackageError::NotFound(_) | PackageError::DigestMismatch(_) => Outcome::Failure,
            PackageError::NoSpace(_) | PackageError::Other(_) => Outcome::Deferred,
            PackageError::Refused(refused) => refused.outcome,
        }
    }

    /// Whom the error is blamed on: the package for one the channel does not have or whose
    /// bytes are not the digest, the host for the rest.
    pub fn class(&self) -> Class {
        match self {
            PackageError::NotFound(_) | PackageError::DigestMismatch(_) => Class::Release,
            PackageError::NoSpace(_) | PackageError::Other(_) => Class::Environment,
            PackageError::Refused(refused) => refused.class,
        }
    }
}

/// Fetches, verifies, unpacks and seals packages.
pub trait Packages: Send + Sync {
    /// The sealed version directory of `service` at `version`, whose package has `digest`. When
    /// `index_digest` is given, the release index must have that SHA-256 (checked before it is
    /// read); `None` checks nothing. With `entry`, the entry the order carries (already judged by
    /// [`crate::entry::check`]), the package is fetched from the entry's locations and proven
    /// through its attestation, and the release index is not read; without one, it comes from
    /// the release channel.
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
        entry: Option<&Entry>,
    ) -> Result<VersionDir, PackageError>;

    /// Removes what a crashed fetch or unpack of `service` left in its package directory. The
    /// default removes nothing.
    fn remove_orphans(&self, _service: &ServiceName) -> Result<(), PackageError> {
        Ok(())
    }

    /// The age of the Sigstore trusted root the store checks attestations with, and the reason
    /// it needs attention, read now: what the admin `Status` reports of it. The default has no
    /// root to report.
    fn trusted_root(&self) -> Option<RootReport> {
        None
    }
}
