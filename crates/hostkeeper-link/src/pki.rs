//! [`PkiCertSource`]: the host's client certificate and key, read from the pki directory at
//! every heartbeat (docs/design/hostkeeper-design.md, section 10).
//!
//! Enrollment (`hostkeeper-enroll`) writes the directory; this module only reads it:
//!
//! ```text
//! <pki>/
//!   gen-<n>/
//!     cert.pem          the host certificate, PEM
//!     key               the key it was issued for, PKCS#8 DER
//!   current -> gen-<n>  the newest complete generation, switched by rename
//! ```
//!
//! At each call the `current` link is read once and both files are read from the generation it
//! names, so a switch of `current` between the two reads cannot pair the certificate of one
//! generation with the key of another. A complete generation is never rewritten.
//!
//! - No `current` link: "not enrolled", waiting for `<pki>/current`, and nothing is kept.
//! - A certificate file that begins with a PEM certificate (enrollment writes that one
//!   certificate only) and a P-256 PKCS#8 key whose public key is that certificate's: the pair,
//!   the certificate file as read and the key as PKCS#8 PEM. It is kept as the last good pair.
//! - Anything else (a file that cannot be read, a certificate file that does not begin with a
//!   PEM certificate, a key that is not a P-256 PKCS#8 key, a key that does not match its
//!   certificate): not used. The last good pair is used instead, or "not enrolled" when there is
//!   none. The refusal is logged once, and
//!   again only when what was read changes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use p256::ecdsa::SigningKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use x509_cert::Certificate;
use x509_cert::der::{DecodePem, Encode};

use crate::ports::{CertSource, CertState, ClientIdentity, Log};

/// The link to the generation in use (as `hostkeeper-enroll` names it).
pub(crate) const CURRENT: &str = "current";
/// The host certificate in a generation.
const CERT: &str = "cert.pem";
/// The key in a generation.
const KEY: &str = "key";
/// The CA bundle in a generation, which [`crate::JsonMtlsLink`] takes its root from.
pub(crate) const CA: &str = "ca.pem";

/// The file-based [`CertSource`]: the pair under the `current` link of a pki directory.
pub struct PkiCertSource {
    root: PathBuf,
    log: Arc<dyn Log>,
    kept: Mutex<Kept>,
}

/// What the source remembers between two heartbeats.
#[derive(Default)]
struct Kept {
    /// The last pair that was used.
    good: Option<ClientIdentity>,
    /// The last refusal logged, so that it is logged once.
    refused: Option<Refused>,
}

/// A pair that is not used: why, and the bytes that were read (empty for a file that could not
/// be read), which tell one refusal from the next.
#[derive(PartialEq, Eq)]
struct Refused {
    fault: String,
    cert: Vec<u8>,
    key: Vec<u8>,
}

/// What one read of the directory found.
enum Read {
    Absent,
    Pair(ClientIdentity),
    Refused(Refused),
}

impl PkiCertSource {
    /// The source over the pki directory `root`, logging its refusals to `log`.
    pub fn new(root: &Path, log: Arc<dyn Log>) -> PkiCertSource {
        PkiCertSource {
            root: root.to_path_buf(),
            log,
            kept: Mutex::new(Kept::default()),
        }
    }
}

impl CertSource for PkiCertSource {
    fn current(&self) -> CertState {
        let link = self.root.join(CURRENT);
        // A panic while the lock was held left `Kept` whole: each field is set in one step.
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        match read(&self.root, &link) {
            Read::Absent => {
                *kept = Kept::default();
                CertState::NotEnrolled { waiting_for: link }
            }
            Read::Pair(identity) => {
                kept.refused = None;
                kept.good = Some(identity.clone());
                CertState::Enrolled(identity)
            }
            Read::Refused(refused) => {
                if kept.refused.as_ref() != Some(&refused) {
                    let instead = match kept.good {
                        Some(_) => "the last good pair is kept",
                        None => "no pair is used",
                    };
                    self.log.line(&format!(
                        "error: the client certificate and key are not used: {}; {instead}",
                        refused.fault
                    ));
                }
                kept.refused = Some(refused);
                match &kept.good {
                    Some(good) => CertState::Enrolled(good.clone()),
                    None => CertState::NotEnrolled { waiting_for: link },
                }
            }
        }
    }
}

/// Reads the generation `link` names under `root`, and checks its pair.
fn read(root: &Path, link: &Path) -> Read {
    let generation = match fs::read_link(link) {
        Ok(target) => root.join(target),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Read::Absent,
        Err(e) => return Read::Refused(unreadable(link, &e)),
    };
    let (cert_path, key_path) = (generation.join(CERT), generation.join(KEY));
    let (cert, key) = match (fs::read(&cert_path), fs::read(&key_path)) {
        (Ok(cert), Ok(key)) => (cert, key),
        (Err(e), _) => return Read::Refused(unreadable(&cert_path, &e)),
        (_, Err(e)) => return Read::Refused(unreadable(&key_path, &e)),
    };
    match pair(&cert, &key) {
        Ok(key_pem) => Read::Pair(ClientIdentity::new(cert, key_pem)),
        Err(fault) => Read::Refused(Refused {
            fault: fault.text(&cert_path, &key_path),
            cert,
            key,
        }),
    }
}

fn unreadable(path: &Path, e: &io::Error) -> Refused {
    Refused {
        fault: format!("{} cannot be read: {e}", path.display()),
        cert: Vec::new(),
        key: Vec::new(),
    }
}

/// Why a certificate and a key that were read are not a pair.
enum PairFault {
    NotPem,
    NotKey,
    Mismatch,
}

impl PairFault {
    fn text(&self, cert: &Path, key: &Path) -> String {
        let (cert, key) = (cert.display(), key.display());
        match self {
            PairFault::NotPem => format!("{cert} does not begin with a PEM certificate"),
            PairFault::NotKey => format!("{key} is not a P-256 PKCS#8 key"),
            PairFault::Mismatch => format!("the key {key} does not match the certificate {cert}"),
        }
    }
}

/// The key as PKCS#8 PEM, when `key_der` is the P-256 key of the certificate `cert_pem`.
fn pair(cert_pem: &[u8], key_der: &[u8]) -> Result<Vec<u8>, PairFault> {
    let leaf = Certificate::from_pem(cert_pem).map_err(|_| PairFault::NotPem)?;
    let key = SigningKey::from_pkcs8_der(key_der).map_err(|_| PairFault::NotKey)?;
    // Encoding a key that decoded does not fail; were it to, the pair is not used.
    let held = key.verifying_key().to_public_key_der().ok();
    let certified = leaf
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .ok();
    let pem = key.to_pkcs8_pem(LineEnding::LF).ok();
    match (held, certified, pem) {
        (Some(held), Some(certified), Some(pem)) if held.as_bytes() == certified.as_slice() => {
            Ok(pem.as_bytes().to_vec())
        }
        _ => Err(PairFault::Mismatch),
    }
}
