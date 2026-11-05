//! The start of enrollment: the CA, the store, and the coordinator's server certificate.
//!
//! - The CA's key is the file `--ca-key-file` names (PKCS#8, mode 0600, owned by the user the
//!   coordinator runs as; a key file open to group or others, another user's, or a symbolic
//!   link stops the start). Its certificate is beside it, `<ca-key-file>.cert` (DER, 0644).
//!   When the key file does not exist, `--ca-init` makes both; without it the start stops. The
//!   CA is made once: every daemon config pins the SHA-256 of its certificate, which the start
//!   logs at info.
//! - The store is the directory `--enrollment-dir` names.
//! - The server certificate is the file `--server-cert-file` names: one PEM file, mode 0600,
//!   holding the certificate, the CA certificate and the certificate's P-256 key, in that order,
//!   which the heartbeat listener serves (its chain ends in the CA a daemon pins). The start
//!   makes a new one, with a new key, a serial from the store and the DNS names of
//!   `--server-name`, valid [`SERVER_VALIDITY_CAP_SECS`], when the file is missing, does not
//!   start with a certificate, or has less than a third of its validity left. Nothing else
//!   renews it; a coordinator is restarted within its validity.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use hostkeeper_enrollment::ca::{Ca, CaError, CaFiles, P256Key, SERVER_VALIDITY_CAP_SECS, Signer};
use hostkeeper_enrollment::enroll::{EnrollmentStore, StoreError};
use hostkeeper_enrollment::fs::{Fs, write_atomic};
use hostkeeper_enrollment::validity::UnixClock;
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};

use super::{Enrollment, Random};

/// What is added to the CA key file's path to name the CA certificate's.
pub const CA_CERT_SUFFIX: &str = ".cert";

/// The mode of the server certificate file, which holds its key.
const SERVER_FILE_MODE: u32 = 0o600;

/// The files and flags enrollment starts from.
#[derive(Clone, Copy, Debug)]
pub struct Setup<'a> {
    /// `--enrollment-dir`.
    pub dir: &'a Path,
    /// `--ca-key-file`.
    pub ca_key: &'a Path,
    /// `--ca-init`.
    pub ca_init: bool,
    /// `--server-cert-file`.
    pub server_cert: &'a Path,
    /// `--server-name`, each.
    pub server_names: &'a [String],
}

/// Why enrollment did not start.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The CA's files are refused.
    #[error("--ca-key-file: {0}")]
    Ca(#[from] CaError),
    /// There is no CA and `--ca-init` was not given.
    #[error(
        "--ca-key-file: {} does not exist; start once with --ca-init to make the CA (every daemon config pins its certificate)",
        .0.display()
    )]
    NoCa(PathBuf),
    /// The store's directory is refused.
    #[error("--enrollment-dir: {0}")]
    Store(#[from] StoreError),
    /// The server certificate could not be read, made or written.
    #[error("--server-cert-file {}: {reason}", path.display())]
    Server {
        /// The file.
        path: PathBuf,
        /// What failed.
        reason: String,
    },
}

/// The CA certificate's path: the key file's with [`CA_CERT_SUFFIX`] added.
#[must_use]
pub fn ca_certificate_path(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_owned();
    name.push(CA_CERT_SUFFIX);
    PathBuf::from(name)
}

/// `der` as one PEM block labelled `label`: base64 in lines of 64 characters.
#[must_use]
pub fn pem(label: &str, der: &[u8]) -> String {
    let text = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in text.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}

/// Whether the server certificate file whose bytes are `file` is made again at `now` (unix
/// seconds): there is none, its first PEM block is no certificate, or less than a third of its
/// validity is left.
#[must_use]
pub fn server_due(file: Option<&[u8]>, now: u64) -> bool {
    let window = file
        .and_then(|bytes| x509_parser::pem::parse_x509_pem(bytes).ok())
        .and_then(|(_, block)| {
            let certificate = block.parse_x509().ok()?;
            let validity = certificate.validity();
            Some((
                validity.not_before.timestamp(),
                validity.not_after.timestamp(),
            ))
        });
    window.is_none_or(|(start, end)| {
        let third = end.saturating_sub(start) / 3;
        i64::try_from(now).unwrap_or(i64::MAX) >= end.saturating_sub(third)
    })
}

/// Opens or makes the CA, opens the store and makes the server certificate when it is due, all
/// through `fs`, at the time `clock` reads; the poll nonces are drawn from `random`.
///
/// # Errors
/// [`SetupError`]: the CA's files are refused or missing without `--ca-init`, the store's
/// directory is refused, or the server certificate cannot be read, made or written.
pub fn setup(
    fs: Arc<dyn Fs>,
    setup: &Setup<'_>,
    clock: Arc<dyn UnixClock>,
    random: Random,
) -> Result<Enrollment, SetupError> {
    let certificate = ca_certificate_path(setup.ca_key);
    let files = CaFiles {
        key: setup.ca_key,
        certificate: &certificate,
    };
    let ca = match Ca::open(&*fs, files, Arc::clone(&clock)) {
        Ok(ca) => ca,
        Err(CaError::Missing { .. }) if setup.ca_init => Ca::init(&*fs, files, Arc::clone(&clock))?,
        Err(CaError::Missing { path }) => return Err(SetupError::NoCa(path)),
        Err(other) => return Err(other.into()),
    };
    tracing::info!(ca_sha256 = %ca.bundle().sha256_hex(), "the enrollment CA");
    let store = EnrollmentStore::open(Arc::clone(&fs), setup.dir)?;
    let server = |reason: String| SetupError::Server {
        path: setup.server_cert.to_path_buf(),
        reason,
    };
    let name = setup
        .server_cert
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| server("does not name a file".to_owned()))?;
    let dir = setup
        .server_cert
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let existing = fs
        .read(setup.server_cert)
        .map_err(|e| server(e.to_string()))?;
    if server_due(existing.as_deref(), clock.now_unix()) {
        let text = server_pem(&ca, &store, setup.server_names).map_err(server)?;
        write_atomic(&*fs, dir, name, text.as_bytes(), SERVER_FILE_MODE)
            .map_err(|e| server(e.to_string()))?;
        tracing::info!(names = ?setup.server_names, "made the coordinator's server certificate");
    }
    Ok(Enrollment::new(store, ca, clock, random))
}

/// A new server certificate for `names`, as the file holds it: the certificate, the CA
/// certificate, and the new key.
fn server_pem(ca: &Ca, store: &EnrollmentStore, names: &[String]) -> Result<String, String> {
    let pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let key = P256Key::from_spki(&pair.subject_public_key_info()).map_err(|e| e.to_string())?;
    let serial = store.allocate_serial().map_err(|e| e.to_string())?;
    let certificate = ca
        .issue_server(&key, names, SERVER_VALIDITY_CAP_SECS, serial)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "{}{}{}",
        pem("CERTIFICATE", &certificate.der),
        pem("CERTIFICATE", ca.bundle().certificate_der()),
        pair.serialize_pem()
    ))
}
