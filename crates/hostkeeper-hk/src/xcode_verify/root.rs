//! Apple's published root certificate: read from the file `--apple-root` names, or fetched over
//! HTTPS from Apple's certificate authority page and trusted by the system's roots. Its SHA-256
//! fingerprint is computed from the DER and compared with the one the code holds; its subject is
//! recorded.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use crate::call::from_der;
use crate::send::{Limits, Method, certificates, send};
use crate::xcode_push::Mac;
use crate::xcode_record::AppleRoot;

/// What the certificate is, when it could be had: its SHA-256 fingerprint and its subject.
struct Certificate {
    sha256: String,
    subject: String,
}

/// The bytes of the certificate: the file, or Apple's page over HTTPS.
fn bytes(
    mac: &Mac,
    file: Option<&Path>,
    roots: &dyn Fn() -> Vec<Vec<u8>>,
    timeout: u32,
) -> Result<Vec<u8>, String> {
    if let Some(file) = file {
        return std::fs::read(file)
            .map_err(|error| format!("cannot read {} ({error})", file.display()));
    }
    let wait = Duration::from_secs(u64::from(timeout));
    let limits = Limits {
        connect: wait,
        total: wait,
    };
    let answer = send(
        &mac.apple_root_url,
        &Method::Get(None),
        &from_der(&roots()),
        limits,
    )
    .map_err(|error| format!("cannot fetch {} ({error})", mac.apple_root_url))?;
    if answer.status != 200 {
        return Err(format!(
            "{} was answered HTTP {}",
            mac.apple_root_url, answer.status
        ));
    }
    Ok(answer.body)
}

/// The certificate in `bytes`, DER or PEM: its fingerprint and subject.
fn certificate(bytes: &[u8]) -> Result<Certificate, String> {
    let der = if bytes.starts_with(b"-----BEGIN") {
        match certificates(bytes).first() {
            Some(cert) => cert.der().to_vec(),
            None => return Err("holds no certificate".to_owned()),
        }
    } else {
        bytes.to_vec()
    };
    let (_, parsed) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| "is not a certificate, in DER or PEM".to_owned())?;
    Ok(Certificate {
        sha256: Sha256::digest(&der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect(),
        subject: parsed.subject().to_string(),
    })
}

/// Apple's published root: from `file` or from the page, and whether its fingerprint is the one
/// the Mac's `apple_root_sha256` holds.
pub(super) fn published(
    mac: &Mac,
    file: Option<&Path>,
    roots: &dyn Fn() -> Vec<Vec<u8>>,
    timeout: u32,
) -> AppleRoot {
    let source = match file {
        Some(file) => file.display().to_string(),
        None => mac.apple_root_url.clone(),
    };
    let read = bytes(mac, file, roots, timeout)
        .and_then(|bytes| certificate(&bytes).map_err(|why| format!("{source} {why}")));
    let constant = mac.apple_root_sha256.clone();
    match read {
        Ok(found) => AppleRoot {
            equal: found.sha256 == mac.apple_root_sha256,
            source,
            sha256: Some(found.sha256),
            subject: Some(found.subject),
            constant,
            error: None,
        },
        Err(error) => AppleRoot {
            source,
            sha256: None,
            subject: None,
            constant,
            equal: false,
            error: Some(error),
        },
    }
}
