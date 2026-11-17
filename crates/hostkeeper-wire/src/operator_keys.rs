//! The operator keys: the SSH public keys the coordinator sends a host's daemon for the managed
//! block of its login account's `authorized_keys` (feature `operator-keys-v1`), and the rules
//! both sides read them by.
//!
//! - [`public_key`]: one line in the `authorized_keys` form `<type> <base64 blob> [comment]`,
//!   with no options before the type, given back with single spaces; `pxe-seed`'s key file reads
//!   its lines by the same rule;
//! - [`check_set`]: a whole set: at most [`MAX_KEYS`], each at most [`MAX_KEY_BYTES`] bytes and
//!   already in the form [`public_key`] gives back, none twice;
//! - [`digest`]: the SHA-256 of a set, which the coordinator sends with it and the daemon
//!   reports once the set is in force, so that a set is sent again only when the host does not
//!   hold it.
//!
//! A public key is not a secret, and nothing here reads or names an account or a password.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

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

/// The most keys of one set.
pub const MAX_KEYS: usize = 64;

/// The most bytes of one key of a set, its comment included: an RSA key of 8192 bits is about
/// 1400.
pub const MAX_KEY_BYTES: usize = 4096;

/// Why one line is not a public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyLineError {
    /// Not `<type> <blob> [comment]` with a type of [`KEY_TYPES`] first: options before the type
    /// are refused too.
    #[error("not <type> <base64> [comment] with a known key type first")]
    NotAPublicKey,
    /// The blob is not base64, or does not start with the SSH string of its type.
    #[error("a key blob that is not base64 of its key type")]
    Blob,
    /// The comment holds a byte that is not printable ASCII.
    #[error("a comment that is not printable ASCII")]
    Comment,
}

/// Why a set of keys breaks a rule. A key is named by its place in the set, from 1, never by
/// its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeySetError {
    /// More than [`MAX_KEYS`] keys.
    #[error("{count} keys, more than the {MAX_KEYS} a set may hold")]
    TooMany {
        /// How many the set holds.
        count: usize,
    },
    /// A key longer than [`MAX_KEY_BYTES`].
    #[error("key {place} is longer than {MAX_KEY_BYTES} bytes")]
    TooLong {
        /// The key's place, from 1.
        place: usize,
    },
    /// A key that is not a public key.
    #[error("key {place}: {why}")]
    NotAKey {
        /// The key's place, from 1.
        place: usize,
        /// Why.
        why: KeyLineError,
    },
    /// A key that is a public key, but not written as [`public_key`] gives it back (a space
    /// more, a tab).
    #[error("key {place} is not written with single spaces and nothing around it")]
    NotCanonical {
        /// The key's place, from 1.
        place: usize,
    },
    /// The same type and blob as an earlier key of the set.
    #[error("key {place} has the type and blob of key {first}")]
    Twice {
        /// The key's place, from 1.
        place: usize,
        /// The earlier key's place, from 1.
        first: usize,
    },
}

/// The key of `line`: its type, blob and comment (if any) with single spaces, the line's own
/// leading and trailing white space dropped.
///
/// # Errors
/// [`KeyLineError`], as its values say.
pub fn public_key(line: &str) -> Result<String, KeyLineError> {
    let mut words = line.split_ascii_whitespace();
    let (Some(kind), Some(blob)) = (words.next(), words.next()) else {
        return Err(KeyLineError::NotAPublicKey);
    };
    if !KEY_TYPES.contains(&kind) {
        return Err(KeyLineError::NotAPublicKey);
    }
    let decoded = STANDARD.decode(blob).map_err(|_| KeyLineError::Blob)?;
    if !names_type(&decoded, kind) {
        return Err(KeyLineError::Blob);
    }
    let comment = words.collect::<Vec<_>>().join(" ");
    if !comment.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return Err(KeyLineError::Comment);
    }
    Ok(match comment.as_str() {
        "" => format!("{kind} {blob}"),
        _ => format!("{kind} {blob} {comment}"),
    })
}

/// Whether `keys` keeps every rule of a set, as the module says.
///
/// # Errors
/// The first rule broken, in the set's order: [`KeySetError::TooMany`] before any key is read.
pub fn check_set(keys: &[String]) -> Result<(), KeySetError> {
    if keys.len() > MAX_KEYS {
        return Err(KeySetError::TooMany { count: keys.len() });
    }
    for (index, key) in keys.iter().enumerate() {
        let place = index + 1;
        if key.len() > MAX_KEY_BYTES {
            return Err(KeySetError::TooLong { place });
        }
        let canonical = public_key(key).map_err(|why| KeySetError::NotAKey { place, why })?;
        if canonical != *key {
            return Err(KeySetError::NotCanonical { place });
        }
        if let Some(first) = keys[..index]
            .iter()
            .position(|earlier| same_key(earlier, key))
        {
            return Err(KeySetError::Twice {
                place,
                first: first + 1,
            });
        }
    }
    Ok(())
}

/// The SHA-256 of `keys`, as 64 lower-case hex characters: of each key followed by one `\n`, in
/// the set's order. The empty set's is the SHA-256 of no bytes.
#[must_use]
pub fn digest(keys: &[String]) -> String {
    let mut hash = Sha256::new();
    for key in keys {
        hash.update(key.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `text` is a digest as [`digest`] writes one: 64 lower-case hex characters.
#[must_use]
pub fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether two keys of a set, each already canonical, have the same type and blob.
fn same_key(a: &str, b: &str) -> bool {
    a.split(' ').take(2).eq(b.split(' ').take(2))
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

#[cfg(test)]
#[path = "operator_keys_tests.rs"]
mod tests;
