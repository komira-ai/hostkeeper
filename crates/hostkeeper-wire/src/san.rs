//! What a certificate's subjectAltNames say: the URIs under a prefix, and the DNS and IP names.
//!
//! The certificate is not verified here: TLS has already verified it against the listener's
//! client CA. This module only reads the bytes. The role URIs and the common name are never
//! names; a DNS name that looks like a role is not one.

use std::net::IpAddr;

use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{GeneralName, ParsedExtension};

/// Why a certificate's names cannot be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SanError {
    /// The bytes are not an X.509 certificate.
    #[error("the client certificate does not parse: {0}")]
    NotACertificate(String),
}

/// What follows `prefix` in each of the certificate's URI subjectAltNames that starts with
/// it, in the order the certificate lists them (byte for byte: the prefix is not matched
/// ignoring case).
///
/// # Errors
/// [`SanError::NotACertificate`]: the bytes are not an X.509 certificate.
pub fn uri_values_of(der: &[u8], prefix: &str) -> Result<Vec<String>, SanError> {
    let cert = parse(der)?;
    Ok(alt_names(&cert)
        .filter_map(|name| match name {
            GeneralName::URI(uri) => uri.strip_prefix(prefix).map(str::to_owned),
            _ => None,
        })
        .collect())
}

fn parse(der: &[u8]) -> Result<X509Certificate<'_>, SanError> {
    x509_parser::parse_x509_certificate(der)
        .map(|(_, cert)| cert)
        .map_err(|e| SanError::NotACertificate(e.to_string()))
}

/// Every entry of the certificate's subjectAltName extensions.
fn alt_names<'c, 'd>(cert: &'c X509Certificate<'d>) -> impl Iterator<Item = &'c GeneralName<'d>> {
    cert.extensions()
        .iter()
        .filter_map(|e| match e.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(san) => Some(&san.general_names),
            _ => None,
        })
        .flatten()
}

/// The hosts a certificate speaks for: its DNS and IP subjectAltNames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanNames {
    dns: Vec<String>,
    ips: Vec<IpAddr>,
}

impl SanNames {
    /// The DNS names as written, in the order the certificate lists them.
    #[must_use]
    pub fn dns(&self) -> &[String] {
        &self.dns
    }

    /// The IP addresses, in the order the certificate lists them.
    #[must_use]
    pub fn ips(&self) -> &[IpAddr] {
        &self.ips
    }
}

/// The DNS and IP names a DER certificate carries in its subjectAltName extensions.
///
/// # Errors
/// [`SanError::NotACertificate`]: the bytes are not an X.509 certificate.
pub fn names_of(der: &[u8]) -> Result<SanNames, SanError> {
    let cert = parse(der)?;
    let mut names = SanNames::default();
    for name in alt_names(&cert) {
        match name {
            GeneralName::DNSName(dns) => names.dns.push((*dns).to_owned()),
            GeneralName::IPAddress(raw) => names.ips.extend(ip_of(raw)),
            _ => {}
        }
    }
    Ok(names)
}

/// The address an IP subjectAltName holds: four bytes (IPv4) or sixteen (IPv6).
fn ip_of(raw: &[u8]) -> Option<IpAddr> {
    match <[u8; 4]>::try_from(raw) {
        Ok(v4) => Some(IpAddr::from(v4)),
        Err(_) => <[u8; 16]>::try_from(raw).ok().map(IpAddr::from),
    }
}
