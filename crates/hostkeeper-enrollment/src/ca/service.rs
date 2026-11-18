//! A service certificate as a relying party reads it back: [`CaBundle::service_certificate`].
//!
//! A certificate is a service certificate of this CA when it is signed under the CA's key and has
//! exactly the profile [`Ca::issue_service`](crate::ca::Ca::issue_service) writes: one common
//! name `service:<name>`, the URI subjectAltName `urn:hostkeeper:service:<name>` of the same
//! name, one DNS subjectAltName that is a host id and no other name, an extended key usage of
//! `clientAuth` alone, not a CA, and a P-256 key. It is never a host certificate:
//! [`CaBundle::host_certificate`] reads only the subject `host:<id>`, and this reads only
//! `service:<name>`. Its validity is returned, not judged: the caller holds the clock.

use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::bundle::CaBundle;
use crate::ca::issue::{SERVICE_COMMON_NAME_PREFIX, SERVICE_URI_PREFIX};
use crate::ca::profile::P256Key;
use crate::names::{BadName, HostId, KeyHash, ServiceName};

/// A service's client certificate this CA signed, as [`CaBundle::service_certificate`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceCertificate {
    /// The service its subject names (`service:<name>`).
    pub service: ServiceName,
    /// The host its DNS name names.
    pub host_id: HostId,
    /// The SHA-256 of the SubjectPublicKeyInfo of the key it certifies.
    pub key_hash: KeyHash,
    /// The first second of validity, unix seconds.
    pub not_before_unix: u64,
    /// The last second of validity, unix seconds.
    pub not_after_unix: u64,
}

/// Why a certificate is not a service certificate of this CA.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceCertificateError {
    /// Not one DER certificate, or bytes after it.
    #[error("the certificate is not one DER certificate")]
    NotDer,
    /// The CA's key is not a P-256 key, or the signature does not verify under it.
    #[error("the certificate is not signed by this CA")]
    NotSignedByThisCa,
    /// The subject is not exactly one common name `service:<name>`.
    #[error("the subject is not one common name service:<name>")]
    Subject,
    /// The name in the subject is not a service name.
    #[error("the service in the subject is not a service name: {0}")]
    ServiceName(BadName),
    /// The subjectAltName extension is absent or unreadable, or has a name that is not the
    /// service's URI or a host id's DNS name, or has either twice or not at all.
    #[error(
        "the subjectAltName is not exactly the URI urn:hostkeeper:service:<name> and one DNS host id"
    )]
    AltNames,
    /// The extended key usage is absent or unreadable, or is more than `clientAuth`.
    #[error("the extended key usage is not clientAuth alone")]
    ExtendedKeyUsage,
    /// The basic constraints say the certificate is a CA.
    #[error("the certificate is a CA")]
    IsCa,
    /// The key is not an uncompressed P-256 key.
    #[error("the key is not an uncompressed ECDSA P-256 key")]
    Key,
    /// The validity ends are not unix seconds from the epoch on.
    #[error("the validity is not a window of unix seconds")]
    Validity,
}

impl CaBundle {
    /// The service, host, key and validity of `certificate_der`, when it is a service
    /// certificate this CA signed. The signature is checked under the CA's key; the time is not
    /// looked at.
    ///
    /// # Errors
    /// [`ServiceCertificateError`]: the first rule it breaks, in the order the enum lists them.
    pub fn service_certificate(
        &self,
        certificate_der: &[u8],
    ) -> Result<ServiceCertificate, ServiceCertificateError> {
        let certificate = X509Certificate::from_der(certificate_der)
            .ok()
            .filter(|(rest, _)| rest.is_empty())
            .map(|(_, certificate)| certificate)
            .ok_or(ServiceCertificateError::NotDer)?;
        let signed = self.key().is_some_and(|ca| {
            ca.verify(
                certificate.tbs_certificate.as_ref(),
                &certificate.signature_value.data,
            )
        });
        if !signed {
            return Err(ServiceCertificateError::NotSignedByThisCa);
        }
        let service = service_of(&certificate)?;
        let host_id = host_of(&certificate, &service)?;
        client_auth_only(&certificate)?;
        if certificate
            .basic_constraints()
            .ok()
            .flatten()
            .is_some_and(|constraints| constraints.value.ca)
        {
            return Err(ServiceCertificateError::IsCa);
        }
        let key = P256Key::from_spki(certificate.public_key().raw)
            .map_err(|_| ServiceCertificateError::Key)?;
        let validity = certificate.validity();
        let (Ok(not_before_unix), Ok(not_after_unix)) = (
            u64::try_from(validity.not_before.timestamp()),
            u64::try_from(validity.not_after.timestamp()),
        ) else {
            return Err(ServiceCertificateError::Validity);
        };
        Ok(ServiceCertificate {
            service,
            host_id,
            key_hash: key.key_hash(),
            not_before_unix,
            not_after_unix,
        })
    }
}

/// The service of a subject that is exactly one common name `service:<name>`.
fn service_of(certificate: &X509Certificate<'_>) -> Result<ServiceName, ServiceCertificateError> {
    let names: Vec<_> = certificate.subject().iter_attributes().collect();
    let [only] = names.as_slice() else {
        return Err(ServiceCertificateError::Subject);
    };
    let name = only
        .as_str()
        .ok()
        .filter(|_| *only.attr_type() == x509_parser::oid_registry::OID_X509_COMMON_NAME)
        .and_then(|text| text.strip_prefix(SERVICE_COMMON_NAME_PREFIX))
        .ok_or(ServiceCertificateError::Subject)?;
    ServiceName::parse(name).map_err(ServiceCertificateError::ServiceName)
}

/// The host of the one DNS name of the alternative names, which are that and the URI of
/// `service` and nothing else.
fn host_of(
    certificate: &X509Certificate<'_>,
    service: &ServiceName,
) -> Result<HostId, ServiceCertificateError> {
    let uri = format!("{SERVICE_URI_PREFIX}{}", service.as_str());
    let alternatives = certificate
        .subject_alternative_name()
        .ok()
        .flatten()
        .ok_or(ServiceCertificateError::AltNames)?;
    let mut uris = 0;
    let mut hosts = Vec::new();
    for name in &alternatives.value.general_names {
        match name {
            GeneralName::URI(text) if *text == uri => uris += 1,
            GeneralName::DNSName(text) => hosts.push(*text),
            _ => return Err(ServiceCertificateError::AltNames),
        }
    }
    match (uris, hosts.as_slice()) {
        (1, [host]) => HostId::parse(host).map_err(|_| ServiceCertificateError::AltNames),
        _ => Err(ServiceCertificateError::AltNames),
    }
}

/// Whether the extended key usage is `clientAuth` and nothing else.
fn client_auth_only(certificate: &X509Certificate<'_>) -> Result<(), ServiceCertificateError> {
    let usage = certificate
        .extended_key_usage()
        .ok()
        .flatten()
        .ok_or(ServiceCertificateError::ExtendedKeyUsage)?
        .value;
    let only = usage.client_auth
        && !(usage.any
            || usage.server_auth
            || usage.code_signing
            || usage.email_protection
            || usage.time_stamping
            || usage.ocsp_signing
            || !usage.other.is_empty());
    if only {
        Ok(())
    } else {
        Err(ServiceCertificateError::ExtendedKeyUsage)
    }
}
