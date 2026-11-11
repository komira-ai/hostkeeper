//! The mirror of a private repository (design: docs/design/hostgroups.md, 6.6): the host reads
//! such a repository from a local copy, `file:<mirror root>/<name>`, that the daemon fills from
//! the coordinator. This crate knows the copy only through the [`Mirror`] port; `stage` calls it
//! before the package manager reads the index ([`Mirror::index`]) and before it downloads the
//! package ([`Mirror::pool`]). The repository they are given is the entry apt reads, whose URI
//! ends in the registry name of the private repository ([`mirrored_name`]); the entry's own name
//! is its channel, `<registry name>-<suite>`.

use hostkeeper_release::registry;

use super::config::Repository;
use super::outcome::{Failure, Reason};

/// Why the mirror could not be filled.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MirrorError {
    /// The repository has not published what the host needs (the group's suite).
    #[error("{0}")]
    NotPublished(String),
    /// The coordinator, or the origin behind it, could not be read, or what it gave is not
    /// what its index says.
    #[error("{0}")]
    Unreachable(String),
    /// The package file is not the one the order pins.
    #[error("{0}")]
    Digest(String),
    /// The mirror's files could not be written.
    #[error("{0}")]
    Io(String),
}

impl MirrorError {
    /// The failure of the step that met it.
    pub(super) fn failure(&self) -> Failure {
        let reason = match self {
            MirrorError::NotPublished(_) => Reason::NotInRepository,
            MirrorError::Unreachable(_) => Reason::FetchFailed,
            MirrorError::Digest(_) => Reason::StoreDigestMismatch,
            MirrorError::Io(_) => Reason::IoError,
        };
        Failure::new(reason, format!("the mirror: {self}"))
    }
}

/// The local copy of the private repositories.
pub trait Mirror: Send + Sync {
    /// Makes the copy of `repository` hold the index files of its suite for `architecture`
    /// that its `InRelease` lists, replacing what it held.
    ///
    /// # Errors
    /// [`MirrorError`]: why the copy could not be made.
    fn index(&self, repository: &Repository, architecture: &str) -> Result<(), MirrorError>;

    /// Makes the copy hold the package file `filename` (the index's `Filename`), whose size
    /// and SHA-256 are `size` and `sha256`.
    ///
    /// # Errors
    /// [`MirrorError`]: why the file could not be put in the copy.
    fn pool(
        &self,
        repository: &Repository,
        filename: &str,
        sha256: &str,
        size: u64,
    ) -> Result<(), MirrorError>;
}

/// The registry name of the private repository `repository` is read from, when its URI is a
/// directory of the mirror under `root` ([`registry::mirror_uri`]): the one path component after
/// the root. The entry's own name is its channel, `<registry name>-<suite>`.
pub fn mirrored_name<'a>(repository: &'a Repository, root: &std::path::Path) -> Option<&'a str> {
    let prefix = registry::mirror_uri(&root.to_string_lossy(), "");
    repository
        .uri
        .strip_prefix(&prefix)
        .filter(|name| !name.is_empty() && !name.contains('/'))
}
