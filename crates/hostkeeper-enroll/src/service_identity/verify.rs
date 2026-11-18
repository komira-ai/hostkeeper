//! The checks a service certificate passes before it is installed (docs/design/service-identity.md,
//! section 4), in this order:
//!
//! - it parses: DER, or one PEM block of DER;
//! - the CA bundle that came with it is PEM certificates;
//! - its signature algorithm is ECDSA with SHA-256 in both fields, and a CA of the bundle the
//!   host trusts (the current `ca.pem` of its pki directory) issued it and signed it;
//! - its extended key usage is `clientAuth` alone; it is an end entity whose extensions are the
//!   ones [`crate::verify::check_usage`] reads, and its key usage allows digital signatures;
//! - its subject is exactly `service:<name>`, and its subject alternative name is exactly the URI
//!   `urn:hostkeeper:service:<name>` and the DNS name of the host's own id;
//! - its key is the key the daemon holds for the service;
//! - `notBefore <= now < notAfter` on the wall clock.
//!
//! Whether it is newer than the installed certificate is the caller's check.

use x509_cert::Certificate;
use x509_cert::der::oid::db::rfc5280::ID_KP_CLIENT_AUTH;
use x509_cert::der::oid::db::rfc5912::ECDSA_WITH_SHA_256;
use x509_cert::der::pem::{LineEnding, encode_string};
use x509_cert::der::{Decode, DecodePem, Encode};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{ExtendedKeyUsage, SubjectAltName};

use crate::key::HostKey;
use crate::validity::{self, WallClock};
use crate::verify::{check_usage, common_name, signed_by, window};
use crate::wire::HostId;

/// What a service certificate's common name begins with.
const COMMON_NAME_PREFIX: &str = "service:";
/// What the URI of a service certificate begins with.
const URI_PREFIX: &str = "urn:hostkeeper:service:";

/// Why a service certificate was refused. Nothing is installed for any of them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityRefusal {
    #[error("the certificate is neither DER nor one PEM certificate")]
    NotACertificate,
    #[error("the CA bundle that came with the certificate is not PEM certificates")]
    BundleNotPem,
    #[error("the host's own CA bundle is not PEM certificates")]
    TrustNotPem,
    #[error("the certificate's signature algorithm is not ECDSA with SHA-256 in both its fields")]
    Algorithm,
    #[error("the certificate is not signed by a CA this host trusts")]
    NotSignedByTrustedCa,
    #[error(
        "the certificate is a CA certificate, or an extension of it is not one this daemon accepts: {0}"
    )]
    Profile(String),
    #[error("the certificate's extended key usage is not client authentication alone")]
    ExtendedKeyUsage,
    #[error("the certificate's subject is not exactly {0}")]
    Subject(String),
    #[error("the certificate's subject alternative name is not exactly {uri} and {dns}")]
    AlternativeNames { uri: String, dns: String },
    #[error("the certificate is for another key than the one this host holds for the service")]
    OtherKey,
    #[error("the certificate is not valid now: it is valid from {not_before} until {not_after}")]
    NotValidNow { not_before: u64, not_after: u64 },
    #[error(
        "the certificate is not newer than the installed one: it is valid from {not_before}, the installed one from {installed}"
    )]
    NotNewer { not_before: u64, installed: u64 },
}

/// What a certificate must be for.
pub(crate) struct Expected<'a> {
    pub service: &'a str,
    pub host: &'a HostId,
    pub key: &'a HostKey,
    /// The CA bundle the host trusts, PEM.
    pub trusted_pem: &'a str,
}

/// A certificate that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    /// The certificate as PEM, what is installed.
    pub pem: String,
    pub not_before: u64,
    pub not_after: u64,
}

/// The refusal of anything that does not parse or encode as a certificate.
fn not_a_certificate<E>(_: E) -> IdentityRefusal {
    IdentityRefusal::NotACertificate
}

/// A certificate in the bytes the coordinator sent: PEM when it begins as PEM, else DER.
pub(crate) fn parse(bytes: &[u8]) -> Result<Certificate, IdentityRefusal> {
    let parsed = if bytes.starts_with(b"-----BEGIN") {
        Certificate::from_pem(bytes)
    } else {
        Certificate::from_der(bytes)
    };
    parsed.map_err(not_a_certificate)
}

/// The window of an installed certificate in the bytes of `cert.pem`; none when they are not a
/// certificate.
pub(crate) fn installed_window(bytes: &[u8]) -> Option<(u64, u64)> {
    parse(bytes).ok().map(|cert| window(&cert))
}

/// Every check of the module documentation but "newer than the installed one".
pub(crate) fn check(
    received: &[u8],
    bundle_pem: &str,
    expected: &Expected<'_>,
    wall: &dyn WallClock,
) -> Result<Checked, IdentityRefusal> {
    let leaf = parse(received)?;
    Certificate::load_pem_chain(bundle_pem.as_bytes())
        .map_err(|_| IdentityRefusal::BundleNotPem)?;
    let trusted = Certificate::load_pem_chain(expected.trusted_pem.as_bytes())
        .map_err(|_| IdentityRefusal::TrustNotPem)?;
    let tbs = leaf.tbs_certificate();
    let algorithm = leaf.signature_algorithm();
    if (
        &algorithm.oid,
        &algorithm.parameters,
        algorithm == tbs.signature(),
    ) != (&ECDSA_WITH_SHA_256, &None, true)
    {
        return Err(IdentityRefusal::Algorithm);
    }
    let signed = trusted
        .iter()
        .any(|ca| tbs.issuer() == ca.tbs_certificate().subject() && signed_by(&leaf, ca));
    if !signed {
        return Err(IdentityRefusal::NotSignedByTrustedCa);
    }
    check_extended_key_usage(&leaf)?;
    check_usage(&leaf).map_err(|refusal| IdentityRefusal::Profile(refusal.to_string()))?;
    check_names(&leaf, expected)?;
    if tbs.subject_public_key_info().to_der().ok().as_deref() != Some(expected.key.spki_der()) {
        return Err(IdentityRefusal::OtherKey);
    }
    let (not_before, not_after) = window(&leaf);
    if !validity::service_valid_now(wall, not_before, not_after) {
        return Err(IdentityRefusal::NotValidNow {
            not_before,
            not_after,
        });
    }
    let der = leaf.to_der().map_err(not_a_certificate)?;
    let pem = encode_string("CERTIFICATE", LineEnding::LF, &der);
    let pem = pem.map_err(not_a_certificate)?;
    Ok(Checked {
        pem,
        not_before,
        not_after,
    })
}

/// The extended key usage is present and is `clientAuth` and nothing else.
fn check_extended_key_usage(leaf: &Certificate) -> Result<(), IdentityRefusal> {
    let usage = leaf
        .tbs_certificate()
        .get_extension::<ExtendedKeyUsage>()
        .map_err(|_| IdentityRefusal::ExtendedKeyUsage)?;
    match usage {
        Some((_, usage)) if usage.0 == [ID_KP_CLIENT_AUTH] => Ok(()),
        _ => Err(IdentityRefusal::ExtendedKeyUsage),
    }
}

/// The subject and the subject alternative name name this service on this host.
fn check_names(leaf: &Certificate, expected: &Expected<'_>) -> Result<(), IdentityRefusal> {
    let subject = format!("{COMMON_NAME_PREFIX}{}", expected.service);
    if common_name(leaf).as_deref() != Some(subject.as_str()) {
        return Err(IdentityRefusal::Subject(subject));
    }
    let uri = format!("{URI_PREFIX}{}", expected.service);
    let dns = expected.host.to_string();
    let names = leaf
        .tbs_certificate()
        .get_extension::<SubjectAltName>()
        .ok()
        .flatten()
        .map(|(_, names)| names.0)
        .unwrap_or_default();
    let count = |is: &dyn Fn(&GeneralName) -> bool| names.iter().filter(|name| is(name)).count();
    let uris = count(
        &|name| matches!(name, GeneralName::UniformResourceIdentifier(text) if text.as_str() == uri),
    );
    let dnses = count(&|name| matches!(name, GeneralName::DnsName(text) if text.as_str() == dns));
    if (names.len(), uris, dnses) != (2, 1, 1) {
        return Err(IdentityRefusal::AlternativeNames { uri, dns });
    }
    Ok(())
}

/// The key of the PKCS#8 PEM of a `key.pem`.
pub(crate) fn key_from_pem(pem: &[u8]) -> Result<HostKey, String> {
    let (label, der) = x509_cert::der::pem::decode_vec(pem)
        .map_err(|e| format!("the key file is not PEM: {e}"))?;
    if label != "PRIVATE KEY" {
        return Err(format!("the key file is a {label:?}, not a PRIVATE KEY"));
    }
    HostKey::from_pkcs8(&der).map_err(|e| e.to_string())
}

/// `key` as the PKCS#8 PEM of `key.pem`.
pub(crate) fn key_to_pem(key: &HostKey) -> Result<String, String> {
    let der = key.pkcs8_der().map_err(|e| e.to_string())?;
    encode_string("PRIVATE KEY", LineEnding::LF, &der).map_err(|e| e.to_string())
}
