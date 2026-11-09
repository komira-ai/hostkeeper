//! Why reading or switching the host's links refused or failed. [`PlacedError::class`] and
//! [`PlacedError::reason`] say how a step or force record reports it, in the form
//! `hostkeeper_config::manifest::ManifestRefusal` uses.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Class;

use crate::store::SCHEMA;
use crate::unit_control::UnitControlError;

/// What holds a path where a link of `current` or `previous` was expected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A link to a target that is not a version beside it.
    Link(PathBuf),
    /// A socket, a pipe or a device.
    Other,
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Owner::File => f.write_str("a regular file"),
            Owner::Directory => f.write_str("a directory"),
            Owner::Link(target) => write!(f, "a link to {}", target.display()),
            Owner::Other => f.write_str("a socket, pipe or device"),
        }
    }
}

/// Why reading or switching the host's links refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PlacedError {
    /// `current` or `previous` is not a link to a version beside it: `Failed`, class release,
    /// `path_conflict`. Nothing was overwritten.
    #[error("{} is held by {owner}", .path.display())]
    PathConflict { path: PathBuf, owner: Owner },
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
    /// [`crate::Store::save`] was given the lock of another record.
    #[error("the lock {} is not the lock of {}", .lock.display(), .record.display())]
    ForeignLock { lock: PathBuf, record: PathBuf },
}

impl PlacedError {
    fn kind(&self) -> (Class, &'static str) {
        match self {
            PlacedError::PathConflict { .. } => (Class::Release, "path_conflict"),
            PlacedError::Corrupt { .. }
            | PlacedError::NewerSchema { .. }
            | PlacedError::Io { .. }
            | PlacedError::Locked { .. }
            | PlacedError::ForeignLock { .. } => (Class::Environment, "link_failed"),
            PlacedError::Units(_) => (Class::Environment, "daemon_reload_failed"),
        }
    }

    /// `Release` for `path_conflict`, `Environment` for a failure of the host.
    pub fn class(&self) -> Class {
        self.kind().0
    }

    /// The reason token: `path_conflict`, `link_failed` or `daemon_reload_failed`.
    pub fn reason(&self) -> &'static str {
        self.kind().1
    }
}

/// The error of the operation `op` on `path`.
pub(crate) fn io_at(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> PlacedError {
    let path = path.to_path_buf();
    move |source| PlacedError::Io { op, path, source }
}
