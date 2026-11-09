//! The one-time nonce the MDM gives a Mac, read from the managed preferences of the domain
//! `dev.hostkeeper.enrollment` (docs/design/hostkeeper-mdm.md, section 5.3).
//!
//! The MDM's policy gate installs a profile whose one payload sets two keys of that domain:
//! `nonce`, the 32 bytes as data, and `request_id`, the request's key hash as 64 lower-case hex
//! digits. macOS writes the effective preferences of the domain to [`MANAGED_ENROLLMENT`], a
//! property list, binary or XML, whose top-level dictionary holds the keys, as the daemon's start
//! reads the domain `dev.hostkeeper.config`. Keys other than the two are ignored.
//!
//! [`ManagedPreferences`] is the port the enroller reads the file through, in each step that
//! enrolls; [`ManagedFile`] is its implementation over the file, which it accepts only when the
//! trusted account (root) owns it and no one else can write it
//! ([`hostkeeper_config::trust::read_trusted`]). [`read_nonce`] turns the bytes into a
//! [`ManagedNonce`]. A file that is missing, untrusted or malformed is never an error of the
//! step: the request stays pending for an operator.

use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use hostkeeper_config::trust::{TrustError, read_trusted};
use plist::Value;
use sha2::{Digest, Sha256};

use crate::wire::RequestId;

/// Where macOS writes the managed preferences of `dev.hostkeeper.enrollment`. Its path, owner
/// and mode are to be checked on a device (docs/design/hostkeeper-mdm.md, task M16).
pub const MANAGED_ENROLLMENT: &str = "/Library/Managed Preferences/dev.hostkeeper.enrollment.plist";

/// The length of a Mac nonce, in bytes.
pub const MAC_NONCE_BYTES: usize = 32;

/// A Mac nonce: [`MAC_NONCE_BYTES`] bytes, one hour and one use. Its `Debug` shows no byte of
/// it, so no log line or error carries it.
#[derive(Clone, PartialEq, Eq)]
pub struct MacNonce(Vec<u8>);

impl MacNonce {
    /// `bytes` as a nonce, or `None` when they are not [`MAC_NONCE_BYTES`] long.
    pub fn new(bytes: Vec<u8>) -> Option<MacNonce> {
        (bytes.len() == MAC_NONCE_BYTES).then_some(MacNonce(bytes))
    }

    /// The bytes, for the `Enroll` request.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The SHA-256 of the bytes: what the enroller keeps of a nonce it posted.
    pub(crate) fn digest(&self) -> [u8; 32] {
        Sha256::digest(&self.0).into()
    }
}

impl fmt::Debug for MacNonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MacNonce(..)")
    }
}

/// What the managed preferences say: a nonce, and the request it was issued for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedNonce {
    pub request_id: RequestId,
    pub nonce: MacNonce,
}

/// Why the managed preferences hold no nonce the enroller can use. No variant carries a byte of
/// the file.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManagedError {
    #[error("not a property list")]
    NotPlist,
    #[error("not a dictionary at its top level")]
    NotDictionary,
    #[error("no {0} key")]
    Missing(&'static str),
    #[error("its nonce is not data")]
    NonceNotData,
    #[error("its nonce has {0} bytes, not {MAC_NONCE_BYTES}")]
    NonceLength(usize),
    #[error("its request_id is not 64 lower-case hex digits")]
    RequestId,
}

/// The managed preferences of `dev.hostkeeper.enrollment`, as the bytes of their file.
pub trait ManagedPreferences: Send {
    /// The file's bytes, once it is judged trusted; an error when it is missing, not a regular
    /// file, not the trusted account's, writable by another, or unreadable.
    fn read(&self) -> Result<Vec<u8>, TrustError>;
}

/// [`ManagedPreferences`] over a file that the account `trusted_uid` must own and that only it
/// may write: on a Mac, [`MANAGED_ENROLLMENT`] and root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedFile {
    path: PathBuf,
    trusted_uid: u32,
}

impl ManagedFile {
    pub fn new(path: &Path, trusted_uid: u32) -> ManagedFile {
        ManagedFile {
            path: path.to_path_buf(),
            trusted_uid,
        }
    }

    /// The Mac's own: [`MANAGED_ENROLLMENT`], owned by root.
    pub fn host() -> ManagedFile {
        ManagedFile::new(Path::new(MANAGED_ENROLLMENT), 0)
    }
}

impl ManagedPreferences for ManagedFile {
    fn read(&self) -> Result<Vec<u8>, TrustError> {
        read_trusted(&self.path, self.trusted_uid)
    }
}

/// The nonce and request id of the managed preferences in `bytes`; see the module documentation.
pub fn read_nonce(bytes: &[u8]) -> Result<ManagedNonce, ManagedError> {
    let value = Value::from_reader(Cursor::new(bytes)).map_err(|_| ManagedError::NotPlist)?;
    let dict = value.as_dictionary().ok_or(ManagedError::NotDictionary)?;
    let nonce = match dict.get("nonce").ok_or(ManagedError::Missing("nonce"))? {
        Value::Data(data) => {
            MacNonce::new(data.clone()).ok_or(ManagedError::NonceLength(data.len()))?
        }
        _ => return Err(ManagedError::NonceNotData),
    };
    let request_id = dict
        .get("request_id")
        .ok_or(ManagedError::Missing("request_id"))?
        .as_string()
        .and_then(RequestId::parse)
        .ok_or(ManagedError::RequestId)?;
    Ok(ManagedNonce { request_id, nonce })
}
