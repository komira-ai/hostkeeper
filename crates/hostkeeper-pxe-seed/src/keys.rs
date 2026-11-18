//! The file of `--ssh-authorized-keys`: the SSH public keys of the installed system's operator
//! account.
//!
//! One key per line, in the `authorized_keys` form `<type> <base64 blob> [comment]`; blank
//! lines and lines starting with `#` are skipped. A line of a private key (a PEM or OpenSSH
//! `PRIVATE KEY` block, or a PuTTY key file) refuses the whole file. A refusal names the line by
//! number and never holds the line's text, so a private key given by mistake is not repeated
//! into a log.

use hostkeeper_wire::operator_keys::{KeyLineError, public_key};

/// The key types accepted: the rule of the operator keys a coordinator sends
/// (`hostkeeper_wire::operator_keys`), which reads each line of this file too.
pub use hostkeeper_wire::operator_keys::KEY_TYPES;

/// Why the file of SSH public keys was refused. Each line number counts from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeysError {
    #[error("the SSH keys file holds a private key at line {line}; it must hold public keys only")]
    PrivateKey { line: usize },
    #[error(
        "the SSH keys file's line {line} is not <type> <base64> [comment] with a known key type"
    )]
    NotAPublicKey { line: usize },
    #[error("the SSH keys file's line {line} has a key blob that is not base64 of its key type")]
    Blob { line: usize },
    #[error("the SSH keys file's line {line} has a comment that is not printable ASCII")]
    Comment { line: usize },
    #[error("the SSH keys file holds no public key")]
    Empty,
}

/// The public keys of the file, in its order, each as `<type> <blob>` or `<type> <blob>
/// <comment>` with single spaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedKeys(Vec<String>);

impl AuthorizedKeys {
    /// The keys of `text`, refused as [`KeysError`] says.
    pub fn parse(text: &str) -> Result<AuthorizedKeys, KeysError> {
        // A private key anywhere refuses the file before any other line is judged.
        if let Some(line) = text.lines().position(is_private) {
            return Err(KeysError::PrivateKey { line: line + 1 });
        }
        let mut keys = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line_text = line.trim();
            if line_text.is_empty() | line_text.starts_with('#') {
                continue;
            }
            keys.push(key(line_text, index + 1)?);
        }
        if keys.is_empty() {
            return Err(KeysError::Empty);
        }
        Ok(AuthorizedKeys(keys))
    }

    /// The keys, in the file's order.
    pub fn keys(&self) -> &[String] {
        &self.0
    }
}

/// Whether `line` belongs to a private key: the armour line of a PEM or OpenSSH private key,
/// or the header of a PuTTY key file.
fn is_private(line: &str) -> bool {
    line.contains("PRIVATE KEY") | line.starts_with("PuTTY-User-Key-File-")
}

/// The key of one line that is neither blank nor a comment, by the rule of
/// [`public_key`].
fn key(line_text: &str, line: usize) -> Result<String, KeysError> {
    public_key(line_text).map_err(|why| match why {
        KeyLineError::NotAPublicKey => KeysError::NotAPublicKey { line },
        KeyLineError::Blob => KeysError::Blob { line },
        KeyLineError::Comment => KeysError::Comment { line },
    })
}
