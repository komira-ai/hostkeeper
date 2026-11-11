//! Apple's roots, which the device blob's chain must end at.
//!
//! The two are the public certificates Apple lists at `apple.com/certificateauthority`: Apple Root
//! CA (RSA, SHA-1 signed, to 2035) and Apple Root CA - G3 (ECC, to 2039). Their fingerprints are in
//! `docs/design/coordinator-mdm-cm12a.md`. The intermediates a Mac's device identity chains
//! through are expected in the blob itself; whether Apple publishes them is not established.
use x509_cert::Certificate;
use x509_cert::der::{DecodePem, Encode};

const APPLE_ROOT_CA: &str = include_str!("../roots/apple-root-ca.pem");
const APPLE_ROOT_CA_G3: &str = include_str!("../roots/apple-root-ca-g3.pem");

/// The DER of Apple Root CA and of Apple Root CA - G3.
///
/// A root that did not read would be left out, which refuses every blob (fail closed); a test
/// reads both.
#[must_use]
pub fn apple() -> Vec<Vec<u8>> {
    [APPLE_ROOT_CA, APPLE_ROOT_CA_G3]
        .iter()
        .filter_map(|pem| Certificate::from_pem(pem).ok()?.to_der().ok())
        .collect()
}
