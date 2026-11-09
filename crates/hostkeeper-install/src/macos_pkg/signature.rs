//! The signature check of `stage` (packages-repositories.md section 4.7, step 2): `pkgutil
//! --check-signature` of the verified file must exit 0 and report a signature whose chain the
//! Mac trusts, and the leaf's SHA-256 fingerprint must be one of the host's signers.
//!
//! `pkgutil` prints the chain leaf first:
//!
//! ```text
//! Package "tierstore-1.6.0.pkg":
//!    Status: signed by a certificate trusted by macOS
//!    Certificate Chain:
//!     1. Tierstore Installer
//!        Expires: ...
//!        SHA256 Fingerprint:
//!            3D 2A ... (16 bytes)
//!            ... (16 bytes)
//!        ------------------------------------------------------------------------
//!     2. Tierstore Root
//!        ...
//! ```
//!
//! The leaf's fingerprint is the lines after the first `SHA256 Fingerprint:`, up to the line
//! that is not hex, joined with all white space dropped, in lower case.

use super::signers::{Fingerprint, Signers};

/// The status prefixes of a signature the Mac trusts: a Developer ID Installer identity, one
/// whose root a trust setting (the MDM's profile, or the test's) admits, and Apple's own.
pub const TRUSTED_STATUS: [&str; 3] = [
    "signed by a developer certificate issued by Apple",
    "signed by a certificate trusted by",
    "signed Apple Software",
];

/// What the check found wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("pkgutil --check-signature exited {0:?}: {1}")]
    Exit(Option<i32>, String),
    #[error("pkgutil prints no status line")]
    NoStatus,
    #[error("the signature is not one the Mac trusts: {0}")]
    Untrusted(String),
    #[error("pkgutil prints no SHA-256 fingerprint of the leaf certificate")]
    NoFingerprint,
    #[error("the leaf certificate {0} is none of the host's signers")]
    NotASigner(String),
}

/// The check of `pkgutil --check-signature`'s exit code and output against `signers`; the
/// leaf's fingerprint when it passes.
pub fn check(
    code: Option<i32>,
    stdout: &str,
    signers: &Signers,
) -> Result<Fingerprint, SignatureError> {
    if code != Some(0) {
        return Err(SignatureError::Exit(code, last_text(stdout)));
    }
    let status = status(stdout).ok_or(SignatureError::NoStatus)?;
    if !TRUSTED_STATUS.iter().any(|ok| status.starts_with(ok)) {
        return Err(SignatureError::Untrusted(status.to_owned()));
    }
    let leaf = leaf_fingerprint(stdout).ok_or(SignatureError::NoFingerprint)?;
    match signers.names(&leaf) {
        true => Ok(leaf),
        false => Err(SignatureError::NotASigner(leaf.as_str().to_owned())),
    }
}

/// The text after `Status:` on its line.
pub fn status(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Status:"))
        .map(str::trim)
}

/// The fingerprint of the chain's first certificate.
pub fn leaf_fingerprint(stdout: &str) -> Option<Fingerprint> {
    let mut lines = stdout
        .lines()
        .skip_while(|line| line.trim() != "SHA256 Fingerprint:");
    lines.next()?;
    let hex: String = lines
        .map(str::trim)
        .take_while(|line| !line.is_empty() && line.split_whitespace().all(is_hex_pair))
        .flat_map(|line| line.split_whitespace())
        .collect();
    Fingerprint::parse(&hex)
}

fn is_hex_pair(word: &str) -> bool {
    word.len() == 2 && word.bytes().all(|b| b.is_ascii_hexdigit())
}

fn last_text(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
#[path = "signature_tests.rs"]
mod tests;
