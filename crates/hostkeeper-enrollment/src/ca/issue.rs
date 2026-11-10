//! Issuance: the certificates the CA signs, each under its profile (design 4.4).
//!
//! | | host client certificate | coordinator server certificate |
//! |---|---|---|
//! | subject | `CN=host:<id>`, the approved id | `CN=hostkeeper coordinator` |
//! | subjectAltName | none | the DNS names and IP addresses asked for |
//! | extended key usage | `clientAuth` only | `serverAuth` only |
//! | key usage | `digitalSignature` | `digitalSignature` |
//! | basic constraints | `CA:FALSE` | `CA:FALSE` |
//! | validity | from now, at most [`HOST_VALIDITY_SECS`] (7 days) | from now, at most [`SERVER_VALIDITY_CAP_SECS`] (90 days) |
//!
//! "Now" is the CA's [`UnixClock`], read once per certificate. A validity above the cap is
//! refused (`validity_over_cap`), never clamped; the cap is held here, not by the caller.
//! The serial number is the caller's: the store hands out serials from a counter it persists
//! ([`crate::enroll::EnrollmentStore::allocate_serial`]); 1 is the CA certificate's own and is
//! refused, as is 0.

use std::net::IpAddr;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PublicKeyData, SanType, SerialNumber, SignatureAlgorithm,
};

use crate::ca::bundle::CaBundle;
use crate::ca::profile::P256Key;
use crate::names::HostId;
use crate::validity::{UnixClock, Window};

/// The validity of a host's client certificate unless the store is given another
/// ([`crate::enroll::EnrollmentStore::with_host_validity_secs`]), and its cap: 7 days.
pub const HOST_VALIDITY_SECS: u64 = 7 * 24 * 60 * 60;

/// The longest validity of the coordinator's server certificate: 90 days. The coordinator asks
/// for a new one before it ends.
pub const SERVER_VALIDITY_CAP_SECS: u64 = 90 * 24 * 60 * 60;

/// The validity of the CA certificate itself: 20 years of 365 days. Rotating the CA key is
/// not built.
pub const CA_VALIDITY_SECS: u64 = 20 * 365 * 24 * 60 * 60;

/// The CA certificate's serial number. Every other certificate's is above it.
pub const CA_SERIAL: u64 = 1;

/// The common name of the CA certificate.
pub const CA_COMMON_NAME: &str = "hostkeeper CA";

/// The common name of the coordinator's server certificate.
pub const SERVER_COMMON_NAME: &str = "hostkeeper coordinator";

/// What a certificate is for.
#[derive(Clone, Copy, Debug)]
pub enum Subject<'a> {
    /// A host's client certificate, for the approved host id.
    Host(&'a HostId),
    /// The coordinator's server certificate, for these names.
    Server(ServerNames<'a>),
}

/// The names a coordinator's server certificate is for: at least one DNS name, and the IP
/// addresses a daemon dials it by (a network-installed host dials the LAN listener's address).
/// The certificate names the DNS names first, then the addresses, each in the order given.
#[derive(Clone, Copy, Debug)]
pub struct ServerNames<'a> {
    /// The DNS names.
    pub dns: &'a [String],
    /// The IP addresses.
    pub ips: &'a [IpAddr],
}

/// One certificate to sign. The key has met the profile ([`P256Key`]).
#[derive(Clone, Copy, Debug)]
pub struct SignRequest<'a> {
    pub subject: Subject<'a>,
    pub key: &'a P256Key,
    pub validity_secs: u64,
    pub serial: u64,
}

/// A signed certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    /// The certificate, DER.
    pub der: Vec<u8>,
    pub serial: u64,
    /// Unix seconds, both ends included.
    pub not_before: u64,
    pub not_after: u64,
}

/// Why the CA did not sign.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    /// A validity above the cap of its kind of certificate.
    #[error("validity_over_cap: {requested} seconds asked for, at most {cap} allowed")]
    ValidityOverCap { requested: u64, cap: u64 },
    /// A validity of 0 seconds.
    #[error("a certificate needs a validity of at least one second")]
    EmptyValidity,
    /// The clock reads a time whose window a certificate cannot hold.
    #[error("the clock reads {now}, whose validity window does not fit a certificate")]
    ClockOutOfRange { now: u64 },
    /// A serial number of 0, or the CA certificate's own.
    #[error("serial {0} is not one a certificate of this CA may carry")]
    BadSerial(u64),
    /// A server certificate with no DNS name.
    #[error("a server certificate needs at least one DNS name")]
    NoDnsName,
    /// A DNS name that is not dot-separated labels of the host-id grammar, 253 bytes at most.
    #[error("{0:?} is not a DNS name the CA signs")]
    BadDnsName(String),
    /// The signing library failed.
    #[error("signing failed: {0}")]
    Signing(String),
}

impl SignError {
    /// The refusal token, for the one refusal that has its own.
    pub fn token(&self) -> Option<&'static str> {
        matches!(self, SignError::ValidityOverCap { .. }).then_some("validity_over_cap")
    }
}

/// Signs a profile-checked request and returns the certificate. [`Ca`] is the in-process
/// implementation; a signer in a separate process would be another.
pub trait Signer: Send + Sync {
    /// Signs `request`.
    ///
    /// # Errors
    /// [`SignError`]: the request breaks a limit of the profile, or signing failed.
    fn sign(&self, request: &SignRequest<'_>) -> Result<Certificate, SignError>;

    /// The CA certificate and its SHA-256.
    fn bundle(&self) -> &CaBundle;
}

/// The CA: its key, its certificate, and the clock its certificates start from.
pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
    bundle: CaBundle,
    clock: Arc<dyn UnixClock>,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ca({})", self.bundle.sha256_hex())
    }
}

impl Ca {
    /// The CA of `key` and its certificate `certificate_der`, which the caller has checked
    /// belong together.
    pub(crate) fn from_parts(
        key: KeyPair,
        certificate_der: Vec<u8>,
        clock: Arc<dyn UnixClock>,
    ) -> Ca {
        Ca {
            issuer: Issuer::new(ca_params(), key),
            bundle: CaBundle::of(certificate_der),
            clock,
        }
    }

    /// A host's client certificate for `host_id`, valid `validity_secs` from now (at most
    /// [`HOST_VALIDITY_SECS`]).
    ///
    /// # Errors
    /// [`SignError`], as [`Signer::sign`].
    pub fn issue_host(
        &self,
        host_id: &HostId,
        key: &P256Key,
        validity_secs: u64,
        serial: u64,
    ) -> Result<Certificate, SignError> {
        self.sign(&SignRequest {
            subject: Subject::Host(host_id),
            key,
            validity_secs,
            serial,
        })
    }

    /// The coordinator's server certificate for `names`, valid `validity_secs` from now (at
    /// most [`SERVER_VALIDITY_CAP_SECS`]).
    ///
    /// # Errors
    /// [`SignError`], as [`Signer::sign`].
    pub fn issue_server(
        &self,
        key: &P256Key,
        names: ServerNames<'_>,
        validity_secs: u64,
        serial: u64,
    ) -> Result<Certificate, SignError> {
        self.sign(&SignRequest {
            subject: Subject::Server(names),
            key,
            validity_secs,
            serial,
        })
    }
}

impl Signer for Ca {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Certificate, SignError> {
        let (cap, mut params) = match request.subject {
            Subject::Host(host_id) => (HOST_VALIDITY_SECS, host_params(host_id)),
            Subject::Server(names) => (SERVER_VALIDITY_CAP_SECS, server_params(names)?),
        };
        let requested = request.validity_secs;
        if requested == 0 {
            return Err(SignError::EmptyValidity);
        }
        if requested > cap {
            return Err(SignError::ValidityOverCap { requested, cap });
        }
        if request.serial <= CA_SERIAL {
            return Err(SignError::BadSerial(request.serial));
        }
        let now = self.clock.now_unix();
        let window = Window::from_now(now, requested).ok_or(SignError::ClockOutOfRange { now })?;
        (params.not_before, params.not_after) = window.times();
        params.serial_number = Some(serial_number(request.serial));
        params.use_authority_key_identifier_extension = true;
        let der = params
            .signed_by(request.key, &self.issuer)
            .map_err(|e| SignError::Signing(e.to_string()))?
            .der()
            .to_vec();
        Ok(Certificate {
            der,
            serial: request.serial,
            not_before: window.not_before,
            not_after: window.not_after,
        })
    }

    fn bundle(&self) -> &CaBundle {
        &self.bundle
    }
}

impl PublicKeyData for P256Key {
    fn der_bytes(&self) -> &[u8] {
        self.point()
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

/// A distinguished name of one common name.
fn common_name(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    dn
}

/// The serial as the shortest big-endian bytes (at least one).
pub(crate) fn serial_number(serial: u64) -> SerialNumber {
    let bytes = serial.to_be_bytes();
    let first = bytes
        .iter()
        .position(|b| *b != 0)
        .unwrap_or(bytes.len() - 1);
    SerialNumber::from_slice(&bytes[first..])
}

/// The CA certificate's parameters, without its validity and serial. The issuer of every
/// certificate is built from the same parameters, so its name matches the CA certificate's
/// subject byte for byte.
pub(crate) fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(CA_COMMON_NAME);
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

/// A host's client certificate: nothing of the request is copied but its key.
fn host_params(host_id: &HostId) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(&format!("host:{host_id}"));
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params
}

/// The coordinator's server certificate: the DNS names, then the IP addresses.
fn server_params(names: ServerNames<'_>) -> Result<CertificateParams, SignError> {
    if names.dns.is_empty() {
        return Err(SignError::NoDnsName);
    }
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(SERVER_COMMON_NAME);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.subject_alt_names = names
        .dns
        .iter()
        .map(String::as_str)
        .map(dns_name)
        .collect::<Result<_, _>>()?;
    params
        .subject_alt_names
        .extend(names.ips.iter().copied().map(SanType::IpAddress));
    Ok(params)
}

/// `name` as a subjectAltName, when it is at most 253 bytes of dot-separated labels of the
/// host-id grammar.
fn dns_name(name: &str) -> Result<SanType, SignError> {
    let labels_ok = name.len() <= 253 && name.split('.').all(|label| HostId::parse(label).is_ok());
    rcgen::string::Ia5String::try_from(name)
        .ok()
        .filter(|_| labels_ok)
        .map(SanType::DnsName)
        .ok_or_else(|| SignError::BadDnsName(name.to_owned()))
}
