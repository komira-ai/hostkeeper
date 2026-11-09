//! The signers file, `/etc/hostkeeper/macos-pkg.toml` (packages-repositories.md section 4.7):
//! the SHA-256 fingerprints of the leaf certificates a service's `.pkg` may be signed by.
//!
//! ```toml
//! # root:wheel 0644
//! signers = ["<64 hex>"]   # 1 to 4: two during a rotation's overlap
//! ```
//!
//! The daemon reads it once at start and refuses to start on a file that is not its owner's
//! (root's), one a group or others can write, one that is a link or no regular file, an unknown
//! key, no signer, more than four, or a signer that is not 64 hex digits. A missing file is no
//! error: that Mac does not announce `macos-pkg`.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::Deserialize;

/// Where the daemon reads the signers.
pub const SIGNERS_FILE: &str = "/etc/hostkeeper/macos-pkg.toml";
/// The most signers a file names.
pub const MAX_SIGNERS: usize = 4;
/// The largest file read.
pub const MAX_SIGNERS_BYTES: u64 = 16 * 1024;

/// A leaf certificate's SHA-256 fingerprint: 64 lower-case hex digits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// `text` as a fingerprint, when it is 64 hex digits of either case.
    pub fn parse(text: &str) -> Option<Fingerprint> {
        (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Fingerprint(text.to_ascii_lowercase()))
    }

    /// The 64 lower-case hex digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The host's signers: 1 to [`MAX_SIGNERS`] fingerprints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signers(Vec<Fingerprint>);

impl Signers {
    /// `fingerprints` as the host's signers, when there are 1 to [`MAX_SIGNERS`].
    pub fn new(fingerprints: Vec<Fingerprint>) -> Result<Signers, SignersError> {
        match fingerprints.len() {
            0 => Err(SignersError::NoSigner),
            n if n > MAX_SIGNERS => Err(SignersError::TooMany(n)),
            _ => Ok(Signers(fingerprints)),
        }
    }

    /// Whether `leaf` is one of them.
    pub fn names(&self, leaf: &Fingerprint) -> bool {
        self.0.contains(leaf)
    }

    /// The fingerprints, in the file's order.
    pub fn fingerprints(&self) -> &[Fingerprint] {
        &self.0
    }
}

/// Why the signers file was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignersError {
    #[error("is a link")]
    Link,
    #[error("is not a regular file")]
    NotRegular,
    #[error("is owned by uid {found}, not {expected}")]
    WrongOwner { found: u32, expected: u32 },
    #[error("has mode {found:o}, which a group or others can write")]
    WritableByOthers { found: u32 },
    #[error("is larger than {MAX_SIGNERS_BYTES} bytes")]
    TooLarge,
    #[error("cannot be read: {0}")]
    Unreadable(String),
    #[error("is not TOML of the signers file: {0}")]
    Syntax(String),
    #[error("names no signer")]
    NoSigner,
    #[error("names {0} signers, more than {MAX_SIGNERS}")]
    TooMany(usize),
    #[error("names {0:?}, which is not 64 hex digits")]
    BadSigner(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileText {
    signers: Vec<String>,
}

/// Reads the signers file at `path`, which `owner_uid` (0 on a host) must own. `Ok(None)`: no
/// file there.
pub fn load(path: &Path, owner_uid: u32) -> Result<Option<Signers>, SignersError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(SignersError::Link),
        Err(e) => return Err(SignersError::Unreadable(e.to_string())),
    };
    let meta = file
        .metadata()
        .map_err(|e| SignersError::Unreadable(e.to_string()))?;
    if !meta.is_file() {
        return Err(SignersError::NotRegular);
    }
    if meta.uid() != owner_uid {
        return Err(SignersError::WrongOwner {
            found: meta.uid(),
            expected: owner_uid,
        });
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(SignersError::WritableByOthers { found: mode });
    }
    let mut bytes = Vec::new();
    file.take(MAX_SIGNERS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| SignersError::Unreadable(e.to_string()))?;
    if bytes.len() as u64 > MAX_SIGNERS_BYTES {
        return Err(SignersError::TooLarge);
    }
    parse(&bytes).map(Some)
}

/// The signers of a file's bytes.
pub fn parse(bytes: &[u8]) -> Result<Signers, SignersError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| SignersError::Syntax("not UTF-8 text".into()))?;
    let file: FileText =
        toml::from_str(text).map_err(|e| SignersError::Syntax(e.message().to_owned()))?;
    let fingerprints = file
        .signers
        .iter()
        .map(|text| Fingerprint::parse(text).ok_or_else(|| SignersError::BadSigner(text.clone())))
        .collect::<Result<Vec<_>, _>>()?;
    Signers::new(fingerprints)
}

#[cfg(test)]
#[path = "signers_tests.rs"]
mod tests;
