//! The plist port of a Mac's maintenance collectors ([`crate::maintenance_collect`]): the
//! dictionary at the top of a property list, XML or binary, read from a host's file. An Xcode's
//! `Contents/version.plist`, the files of `/Library/Managed Preferences` and the software-update
//! declarations' state are read through it, so each collector's own logic is about keys and
//! values alone.
//!
//! A file that is not there is `None`, never an error: the caller says what its absence means. A
//! file that is there and cannot be read, is larger than [`MAX_PLIST_BYTES`], is not a property
//! list, or is one whose top is not a dictionary, is an error naming the host's path and never
//! holding a byte of the file: such a file is reported unreadable, never read as one with no keys.

use std::io::{self, Cursor, Read};

use plist::{Dictionary, Value};

use crate::maintenance_collect::HostRoot;

/// The largest property list read.
pub const MAX_PLIST_BYTES: u64 = 1024 * 1024;

/// Where a collector reads property lists.
pub trait Plists: Send + Sync {
    /// The dictionary at the top of the host's property list `path`, or `None` when there is no
    /// file there.
    ///
    /// # Errors
    /// Why a file that is there gives no dictionary, naming the host's path.
    fn dictionary(&self, path: &str) -> Result<Option<Dictionary>, String>;
}

/// The property lists of the files under a [`HostRoot`].
pub struct HostPlists {
    root: HostRoot,
}

impl HostPlists {
    /// The property lists of the files under `root`.
    #[must_use]
    pub fn new(root: HostRoot) -> HostPlists {
        HostPlists { root }
    }

    /// The host's own property lists.
    #[must_use]
    pub fn at_host() -> HostPlists {
        HostPlists::new(HostRoot::host())
    }
}

impl Plists for HostPlists {
    fn dictionary(&self, path: &str) -> Result<Option<Dictionary>, String> {
        let file = match std::fs::File::open(self.root.path(path)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{path} could not be read: {error}")),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PLIST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{path} could not be read: {error}"))?;
        if bytes.len() as u64 > MAX_PLIST_BYTES {
            return Err(format!("{path} is larger than {MAX_PLIST_BYTES} bytes"));
        }
        Value::from_reader(Cursor::new(bytes))
            .map_err(|_| format!("{path} is not a property list"))?
            .into_dictionary()
            .map(Some)
            .ok_or_else(|| format!("{path} is not a dictionary"))
    }
}

#[cfg(test)]
#[path = "maintenance_plist_tests.rs"]
mod tests;
