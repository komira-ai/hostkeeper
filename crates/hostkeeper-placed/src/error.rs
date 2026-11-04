//! Why linking refused or failed. [`PlacedError::class`] and [`PlacedError::reason`] say how a
//! step or force record reports it, in the form `hostkeeper_config::manifest::ManifestRefusal`
//! uses.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Class;
use hostkeeper_config::names::PathError;

use crate::lint::LintError;
use crate::store::SCHEMA;
use crate::unit_control::UnitControlError;

/// What holds a path that a package claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// `placed.json` lists the path for this other service.
    Service(String),
    /// A regular file the record does not list as a link of the claimant.
    File,
    /// A directory.
    Directory,
    /// A link to this target that the record does not list.
    Link(PathBuf),
    /// A socket, a pipe or a device.
    Other,
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Owner::Service(service) => write!(f, "the service {service}"),
            Owner::File => f.write_str("a regular file placed.json does not list"),
            Owner::Directory => f.write_str("a directory placed.json does not list"),
            Owner::Link(target) => write!(
                f,
                "a link to {} that placed.json does not list",
                target.display()
            ),
            Owner::Other => f.write_str("a socket, pipe or device placed.json does not list"),
        }
    }
}

/// Why a package's claim on its paths is refused (design section 4.7, "Paths").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    #[error("the unit file units/{file} does not start with the service name {service}")]
    UnitName { service: String, file: String },
    #[error("a [[files]] path is empty")]
    EmptyPath,
    #[error("the [[files]] path {path:?} {why}")]
    BadPath { path: String, why: PathError },
    #[error(
        "the [[files]] path {path:?} is {prefix} or below it, where a package may not place a file"
    )]
    Refused { path: String, prefix: &'static str },
    #[error("two entries of the package claim the path {}", .path.display())]
    Duplicate { path: PathBuf },
    /// One claimed path lies below another: a directory placed at one would receive the other.
    #[error("the package claims {} and {}, one below the other", .path.display(), .other.display())]
    Overlap { path: PathBuf, other: PathBuf },
    #[error("the [[files]] source {entry:?} {why}")]
    BadSource { entry: String, why: String },
}

/// Why linking refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PlacedError {
    /// The path is owned by another service, or is occupied by something `placed.json` does not
    /// list: `Failed`, class release, `path_conflict`. Nothing was overwritten.
    #[error("{} is held by {owner}", .path.display())]
    PathConflict { path: PathBuf, owner: Owner },
    #[error(transparent)]
    Claim(#[from] ClaimError),
    #[error(transparent)]
    Lint(#[from] LintError),
    /// `placed.json` holds an `uninstalling` tombstone for the service.
    #[error("the service {service} is being uninstalled; its paths are not linked")]
    Tombstoned { service: String },
    /// `placed.json` cannot be read as a record; it is left as it is.
    #[error("{} is not a placed-path record: {why}", .path.display())]
    Corrupt { path: PathBuf, why: String },
    /// `placed.json` was written by a newer daemon; it is left as it is.
    #[error("{} has schema {schema}; this daemon reads schema {SCHEMA} and leaves it as it is", .path.display())]
    NewerSchema { path: PathBuf, schema: u64 },
    #[error("cannot {op} {}: {source}", .path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Another daemon holds the lock of `placed.json`.
    #[error("another daemon holds {}, or its lock cannot be taken: {source}", .path.display())]
    Locked {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Units(#[from] UnitControlError),
    /// The caller passed two different switch intents for one service.
    #[error("two different switch intents name the service {service}")]
    ConflictingIntents { service: String },
    /// [`crate::Store::save`] was given the lock of another record.
    #[error("the lock {} is not the lock of {}", .lock.display(), .record.display())]
    ForeignLock { lock: PathBuf, record: PathBuf },
}

impl PlacedError {
    fn kind(&self) -> (Class, &'static str) {
        match self {
            PlacedError::PathConflict { .. } => (Class::Release, "path_conflict"),
            PlacedError::Claim(_) | PlacedError::Lint(_) => (Class::Release, "manifest_mismatch"),
            PlacedError::Tombstoned { .. } => (Class::Environment, "uninstalling"),
            PlacedError::Corrupt { .. }
            | PlacedError::NewerSchema { .. }
            | PlacedError::Io { .. }
            | PlacedError::Locked { .. }
            | PlacedError::ConflictingIntents { .. }
            | PlacedError::ForeignLock { .. } => (Class::Environment, "link_failed"),
            PlacedError::Units(_) => (Class::Environment, "daemon_reload_failed"),
        }
    }

    /// `Release` for a refusal of the package (`path_conflict`, a claim or lint refusal),
    /// `Environment` for a failure of the host.
    pub fn class(&self) -> Class {
        self.kind().0
    }

    /// The reason token: `path_conflict`, `manifest_mismatch`, `uninstalling`, `link_failed` or
    /// `daemon_reload_failed`.
    pub fn reason(&self) -> &'static str {
        self.kind().1
    }
}

/// The error of the operation `op` on `path`.
pub(crate) fn io_at(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> PlacedError {
    let path = path.to_path_buf();
    move |source| PlacedError::Io { op, path, source }
}
