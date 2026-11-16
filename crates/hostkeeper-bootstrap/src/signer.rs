//! The signer check of the pkg (docs/design/mac-bootstrap.md, section 4, decision Q8): the output
//! of `pkgutil --check-signature` must say the pkg is signed by a Developer ID certificate that
//! Apple issued, and the leaf certificate must be a Developer ID Installer identity of the Team
//! ID the settings name.
//!
//! `pkgutil` prints the chain leaf first, its name ending in the Team ID in parentheses:
//!
//! ```text
//! Package "hostkeeper-1.0.0-macos-arm64.pkg":
//!    Status: signed by a developer certificate issued by Apple for distribution
//!    Certificate Chain:
//!     1. Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)
//!        SHA256 Fingerprint:
//!            3D 2A ... (16 bytes)
//! ```
//!
//! Only the status "signed by a developer certificate issued by Apple" is accepted. The statuses
//! the host's macos-pkg installer also accepts (a certificate "trusted by" a root a profile
//! installed, Apple's own software) are not: a root of the farm's own could issue a certificate
//! whose name carries any Team ID, and the Team ID is read from that name.

use hostkeeper_install::macos_pkg::signature::{leaf_fingerprint, status};
use hostkeeper_install::macos_pkg::signers::Fingerprint;

/// The status prefix of a signature by a Developer ID certificate that Apple issued.
pub const APPLE_STATUS: &str = "signed by a developer certificate issued by Apple";
/// The start of the leaf's name: the installer identity, not an application identity.
pub const INSTALLER_IDENTITY: &str = "Developer ID Installer:";

/// What the check found wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignerError {
    #[error("pkgutil --check-signature exited {0:?}")]
    Exit(Option<i32>),
    #[error("pkgutil prints no status line")]
    NoStatus,
    #[error("the signature is not a Developer ID signature that Apple issued: {0}")]
    NotApple(String),
    #[error("pkgutil prints no certificate chain")]
    NoLeaf,
    #[error("the leaf certificate {0:?} is not a Developer ID Installer identity")]
    NotInstaller(String),
    #[error("the leaf certificate {0:?} names no Team ID")]
    NoTeamId(String),
    #[error("the pkg is signed by Team ID {found}, not {wanted}")]
    WrongTeam { found: String, wanted: String },
    #[error("pkgutil prints no SHA-256 fingerprint of the leaf certificate")]
    NoFingerprint,
}

/// The name of the chain's first certificate.
fn leaf_name(stdout: &str) -> Option<&str> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "Certificate Chain:");
    lines.next()?;
    let first = lines.find(|line| !line.is_empty())?;
    let (number, name) = first.split_once(". ")?;
    number
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then_some(name.trim())
}

/// The Team ID at the end of a certificate name: the text of its last parentheses.
fn team_of(name: &str) -> Option<&str> {
    let inner = name.strip_suffix(')')?;
    let (_, team) = inner.rsplit_once('(')?;
    Some(team)
}

/// Judges `pkgutil --check-signature`'s exit code and output against `team_id`; the leaf's
/// fingerprint when the signer is the one pinned.
pub fn check(code: Option<i32>, stdout: &str, team_id: &str) -> Result<Fingerprint, SignerError> {
    if code != Some(0) {
        return Err(SignerError::Exit(code));
    }
    let status = status(stdout).ok_or(SignerError::NoStatus)?;
    if !status.starts_with(APPLE_STATUS) {
        return Err(SignerError::NotApple(status.to_owned()));
    }
    let name = leaf_name(stdout).ok_or(SignerError::NoLeaf)?;
    if !name.starts_with(INSTALLER_IDENTITY) {
        return Err(SignerError::NotInstaller(name.to_owned()));
    }
    let found = team_of(name).ok_or_else(|| SignerError::NoTeamId(name.to_owned()))?;
    if found != team_id {
        return Err(SignerError::WrongTeam {
            found: found.to_owned(),
            wanted: team_id.to_owned(),
        });
    }
    leaf_fingerprint(stdout).ok_or(SignerError::NoFingerprint)
}
