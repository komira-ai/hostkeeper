//! The one BMC credential (`--console-bmc-credential-file`, `docs/design/console-broker.md`
//! decision D5): the user and password every BMC shares, which the coordinator holds and no
//! operator sees.
//!
//! ```toml
//! user = "ADMIN"
//! password = "a long random password"
//! ```
//!
//! The file is a secret file: a regular file of the coordinator's user, mode `0600` or `0400`,
//! not followed through a link ([`hostkeeper_wire::secret::read_secret`]). `user` is 1 to 16
//! bytes and `password` 1 to 20 bytes, the most IPMI 2.0 takes, of printable ASCII (a space is
//! allowed in the password only). No error and no `Debug` holds either of them or any text of the
//! file: a [`BmcCredential`] has no `Display` and no `Serialize`, and its `Debug` is redacted.

use std::fmt;
use std::path::Path;

use hostkeeper_wire::secret::{SecretError, read_secret};
use serde::Deserialize;

use crate::file::{ReadError, checked};

/// The flag that names the file, which error texts name instead of its path.
pub const FLAG: &str = "--console-bmc-credential-file";

/// The longest file read.
pub const MAX_FILE_BYTES: u64 = 4096;

/// The longest user name and password IPMI 2.0 takes.
const MAX_USER_BYTES: usize = 16;
const MAX_PASSWORD_BYTES: usize = 20;

/// Why the file is refused. The text names the flag and the rule, never the path or any of the
/// file's text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// The file breaks the secret file rule, or could not be read.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The file is over its cap or not UTF-8.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The text is not TOML of exactly `user` and `password`, both strings. The TOML parser's
    /// own text is not kept: it quotes the file.
    #[error("--console-bmc-credential-file: it is not TOML of exactly user and password")]
    Parse,
    /// The user is not 1 to 16 bytes of printable ASCII with no space.
    #[error(
        "--console-bmc-credential-file: user must be 1 to 16 bytes of printable ASCII with no space"
    )]
    User,
    /// The password is not 1 to 20 bytes of printable ASCII.
    #[error("--console-bmc-credential-file: password must be 1 to 20 bytes of printable ASCII")]
    Password,
}

/// The BMC user and password. Printed, it shows neither; it is neither cloned nor compared, so
/// the one held is the one copy.
pub struct BmcCredential {
    user: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    user: String,
    password: String,
}

impl BmcCredential {
    /// The credential of the file's text `text`.
    ///
    /// # Errors
    /// [`CredentialError`]: the text is not the file's TOML, or the user or the password breaks
    /// its rule.
    pub fn parse(text: &str) -> Result<Self, CredentialError> {
        let file: RawFile = toml::from_str(text).map_err(|_| CredentialError::Parse)?;
        if !printable(&file.user, MAX_USER_BYTES, false) {
            return Err(CredentialError::User);
        }
        if !printable(&file.password, MAX_PASSWORD_BYTES, true) {
            return Err(CredentialError::Password);
        }
        Ok(Self {
            user: file.user,
            password: file.password,
        })
    }

    /// The credential of the secret file at `path`.
    ///
    /// # Errors
    /// [`CredentialError`]: the file breaks the secret file rule or cannot be read, is over
    /// [`MAX_FILE_BYTES`] or not UTF-8, or its text is refused as [`Self::parse`] refuses it.
    pub fn read(path: &Path) -> Result<Self, CredentialError> {
        let bytes = read_secret(path, FLAG, MAX_FILE_BYTES)?;
        Self::parse(&checked(bytes, FLAG, MAX_FILE_BYTES)?)
    }

    /// The BMC user.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The BMC password, for the environment of the BMC tool's process and nothing else.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for BmcCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BmcCredential { user: <redacted>, password: <redacted> }")
    }
}

/// Whether `text` is 1 to `max` bytes of printable ASCII, a space among them only when `space`.
fn printable(text: &str, max: usize, space: bool) -> bool {
    (1..=max).contains(&text.len())
        && text
            .bytes()
            .all(|c| c.is_ascii_graphic() || (space && c == b' '))
}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
