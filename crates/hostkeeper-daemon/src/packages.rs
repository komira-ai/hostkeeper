//! The package port: what the daemon asks of whatever fetches, verifies and unpacks packages
//! (design section 5.5).
//!
//! The daemon asks for the version directory of the package an order names, under the service's
//! lock and outside the step lock: a fetch has its own deadline and is not bounded by the
//! dead-man. The port answers with the unpacked, sealed directory, or with one error of a closed
//! set. The daemon then reads the directory's `manifest.toml` itself, with the manifest's loader,
//! for the order's service and version, so that a store cannot hand it a package of another name
//! or version.

use std::path::Path;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::Staged;

use crate::verb::PackageDigest;

/// The unpacked version directory of one package, sealed: nothing from it runs unless the tree
/// still has the digest it had when it was sealed ([`Staged::check`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionDir {
    staged: Staged,
}

impl VersionDir {
    pub fn new(staged: Staged) -> VersionDir {
        VersionDir { staged }
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
/// variant as its token ([`crate::checks::package_refusal`]) and writes no step record.
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
}

/// Fetches, verifies, unpacks and seals packages.
pub trait Packages: Send + Sync {
    /// The sealed version directory of `service` at `version`, whose package has `digest`.
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &PackageDigest,
    ) -> Result<VersionDir, PackageError>;
}
