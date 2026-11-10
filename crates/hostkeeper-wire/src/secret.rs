//! The secret of the operator API: the bearer token, read from a file that passes the server's
//! own file rule.
//!
//! `read_token` reads the file and checks it each time it is called; when to call it is the
//! caller's choice (the coordinator reads the operator token once, at start, so a rotated
//! token needs a restart). `open_token` opens and checks the file now and reads it later, from
//! the descriptor it keeps ([`OpenToken::read`]): `pxe serve` opens the root-only
//! token file as root and reads it only once it has dropped to an unprivileged user. The token
//! never appears in an error text or a `Debug`. An error names the flag of the file, never its
//! path or its content.

use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The shortest and longest token, as the server's `--api-token-file` takes it.
const TOKEN_MIN: usize = 32;
const TOKEN_MAX: u64 = 4096;

/// The flag that names the token file, which error texts name instead of its path.
pub const TOKEN_FLAG: &str = "--token-file";

/// Why a secret file is refused. The text names the flag of the file and the rule it breaks,
/// never its path or its content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The file could not be opened or read.
    #[error("{flag}: {kind}")]
    Io {
        /// The flag that names the file.
        flag: String,
        /// What the system answered.
        kind: std::io::ErrorKind,
    },
    /// The file is not a regular file owned by this process's user with mode `0600` or `0400`.
    #[error("{flag}: {why}")]
    Rule {
        /// The flag that names the file.
        flag: String,
        /// The part of the rule it breaks.
        why: &'static str,
    },
    /// The file does not hold one token of 32 to 4096 visible ASCII characters.
    #[error("{flag}: it must hold one token of 32 to 4096 visible ASCII characters")]
    Token {
        /// The flag that names the file.
        flag: String,
    },
}

/// Read the token in the file at `path`: a regular file (not followed if it is a symlink,
/// not opened if it is a FIFO) owned by this process's user with mode exactly `0600` or
/// `0400`, one token of visible ASCII, 32 bytes or more, surrounding whitespace dropped. The
/// file is opened and checked on every call; nothing is cached, so a caller that wants a
/// rotated token calls again. The error text names the flag and the rule, never the path or
/// the content.
///
/// # Errors
/// [`SecretError`]: the file cannot be read, breaks the file rule, or holds no such token.
pub fn read_token(path: &Path) -> Result<String, SecretError> {
    open_token(path)?.read()
}

/// The token file at `path`, opened and checked against the file rule of [`read_token`] now,
/// against this process's user now, and read later by [`OpenToken::read`] from the descriptor
/// it keeps, whatever user the process is by then.
///
/// # Errors
/// [`SecretError::Io`]: the file cannot be opened; [`SecretError::Rule`]: it breaks the file
/// rule.
pub fn open_token(path: &Path) -> Result<OpenToken, SecretError> {
    open_secret(path, TOKEN_FLAG).map(|file| OpenToken { file })
}

/// A token file [`open_token`] opened and checked, not read yet. Its `Debug` shows nothing of
/// the file.
pub struct OpenToken {
    file: std::fs::File,
}

impl std::fmt::Debug for OpenToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenToken")
    }
}

impl OpenToken {
    /// The token the file holds, as [`read_token`] takes it.
    ///
    /// # Errors
    /// [`SecretError::Io`]: the file cannot be read; [`SecretError::Token`]: it holds no token
    /// of 32 to 4096 visible ASCII characters.
    pub fn read(self) -> Result<String, SecretError> {
        let bytes = read_limited(self.file, TOKEN_FLAG, TOKEN_MAX)?;
        let fail = || SecretError::Token {
            flag: TOKEN_FLAG.to_owned(),
        };
        let text = String::from_utf8(bytes).map_err(|_| fail())?;
        let token = text.trim();
        let visible = token.bytes().all(|b| b.is_ascii_graphic());
        if text.len() as u64 > TOKEN_MAX || token.len() < TOKEN_MIN || !visible {
            return Err(fail());
        }
        Ok(token.to_owned())
    }
}

/// The bytes of a secret file that passes the file rule, at most `limit` of them (and one
/// more, so that a longer file is seen to be too long). A `limit` of `u64::MAX` means no limit
/// beyond what the file holds. The error text names `flag`.
///
/// # Errors
/// [`SecretError::Io`]: the file cannot be opened or read; [`SecretError::Rule`]: it breaks
/// the file rule.
pub fn read_secret(path: &Path, flag: &str, limit: u64) -> Result<Vec<u8>, SecretError> {
    read_limited(open_secret(path, flag)?, flag, limit)
}

/// The secret file at `path`, open, once it passes the file rule; the error names `flag`.
fn open_secret(path: &Path, flag: &str) -> Result<std::fs::File, SecretError> {
    let (file, meta) = open_checked(path).map_err(|e| io_error(flag, &e))?;
    file_rule(meta.is_file(), meta.uid(), meta.mode(), euid()).map_err(|why| {
        SecretError::Rule {
            flag: flag.to_owned(),
            why,
        }
    })?;
    Ok(file)
}

/// At most `limit` bytes of `file`, and one more; the error names `flag`.
fn read_limited(file: std::fs::File, flag: &str, limit: u64) -> Result<Vec<u8>, SecretError> {
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(flag, &e))?;
    Ok(bytes)
}

/// [`SecretError::Io`] of `error` for the file of `flag`.
fn io_error(flag: &str, error: &std::io::Error) -> SecretError {
    SecretError::Io {
        flag: flag.to_owned(),
        kind: error.kind(),
    }
}

/// The effective user of this process.
pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Open without following a symlink and without blocking on a FIFO; the metadata is of the
/// open file, so what is checked is what is read.
fn open_checked(path: &Path) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    Ok((file, meta))
}

/// The rule for the file itself: regular, owned by `me`, mode exactly `0600` or `0400`.
pub(crate) fn file_rule(is_file: bool, uid: u32, mode: u32, me: u32) -> Result<(), &'static str> {
    if !is_file {
        return Err("it is not a regular file");
    }
    if uid != me {
        return Err("it is not owned by the updater's user");
    }
    if !matches!(mode & 0o7777, 0o600 | 0o400) {
        return Err("its mode must be 0600 or 0400");
    }
    Ok(())
}
