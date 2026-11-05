//! The CA certificate as the hosts see it.
//!
//! A daemon config pins the CA by the SHA-256 of its certificate's DER (`ca_sha256`, design
//! 4.1 and 5.7): [`CaBundle::sha256_hex`] is that value. A poll that finds the certificate made
//! returns it with the host id and the CA certificate ([`PollBundle`]). A certificate a caller
//! presents is read back as the host it names by [`CaBundle::host_certificate`].

use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::profile::{P256Key, subject_id};
use crate::names::HostId;

/// The CA certificate and its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaBundle {
    certificate_der: Vec<u8>,
    sha256: [u8; 32],
    /// The CA's public key, when the certificate is one of a P-256 key.
    key: Option<P256Key>,
}

/// A host's client certificate this CA signed, as [`CaBundle::host_certificate`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCertificate {
    /// The host its subject names (`host:<id>`).
    pub host_id: HostId,
    /// The key it certifies.
    pub key: P256Key,
}

impl CaBundle {
    /// The bundle of the CA certificate `certificate_der`.
    pub fn of(certificate_der: Vec<u8>) -> CaBundle {
        let digest = ring::digest::digest(&ring::digest::SHA256, &certificate_der);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(digest.as_ref());
        let key = X509Certificate::from_der(&certificate_der)
            .ok()
            .and_then(|(_, certificate)| P256Key::from_spki(certificate.public_key().raw).ok());
        CaBundle {
            certificate_der,
            sha256,
            key,
        }
    }

    /// The host and key of `certificate_der`, when it is a certificate this CA signed whose
    /// subject is exactly `host:<id>`: a host's client certificate (the coordinator's server
    /// certificate names no host). The signature is checked under the CA's key; its validity
    /// and usage are not looked at, since TLS checked them when the caller presented it. `None`
    /// for anything else, including a P-256 key it does not certify.
    pub fn host_certificate(&self, certificate_der: &[u8]) -> Option<HostCertificate> {
        let ca = self.key.as_ref()?;
        let (_, certificate) = X509Certificate::from_der(certificate_der).ok()?;
        let signed = ca.verify(
            certificate.tbs_certificate.as_ref(),
            &certificate.signature_value.data,
        );
        subject_id(certificate.subject())
            .filter(|_| signed)
            .zip(P256Key::from_spki(certificate.public_key().raw).ok())
            .map(|(host_id, key)| HostCertificate { host_id, key })
    }

    /// The CA certificate, DER.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    /// The SHA-256 of the CA certificate's DER.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// The SHA-256 as 64 lower-case hex digits: the `ca_sha256` of a daemon config.
    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

/// What a poll returns once the host's certificate is made: the certificate, the host id it
/// names, and the CA certificate to check the coordinator's with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollBundle {
    pub certificate_der: Vec<u8>,
    pub host_id: HostId,
    pub ca: CaBundle,
}
