//! The CA's two files: its key and its certificate.
//!
//! - The key is one PKCS#8 DER file of an ECDSA P-256 key, mode 0600, owned by the user the
//!   coordinator runs as. [`Ca::open`] refuses a key file that is a symbolic link (it is opened
//!   with `O_NOFOLLOW`), that is not a regular file, that another user owns, or that its group
//!   or others may read or write; the check is made on the opened file (`fstat`).
//! - The certificate is the self-signed CA certificate, DER, mode 0644. Its SHA-256 is what
//!   every daemon config pins, so it is made once, by [`Ca::init`], and read back on every
//!   start; it is never made again from the key.
//!
//! Both are written whole through the [`Fs`] port (temporary file, fsync, rename, directory
//! fsync). The paths are the caller's (the coordinator's flags); nothing here picks one. The
//! sealed offline backup of the two files is an operator step (docs/operations/ca-backup.md).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};

use crate::ca::issue::{CA_SERIAL, CA_VALIDITY_SECS, Ca, ca_params, serial_number};
use crate::fs::{FileStat, Fs, write_atomic};
use crate::validity::{UnixClock, Window};

/// The paths of the CA's files.
#[derive(Clone, Copy, Debug)]
pub struct CaFiles<'a> {
    /// The key, PKCS#8 DER, mode 0600.
    pub key: &'a Path,
    /// The CA certificate, DER, mode 0644.
    pub certificate: &'a Path,
}

/// Why the CA could not start.
#[derive(Debug, thiserror::Error)]
pub enum CaError {
    /// A file could not be read or written.
    #[error("cannot use {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    /// The key file is not a regular file of this user closed to group and others.
    #[error(
        "the CA key file {path} must be a regular file owned by uid {uid} with mode 0600 \
         (it is {stat:?})"
    )]
    KeyNotPrivate {
        path: PathBuf,
        uid: u32,
        stat: FileStat,
    },
    /// `open` found no file at a path.
    #[error("{path} does not exist; make the CA first")]
    Missing { path: PathBuf },
    /// `init` found a file already at a path.
    #[error("{path} already exists; the CA is made once")]
    Exists { path: PathBuf },
    /// A path names no file in a directory.
    #[error("{path} does not name a file")]
    BadPath { path: PathBuf },
    /// The key file is not a PKCS#8 ECDSA P-256 key.
    #[error("the CA key file {path} is not a PKCS#8 ECDSA P-256 key: {reason}")]
    BadKey { path: PathBuf, reason: String },
    /// The certificate file is not a DER certificate.
    #[error("the CA certificate file {path} is not a DER certificate: {reason}")]
    BadCertificate { path: PathBuf, reason: String },
    /// The certificate file is not the certificate of the key.
    #[error("the CA certificate {path} does not carry the CA key's public key")]
    CertificateNotOfKey { path: PathBuf },
    /// The clock reads a time whose 20-year window a certificate cannot hold.
    #[error("the clock reads {now}, whose CA validity window does not fit a certificate")]
    ClockOutOfRange { now: u64 },
    /// The signing library failed.
    #[error("making the CA failed: {0}")]
    Signing(String),
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> CaError + '_ {
    move |source| CaError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The directory and file name of `path`.
fn split(path: &Path) -> Result<(&Path, &str), CaError> {
    let name = path.file_name().and_then(|name| name.to_str());
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    name.map(|name| (dir, name)).ok_or(CaError::BadPath {
        path: path.to_path_buf(),
    })
}

impl Ca {
    /// Makes the CA: a new P-256 key and its self-signed certificate, valid
    /// [`CA_VALIDITY_SECS`] from the clock's now, written to `files` (the key first).
    ///
    /// # Errors
    /// [`CaError::Exists`] when either file exists; [`CaError`] for a path, the clock or a
    /// write.
    pub fn init(fs: &dyn Fs, files: CaFiles<'_>, clock: Arc<dyn UnixClock>) -> Result<Ca, CaError> {
        let (key_dir, key_name) = split(files.key)?;
        let (cert_dir, cert_name) = split(files.certificate)?;
        for path in [files.key, files.certificate] {
            if fs.read(path).map_err(io_at(path))?.is_some() {
                return Err(CaError::Exists {
                    path: path.to_path_buf(),
                });
            }
        }
        let now = clock.now_unix();
        let window =
            Window::from_now(now, CA_VALIDITY_SECS).ok_or(CaError::ClockOutOfRange { now })?;
        let signing = |e: rcgen::Error| CaError::Signing(e.to_string());
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(signing)?;
        let mut params = ca_params();
        (params.not_before, params.not_after) = window.times();
        params.serial_number = Some(serial_number(CA_SERIAL));
        let certificate = params.self_signed(&key).map_err(signing)?.der().to_vec();
        write_atomic(fs, key_dir, key_name, &key.serialize_der(), 0o600)
            .map_err(io_at(files.key))?;
        write_atomic(fs, cert_dir, cert_name, &certificate, 0o644)
            .map_err(io_at(files.certificate))?;
        Ok(Ca::from_parts(key, certificate, clock))
    }

    /// Loads the CA made by [`Ca::init`].
    ///
    /// # Errors
    /// [`CaError::KeyNotPrivate`] for a key file open to others or not this user's, an error of
    /// kind `Io` for one that is a symbolic link, [`CaError::Missing`], [`CaError::BadKey`],
    /// [`CaError::BadCertificate`] and [`CaError::CertificateNotOfKey`].
    pub fn open(fs: &dyn Fs, files: CaFiles<'_>, clock: Arc<dyn UnixClock>) -> Result<Ca, CaError> {
        let missing = |path: &Path| CaError::Missing {
            path: path.to_path_buf(),
        };
        let (bytes, stat) = fs
            .read_private(files.key)
            .map_err(io_at(files.key))?
            .ok_or(missing(files.key))?;
        let uid = rustix::process::geteuid().as_raw();
        let private = (stat.is_file, stat.uid == uid, stat.mode & 0o077 == 0);
        if private != (true, true, true) {
            return Err(CaError::KeyNotPrivate {
                path: files.key.to_path_buf(),
                uid,
                stat,
            });
        }
        let bad_key = |reason: String| CaError::BadKey {
            path: files.key.to_path_buf(),
            reason,
        };
        let key = KeyPair::try_from(bytes.as_slice()).map_err(|e| bad_key(e.to_string()))?;
        if key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
            return Err(bad_key("the key is not ECDSA P-256".to_owned()));
        }
        let certificate = fs
            .read(files.certificate)
            .map_err(io_at(files.certificate))?
            .ok_or(missing(files.certificate))?;
        let parsed = x509_parser::parse_x509_certificate(&certificate).map_err(|e| {
            CaError::BadCertificate {
                path: files.certificate.to_path_buf(),
                reason: e.to_string(),
            }
        })?;
        if parsed.1.public_key().raw != key.subject_public_key_info().as_slice() {
            return Err(CaError::CertificateNotOfKey {
                path: files.certificate.to_path_buf(),
            });
        }
        Ok(Ca::from_parts(key, certificate, clock))
    }
}
