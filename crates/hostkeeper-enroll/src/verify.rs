//! The checks a certificate passes before it is stored, or used from the pki directory (design
//! section 5.7, step 4).
//!
//! [`check`], which reads no clock, in order:
//! - the host id is a host id (and, for a renewal or a return, the host's own);
//! - the certificate and the CA bundle are PEM;
//! - the bundle holds the CA whose SHA-256 is the pinned `ca_sha256`;
//! - the certificate is issued by that CA: its issuer is the CA's subject, both of its signature
//!   algorithm fields are ECDSA with SHA-256 without parameters, and its signature verifies under
//!   the CA's P-256 key;
//! - its key is the key the host holds;
//! - its subject is exactly `host:<id>`;
//! - it is not a CA, each of the extensions read here decodes and appears once, every
//!   extension it marks critical is one checked here (basic constraints,
//!   key usage, extended key usage, subject alternative name), a key usage it carries allows
//!   digital signatures, and its extended key usage allows client authentication.
//!
//! [`verify`] is [`check`] and the validity window on the wall clock.
//!
//! The CA of design section 4.4 issues host certificates itself, with a P-256 key, so a
//! certificate is checked against the pinned CA directly; there is no intermediate.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::der::oid::db::rfc5280::{
    ID_CE_BASIC_CONSTRAINTS, ID_CE_EXT_KEY_USAGE, ID_CE_KEY_USAGE, ID_CE_SUBJECT_ALT_NAME,
    ID_KP_CLIENT_AUTH,
};
use x509_cert::der::oid::db::rfc5912::ECDSA_WITH_SHA_256;
use x509_cert::der::{DecodePem, Encode};
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, KeyUsage};

use crate::key::HostKey;
use crate::validity::{self, WallClock};
use crate::wire::{HostId, Issued};

/// Why a certificate was refused. Nothing is stored for any of them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the coordinator named no valid host id")]
    NoHostId,
    #[error("the certificate is for host {issued}, not for this host, {held}")]
    OtherHost { held: HostId, issued: HostId },
    #[error("the certificate is not one PEM certificate")]
    NotPem,
    #[error("the CA bundle is not PEM certificates")]
    BundleNotPem,
    #[error("the CA bundle does not hold the CA whose SHA-256 is the pinned ca_sha256")]
    NoPinnedCa,
    #[error("the certificate's issuer is not the pinned CA")]
    Issuer,
    #[error("the certificate's signature algorithm is not ECDSA with SHA-256 in both its fields")]
    Algorithm,
    #[error("the certificate is not signed by the pinned CA")]
    NotSignedByCa,
    #[error("the certificate is for another key than the one this host holds")]
    OtherKey,
    #[error("the certificate's subject is not exactly {0}")]
    Subject(String),
    #[error("the certificate is a CA certificate")]
    Ca,
    #[error("the certificate marks the extension {0} critical, which this daemon does not check")]
    CriticalExtension(String),
    #[error("the certificate's key usage does not allow digital signatures")]
    KeyUsage,
    #[error("the certificate does not allow client authentication")]
    NoClientAuth,
    #[error("the certificate is not valid now: it is valid from {not_before} through {not_after}")]
    NotValidNow { not_before: u64, not_after: u64 },
}

/// A certificate that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verified {
    pub host_id: HostId,
    pub not_before: u64,
    pub not_after: u64,
}

/// The lower-case hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// [`check`], then the validity window on `wall`.
pub(crate) fn verify(
    issued: &Issued,
    key: &HostKey,
    pin: &str,
    held: Option<&HostId>,
    wall: &dyn WallClock,
) -> Result<Verified, Refusal> {
    let verified = check(issued, key, pin, held)?;
    if !validity::leaf_valid_now(wall, verified.not_before, verified.not_after) {
        return Err(Refusal::NotValidNow {
            not_before: verified.not_before,
            not_after: verified.not_after,
        });
    }
    Ok(verified)
}

/// Every check but the validity window; see the module documentation. `held` is the host's own
/// id when it renews or returns.
pub(crate) fn check(
    issued: &Issued,
    key: &HostKey,
    pin: &str,
    held: Option<&HostId>,
) -> Result<Verified, Refusal> {
    let host_id = HostId::new(&issued.host_id).ok_or(Refusal::NoHostId)?;
    if let Some(held) = held
        && *held != host_id
    {
        return Err(Refusal::OtherHost {
            held: held.clone(),
            issued: host_id,
        });
    }
    let leaf = Certificate::from_pem(issued.cert_pem.as_bytes()).map_err(|_| Refusal::NotPem)?;
    let bundle = Certificate::load_pem_chain(issued.ca_bundle_pem.as_bytes())
        .map_err(|_| Refusal::BundleNotPem)?;
    let ca = bundle
        .iter()
        .find(|ca| der_sha256(ca).as_deref() == Some(pin))
        .ok_or(Refusal::NoPinnedCa)?;
    let tbs = leaf.tbs_certificate();
    if tbs.issuer() != ca.tbs_certificate().subject() {
        return Err(Refusal::Issuer);
    }
    let algorithms = (
        &leaf.signature_algorithm().oid,
        &leaf.signature_algorithm().parameters,
        leaf.signature_algorithm() == tbs.signature(),
    );
    if algorithms != (&ECDSA_WITH_SHA_256, &None, true) {
        return Err(Refusal::Algorithm);
    }
    if !signed_by(&leaf, ca) {
        return Err(Refusal::NotSignedByCa);
    }
    if tbs.subject_public_key_info().to_der().ok().as_deref() != Some(key.spki_der()) {
        return Err(Refusal::OtherKey);
    }
    let subject = host_id.subject();
    if common_name(&leaf).as_deref() != Some(subject.as_str()) {
        return Err(Refusal::Subject(subject));
    }
    check_usage(&leaf)?;
    let (not_before, not_after) = window(&leaf);
    Ok(Verified {
        host_id,
        not_before,
        not_after,
    })
}

/// The certificate is an end entity for client authentication; see the module documentation.
fn check_usage(leaf: &Certificate) -> Result<(), Refusal> {
    let tbs = leaf.tbs_certificate();
    // An extension that does not decode, or appears twice, refuses as its check would.
    let constraints = tbs
        .get_extension::<BasicConstraints>()
        .map_err(|_| Refusal::Ca)?;
    if constraints.is_some_and(|(_, constraints)| constraints.ca) {
        return Err(Refusal::Ca);
    }
    let known = [
        ID_CE_BASIC_CONSTRAINTS,
        ID_CE_KEY_USAGE,
        ID_CE_EXT_KEY_USAGE,
        ID_CE_SUBJECT_ALT_NAME,
    ];
    let extensions = tbs.extensions().map_or(&[][..], Vec::as_slice);
    if let Some(unknown) = extensions
        .iter()
        .find(|ext| ext.critical & !known.contains(&ext.extn_id))
    {
        return Err(Refusal::CriticalExtension(unknown.extn_id.to_string()));
    }
    let usage = tbs
        .get_extension::<KeyUsage>()
        .map_err(|_| Refusal::KeyUsage)?;
    if usage.is_some_and(|(_, usage)| !usage.digital_signature()) {
        return Err(Refusal::KeyUsage);
    }
    let usage = tbs
        .get_extension::<ExtendedKeyUsage>()
        .map_err(|_| Refusal::NoClientAuth)?;
    if !usage.is_some_and(|(_, usage)| usage.0.contains(&ID_KP_CLIENT_AUTH)) {
        return Err(Refusal::NoClientAuth);
    }
    Ok(())
}

/// The SHA-256 of the certificate's DER.
fn der_sha256(cert: &Certificate) -> Option<String> {
    cert.to_der().ok().map(|der| sha256_hex(&der))
}

/// The certificate's signature verifies under `ca`'s P-256 key.
fn signed_by(leaf: &Certificate, ca: &Certificate) -> bool {
    let key = ca
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .ok()
        .and_then(|der| VerifyingKey::from_public_key_der(&der).ok());
    let signature = leaf
        .signature()
        .as_bytes()
        .and_then(|bytes| DerSignature::from_bytes(bytes).ok());
    let signed = leaf.tbs_certificate().to_der().ok();
    match (key, signature, signed) {
        (Some(key), Some(signature), Some(signed)) => key.verify(&signed, &signature).is_ok(),
        _ => false,
    }
}

/// The subject's common name when the subject is that one attribute and nothing else.
pub(crate) fn common_name(cert: &Certificate) -> Option<String> {
    let subject = cert.tbs_certificate().subject();
    let name = subject.common_name().ok().flatten().map(String::from);
    (subject.iter().count() == 1).then_some(name).flatten()
}

/// `not_before` and `not_after` in seconds since the Unix epoch.
pub(crate) fn window(cert: &Certificate) -> (u64, u64) {
    let validity = cert.tbs_certificate().validity();
    (
        validity.not_before.to_unix_duration().as_secs(),
        validity.not_after.to_unix_duration().as_secs(),
    )
}
