//! `hostkeeper-coordinator check-ca enrollment` and `check-ca ssh`: whether a copy of a CA's files
//! is the CA the fleet trusts (docs/operations/ca-backup.md). They read, never write, and run
//! without a coordinator, on the copy wherever it was put back.
//!
//! `enrollment` loads the key and the certificate as the coordinator's start does ([`Ca::open`]:
//! the key a PKCS#8 ECDSA P-256 file of this user closed to others, the certificate one of that
//! key) and compares the certificate's SHA-256 with `--ca-sha256`, the `ca_sha256` every daemon
//! config pins.
//!
//! `ssh` reads the key file as the coordinator reads `--ssh-ca-secret-file` (one key, or two one
//! after the other while a rotation runs) and requires every key in it to be a line of
//! `--public-file`: what hosts trust (a host's CA keys file, the file of
//! `--ssh-cp-ca-public-file`, or the line `ssh-ca init` printed).
//!
//! Both print fingerprints and file names only. No byte of a key is printed, in a report or in a
//! refusal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use hostkeeper_enrollment::ca::{Ca, CaError, CaFiles, Signer as _};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::SystemClock;
use hostkeeper_wire::secret::{SecretError, read_secret};

use crate::backup::Report;
use crate::ssh_cert::{CaKey, KeyError, UserKey};

/// The most bytes of an SSH CA key file: two keys, with room (as `--ssh-ca-secret-file`).
const MAX_SECRET_BYTES: u64 = 16 * 1024;

/// The most bytes of a public key file.
const MAX_PUBLIC_BYTES: u64 = 8 * 1024;

/// The verbs of `check-ca`.
#[derive(Clone, Debug, Args)]
pub struct CheckCa {
    /// Which CA's copy to check.
    #[command(subcommand)]
    pub command: CheckCaCommand,
}

/// The verbs of `check-ca`.
#[derive(Clone, Debug, Subcommand)]
pub enum CheckCaCommand {
    /// Check a copy of the enrollment CA: the key is the certificate's, and the certificate is
    /// the one the daemons pin.
    Enrollment(Enrollment),
    /// Check a copy of an SSH user CA key: every key in the file is one the hosts trust.
    Ssh(Ssh),
}

/// The flags of `check-ca enrollment`.
#[derive(Clone, Debug, Args)]
pub struct Enrollment {
    /// The CA key: PKCS#8 DER, a regular file of this user closed to group and others.
    #[arg(long)]
    pub key_file: PathBuf,
    /// The CA certificate, DER (the coordinator's `<key>.cert`).
    #[arg(long)]
    pub certificate_file: PathBuf,
    /// The `ca_sha256` of a daemon config: 64 hex digits.
    #[arg(long)]
    pub ca_sha256: String,
}

/// The flags of `check-ca ssh`.
#[derive(Clone, Debug, Args)]
pub struct Ssh {
    /// The CA's private key file: one OpenSSH key, or two one after the other, a regular file of
    /// this user with mode 0600 or 0400.
    #[arg(long)]
    pub key_file: PathBuf,
    /// The public keys the hosts trust, one `authorized_keys` line each.
    #[arg(long)]
    pub public_file: PathBuf,
}

/// Why a copy is not the CA, or could not be checked. Nothing was written.
#[derive(Debug, thiserror::Error)]
pub enum CheckCaError {
    /// The enrollment CA's files could not be loaded as the coordinator loads them.
    #[error(transparent)]
    Enrollment(#[from] CaError),
    /// `--ca-sha256` is not a SHA-256.
    #[error("--ca-sha256: not a SHA-256 of 64 hex digits")]
    BadPin,
    /// The certificate is not the one the daemons pin.
    #[error("the certificate's SHA-256 is {found}, not the --ca-sha256 the daemons pin, {pinned}")]
    NotPinned { found: String, pinned: String },
    /// The SSH key file could not be read, or breaks the rule of a secret file.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The SSH key file is larger than two keys.
    #[error("--key-file: larger than {MAX_SECRET_BYTES} bytes")]
    KeyTooLarge,
    /// The SSH key file does not hold one or two unencrypted Ed25519 OpenSSH keys.
    #[error("--key-file: {0}")]
    Key(KeyError),
    /// The public key file could not be read.
    #[error("--public-file: {}: {source}", .path.display())]
    Public {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A line of the public key file is not a public key.
    #[error("--public-file: line {line}: {why}")]
    PublicLine { line: usize, why: KeyError },
    /// The public key file holds no key.
    #[error("--public-file: holds no public key")]
    NoPublicKey,
    /// A key of the key file is not one the hosts trust.
    #[error("the key {fingerprint} is not a line of --public-file")]
    NotTrusted { fingerprint: String },
}

/// Runs the verb `command`.
///
/// # Errors
/// [`CheckCaError`]: the copy is not the CA the fleet trusts, or could not be read.
pub fn run(command: &CheckCaCommand) -> Result<Report, CheckCaError> {
    match command {
        CheckCaCommand::Enrollment(flags) => enrollment(flags),
        CheckCaCommand::Ssh(flags) => ssh(flags),
    }
}

/// `enrollment`: the key and certificate load as one CA, and the certificate is the pinned one.
fn enrollment(flags: &Enrollment) -> Result<Report, CheckCaError> {
    let pinned = pin(&flags.ca_sha256)?;
    let files = CaFiles {
        key: &flags.key_file,
        certificate: &flags.certificate_file,
    };
    // The clock is never read: `open` loads, it does not sign.
    let ca = Ca::open(&RealFs, files, Arc::new(SystemClock))?;
    let found = ca.bundle().sha256_hex();
    if found != pinned {
        return Err(CheckCaError::NotPinned { found, pinned });
    }
    Ok(Report::of(vec![
        format!(
            "the key {} is the key of the certificate {}",
            flags.key_file.display(),
            flags.certificate_file.display()
        ),
        format!("ca_sha256: {found}"),
        "the certificate is the one --ca-sha256 pins".to_owned(),
    ]))
}

/// `text` as the daemon config reads `ca_sha256`: 64 hex digits of either case, lowercased.
fn pin(text: &str) -> Result<String, CheckCaError> {
    if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(CheckCaError::BadPin)
    }
}

/// `ssh`: every key of the key file is a line of the public file.
fn ssh(flags: &Ssh) -> Result<Report, CheckCaError> {
    let secret = read_secret(&flags.key_file, "--key-file", MAX_SECRET_BYTES)?;
    if secret.len() as u64 > MAX_SECRET_BYTES {
        return Err(CheckCaError::KeyTooLarge);
    }
    let keys = CaKey::parse_secret(&String::from_utf8_lossy(&secret)).map_err(CheckCaError::Key)?;
    let trusted = trusted(&flags.public_file)?;
    let mut lines = Vec::new();
    for key in &keys {
        let fingerprint = key.fingerprint();
        if !trusted.contains(&fingerprint) {
            return Err(CheckCaError::NotTrusted { fingerprint });
        }
        lines.push(format!("fingerprint: {fingerprint}"));
    }
    lines.push(format!(
        "every key of {} is a line of {}",
        flags.key_file.display(),
        flags.public_file.display()
    ));
    Ok(Report::of(lines))
}

/// The fingerprints of the public keys in the file at `path`; blank lines and `#` comments
/// are skipped, as sshd skips them in `TrustedUserCAKeys`.
fn trusted(path: &Path) -> Result<Vec<String>, CheckCaError> {
    let read = |source| CheckCaError::Public {
        path: path.to_owned(),
        source,
    };
    let meta = fs::metadata(path).map_err(read)?;
    if meta.len() > MAX_PUBLIC_BYTES {
        return Err(read(io::Error::other("too long for a public key file")));
    }
    let text = fs::read_to_string(path).map_err(read)?;
    let mut fingerprints = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = UserKey::parse(line).map_err(|why| CheckCaError::PublicLine {
            line: index + 1,
            why,
        })?;
        fingerprints.push(parsed.fingerprint);
    }
    if fingerprints.is_empty() {
        return Err(CheckCaError::NoPublicKey);
    }
    Ok(fingerprints)
}

#[cfg(test)]
#[path = "ca_check_tests.rs"]
mod tests;
