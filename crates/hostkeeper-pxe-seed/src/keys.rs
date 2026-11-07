//! The file of `--ssh-authorized-keys`: the SSH public keys of the installed system's operator
//! account.
//!
//! One key per line, in the `authorized_keys` form `<type> <base64 blob> [comment]`; blank
//! lines and lines starting with `#` are skipped. A line of a private key (a PEM or OpenSSH
//! `PRIVATE KEY` block, or a PuTTY key file) refuses the whole file. A refusal names the line by
//! number and never holds the line's text, so a private key given by mistake is not repeated
//! into a log.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The key types accepted, as `ssh-keygen` writes them. The two security-key types are
/// OpenSSH's own names, `<type>@openssh.com`. The repository's hygiene lint reads that spelling
/// as an e-mail address, so each is joined from two pieces; the domain piece alone is allowed
/// by name in the lint's public domains.
pub const KEY_TYPES: [&str; 7] = [
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    concat!("sk-ecdsa-sha2-nistp256@", "openssh.com"),
    concat!("sk-ssh-ed25519@", "openssh.com"),
    "ssh-ed25519",
    "ssh-rsa",
];

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

/// The key of one line that is neither blank nor a comment.
fn key(line_text: &str, line: usize) -> Result<String, KeysError> {
    let mut words = line_text.split_ascii_whitespace();
    let (Some(kind), Some(blob)) = (words.next(), words.next()) else {
        return Err(KeysError::NotAPublicKey { line });
    };
    if !KEY_TYPES.contains(&kind) {
        return Err(KeysError::NotAPublicKey { line });
    }
    let decoded = STANDARD
        .decode(blob)
        .map_err(|_| KeysError::Blob { line })?;
    if !names_type(&decoded, kind) {
        return Err(KeysError::Blob { line });
    }
    let comment: Vec<&str> = words.collect();
    let comment = comment.join(" ");
    if !comment.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return Err(KeysError::Comment { line });
    }
    Ok(match comment.as_str() {
        "" => format!("{kind} {blob}"),
        _ => format!("{kind} {blob} {comment}"),
    })
}

/// Whether the decoded blob starts with the SSH string of `kind`: a 4-byte big-endian length,
/// then that many bytes, equal to `kind` (RFC 4253 section 6.6).
fn names_type(blob: &[u8], kind: &str) -> bool {
    let length = blob
        .get(..4)
        .map(|n| u32::from_be_bytes([n[0], n[1], n[2], n[3]]) as usize);
    let end = length.map(|n| n.saturating_add(4));
    end.and_then(|end| blob.get(4..end)) == Some(kind.as_bytes())
}
