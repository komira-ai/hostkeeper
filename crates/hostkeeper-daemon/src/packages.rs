//! The package port: what the daemon asks of whatever fetches the artifact of a `macos-pkg`
//! entry (packages-repositories.md section 4.7).
//!
//! A `macos-pkg` entry carries no attestation: the `.pkg` is fetched by the entry's pin alone
//! ([`Packages::pinned`]) from the host's stores, and the installer's `stage` checks the package's
//! signature. The port answers with the staging file, or with one error of a closed set; the
//! daemon reports the error as its token ([`PackageError::refusal`]). No other installer fetches
//! through it: the package manager of a repository entry downloads for itself.

use std::any::Any;
use std::fmt;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::refusal::Refusal;

/// Why a package could not be had. Each carries a text for the log; the daemon reports the
/// variant as its token ([`PackageError::refusal`]) and writes no step record.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    /// The store has no package of that service and version.
    #[error("the package is not in the store: {0}")]
    NotFound(String),
    /// The package's bytes are not the digest the order names.
    #[error("the package is not the digest the order names: {0}")]
    DigestMismatch(String),
    /// The disk is full; nothing was staged.
    #[error("there is no space to stage the package: {0}")]
    NoSpace(String),
    /// Any other failure to fetch.
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

    /// Whom the error is blamed on: the package for one the store does not have or whose bytes
    /// are not the digest, the host for the rest.
    pub fn class(&self) -> Class {
        match self {
            PackageError::NotFound(_) | PackageError::DigestMismatch(_) => Class::Release,
            PackageError::NoSpace(_) | PackageError::Other(_) => Class::Environment,
            PackageError::Refused(refused) => refused.class,
        }
    }
}

/// The file of an entry's artifact fetched by its pin alone ([`Packages::pinned`]), whose size
/// and SHA-256 are the entry's: what it holds (a staging file the store removes when it is
/// dropped) lives as long as it does.
pub struct Pinned {
    path: PathBuf,
    _held: Box<dyn Any + Send>,
}

impl Pinned {
    /// The file at `path`, kept as long as `held` lives.
    pub fn new(path: &Path, held: Box<dyn Any + Send>) -> Pinned {
        Pinned {
            path: path.to_path_buf(),
            _held: held,
        }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fetches the artifact of an entry by its pin.
pub trait Packages: Send + Sync {
    /// The artifact of `entry` for `service` fetched by its pin alone, with no attestation and
    /// nothing unpacked: the `.pkg` of a `macos-pkg` entry, which carries none (its pin and the
    /// package's signature, checked by the installer's `stage`, are the whole check).
    fn pinned(&self, service: &ServiceName, entry: &Target) -> Result<Pinned, PackageError>;
}
