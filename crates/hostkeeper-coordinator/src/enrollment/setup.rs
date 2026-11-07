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
//!   which the heartbeat listener serves, and the LAN listener with it (its chain ends in the CA
//!   a daemon pins). The start makes a new one, with a new key, a serial from the store, the DNS
//!   names of `--server-name` and the IP address of `--lan-listen`, valid
//!   [`SERVER_VALIDITY_CAP_SECS`], when the file is missing, does not start with a certificate,
//!   has less than a third of its validity left ([`server_due`]), or names other names than
//!   those (a name changed, added or removed; the order is not looked at).
//!   While the coordinator runs, each tick of the reload loop asks the same question of the
//!   file and makes it again the same way ([`ServerFile::keep`], through
//!   [`crate::enrollment::Enrollment::renew_server`]). The new file is written whole: a
//!   temporary file beside it (mode 0600), fsync, rename over the old one, fsync of the
//!   directory; a write that fails removes the temporary file and leaves the old one served.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use hostkeeper_enrollment::ca::{
    Ca, CaError, CaFiles, P256Key, SERVER_VALIDITY_CAP_SECS, ServerNames, Signer,
};
use hostkeeper_enrollment::enroll::{EnrollmentStore, StoreError};
use hostkeeper_enrollment::fs::{Fs, temp_name, write_atomic};
use hostkeeper_enrollment::validity::UnixClock;
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};
use x509_parser::extensions::GeneralName;

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
    /// The IP addresses the server certificate names besides: the address of `--lan-listen`,
    /// when it is given.
    pub server_ips: &'a [IpAddr],
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

/// One subjectAltName of a server certificate, as the names it is made for are compared.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Name {
    /// A DNS name.
    Dns(String),
    /// An IP address: its 4 or 16 bytes, as the certificate holds them.
    Ip(Vec<u8>),
    /// Any other name, which the coordinator never asks for.
    Other,
}

impl Name {
    /// `name`, as a certificate's subjectAltName.
    fn of(name: &GeneralName<'_>) -> Name {
        match name {
            GeneralName::DNSName(dns) => Name::Dns((*dns).to_owned()),
            GeneralName::IPAddress(bytes) => Name::Ip(bytes.to_vec()),
            _ => Name::Other,
        }
    }

    /// `ip` as a certificate holds it.
    fn ip(ip: &IpAddr) -> Name {
        Name::Ip(match ip {
            IpAddr::V4(v4) => v4.octets().to_vec(),
            IpAddr::V6(v6) => v6.octets().to_vec(),
        })
    }
}

/// What the certificate a server certificate file starts with holds that decides whether it is
/// made again: its validity, as unix seconds `(not_before, not_after)`, and its subjectAltNames
/// (none when it has no such extension, or one that does not parse).
struct Held {
    window: (i64, i64),
    names: BTreeSet<Name>,
}

/// What the certificate the server certificate file whose bytes are `file` starts with holds;
/// `None` when its first PEM block is no certificate.
fn held_of(file: &[u8]) -> Option<Held> {
    let (_, block) = x509_parser::pem::parse_x509_pem(file).ok()?;
    let certificate = block.parse_x509().ok()?;
    let validity = certificate.validity();
    let names = certificate
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| san.value.general_names.iter().map(Name::of).collect())
        .unwrap_or_default();
    Some(Held {
        window: (
            validity.not_before.timestamp(),
            validity.not_after.timestamp(),
        ),
        names,
    })
}

/// The validity of the certificate the server certificate file whose bytes are `file` starts
/// with, as unix seconds `(not_before, not_after)`; `None` when its first PEM block is no
/// certificate.
fn validity_of(file: &[u8]) -> Option<(i64, i64)> {
    held_of(file).map(|held| held.window)
}

/// Whether the server certificate file whose bytes are `file` is made again at `now` (unix
/// seconds): there is none, its first PEM block is no certificate, or less than a third of its
/// validity is left.
#[must_use]
pub fn server_due(file: Option<&[u8]>, now: u64) -> bool {
    due(file.and_then(validity_of), now)
}

/// [`server_due`] of a file whose certificate's validity is `window` (`None`: no certificate).
fn due(window: Option<(i64, i64)>, now: u64) -> bool {
    window.is_none_or(|(start, end)| {
        let third = end.saturating_sub(start) / 3;
        i64::try_from(now).unwrap_or(i64::MAX) >= end.saturating_sub(third)
    })
}

/// The coordinator's server certificate file (`--server-cert-file`) and the names it is made
/// for (the DNS names of `--server-name`, the address of `--lan-listen`), written through a
/// file-system port.
pub struct ServerFile {
    fs: Arc<dyn Fs>,
    path: PathBuf,
    dir: PathBuf,
    name: String,
    names: Vec<String>,
    ips: Vec<IpAddr>,
}

impl std::fmt::Debug for ServerFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerFile")
            .field("path", &self.path)
            .field("names", &self.names)
            .field("ips", &self.ips)
            .finish_non_exhaustive()
    }
}

/// What one look at the server certificate file found and did ([`ServerFile::keep`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    /// The file was made again.
    pub renewed: bool,
    /// The end of the validity of the certificate the file now starts with, in unix seconds.
    pub not_after: u64,
}

impl ServerFile {
    /// The file `path` for the DNS names `names` and the IP addresses `ips`, through `fs`.
    ///
    /// # Errors
    /// The text of why: `path` does not name a file.
    pub fn new(
        fs: Arc<dyn Fs>,
        path: &Path,
        names: &[String],
        ips: &[IpAddr],
    ) -> Result<ServerFile, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "does not name a file".to_owned())?;
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(ServerFile {
            fs,
            path: path.to_path_buf(),
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            names: names.to_vec(),
            ips: ips.to_vec(),
        })
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The names the file is made for, as a certificate's subjectAltNames are compared.
    fn wanted(&self) -> BTreeSet<Name> {
        let dns = self.names.iter().cloned().map(Name::Dns);
        dns.chain(self.ips.iter().map(Name::ip)).collect()
    }

    /// Reads the file and, when it is due at `now` ([`server_due`]) or its certificate names
    /// other names than the file is made for (a DNS name or an IP address added, removed or
    /// changed; their order is not looked at), makes it again: a new key, a serial from
    /// `store`, a certificate `ca` signs for [`SERVER_VALIDITY_CAP_SECS`], written
    /// whole (a temporary file of mode 0600 beside it, fsync, rename over it, fsync of the
    /// directory). A write that fails removes the temporary file; the file is then as it was.
    ///
    /// # Errors
    /// The text of why: the file cannot be read, the key, serial or certificate cannot be
    /// made, or the file cannot be written.
    pub fn keep(&self, ca: &Ca, store: &EnrollmentStore, now: u64) -> Result<Kept, String> {
        let existing = self.fs.read(&self.path).map_err(|e| e.to_string())?;
        let held = existing.as_deref().and_then(held_of);
        let renamed = held
            .as_ref()
            .is_some_and(|held| held.names != self.wanted());
        if renamed {
            tracing::info!(
                names = ?self.names,
                ips = ?self.ips,
                "the server certificate names other names than the flags: making it again"
            );
        }
        // A certificate that is not due ends after `now`, so its end is a `u64`.
        let in_use = held
            .map(|held| held.window)
            .filter(|window| !renamed && !due(Some(*window), now))
            .and_then(|(_, end)| u64::try_from(end).ok());
        match in_use {
            Some(not_after) => Ok(Kept {
                renewed: false,
                not_after,
            }),
            None => {
                let names = ServerNames {
                    dns: &self.names,
                    ips: &self.ips,
                };
                let (text, not_after) = server_pem(ca, store, names)?;
                self.write(text.as_bytes())?;
                Ok(Kept {
                    renewed: true,
                    not_after,
                })
            }
        }
    }

    /// Writes `bytes` over the file whole, removing the temporary file when a step fails.
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        write_atomic(&*self.fs, &self.dir, &self.name, bytes, SERVER_FILE_MODE).map_err(|why| {
            // The rename is the last step that can leave the temporary file; once it is done
            // removing the name is a no-op.
            let _ = self.fs.remove(&self.dir.join(temp_name(&self.name)));
            why.to_string()
        })
    }
}

/// Opens or makes the CA, opens the store and makes the server certificate when it is due
/// ([`ServerFile::keep`]), all through `fs`, at the time `clock` reads; the poll nonces are
/// drawn from `random`. The enrollment keeps the server certificate file, so the reload loop
/// renews it ([`Enrollment::renew_server`]).
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
    let file = ServerFile::new(fs, setup.server_cert, setup.server_names, setup.server_ips)
        .map_err(server)?;
    let kept = file.keep(&ca, &store, clock.now_unix()).map_err(server)?;
    if kept.renewed {
        tracing::info!(
            names = ?setup.server_names,
            ips = ?setup.server_ips,
            "made the coordinator's server certificate"
        );
    }
    Ok(Enrollment::new(
        store,
        ca,
        file,
        kept.not_after,
        clock,
        random,
    ))
}

/// A new server certificate for `names`, as the file holds it (the certificate, the CA
/// certificate, and the new key), and the end of its validity in unix seconds.
fn server_pem(
    ca: &Ca,
    store: &EnrollmentStore,
    names: ServerNames<'_>,
) -> Result<(String, u64), String> {
    let pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let key = P256Key::from_spki(&pair.subject_public_key_info()).map_err(|e| e.to_string())?;
    let serial = store.allocate_serial().map_err(|e| e.to_string())?;
    let certificate = ca
        .issue_server(&key, names, SERVER_VALIDITY_CAP_SECS, serial)
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}{}",
        pem("CERTIFICATE", &certificate.der),
        pem("CERTIFICATE", ca.bundle().certificate_der()),
        pair.serialize_pem()
    );
    Ok((text, certificate.not_after))
}
