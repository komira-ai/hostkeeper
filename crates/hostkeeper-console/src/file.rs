//! Reading one of the console's plain files: a regular file of at most a cap, as UTF-8.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Why a console file could not be read. The text names the file's flag, never its path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// The file could not be opened or read.
    #[error("{flag}: {kind}")]
    Io {
        /// The flag that names the file.
        flag: &'static str,
        /// What opening or reading it said.
        kind: io::ErrorKind,
    },
    /// The path is not a regular file.
    #[error("{flag}: it is not a regular file")]
    NotAFile {
        /// The flag that names the file.
        flag: &'static str,
    },
    /// The file is over its cap.
    #[error("{flag}: it is over {limit} bytes")]
    TooLong {
        /// The flag that names the file.
        flag: &'static str,
        /// The most bytes it may hold.
        limit: u64,
    },
    /// The file is not UTF-8.
    #[error("{flag}: it is not UTF-8")]
    NotText {
        /// The flag that names the file.
        flag: &'static str,
    },
}

/// The text of the regular file at `path`, of at most `limit` bytes, named `flag` in errors.
pub(crate) fn read_text(path: &Path, flag: &'static str, limit: u64) -> Result<String, ReadError> {
    let io = |e: io::Error| ReadError::Io {
        flag,
        kind: e.kind(),
    };
    let file = File::open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(ReadError::NotAFile { flag });
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io)?;
    checked(bytes, flag, limit)
}

/// `bytes` as text, when they are at most `limit` bytes of UTF-8.
pub(crate) fn checked(bytes: Vec<u8>, flag: &'static str, limit: u64) -> Result<String, ReadError> {
    if bytes.len() as u64 > limit {
        return Err(ReadError::TooLong { flag, limit });
    }
    String::from_utf8(bytes).map_err(|_| ReadError::NotText { flag })
}
