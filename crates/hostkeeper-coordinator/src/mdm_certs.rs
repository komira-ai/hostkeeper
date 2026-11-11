//! What the MDM status reads of a certificate and its key (`docs/design/coordinator-mdm.md`,
//! section 4.9): the end of validity, the push topic, the DNS names, and whether a private key is
//! the certificate's.
//!
//! Only public facts leave this module: a key is read to compare its public half with the
//! certificate's and is dropped, and nothing here prints either.

use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use x509_parser::extensions::GeneralName;

/// The subject attribute an APNs certificate names its topic in: `UID`.
const UID_OID: &str = "0.9.2342.19200300.100.1.1";

/// What a certificate says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CertFacts {
    /// The end of validity, in unix seconds.
    pub(crate) not_after_unix: i64,
    /// The subject UID, which on a push certificate is the topic.
    pub(crate) topic: Option<String>,
    /// The DNS names of its subject alternative names.
    pub(crate) names: Vec<String>,
    /// Its SubjectPublicKeyInfo, DER.
    spki: Vec<u8>,
}

impl CertFacts {
    /// The facts of the first certificate in `pem` (a chain's first is the leaf), or none when
    /// it holds no certificate that reads.
    pub(crate) fn read(pem: &[u8]) -> Option<Self> {
        let (_, block) = x509_parser::pem::parse_x509_pem(pem).ok()?;
        let cert = block.parse_x509().ok()?;
        let topic = cert
            .subject()
            .iter_attributes()
            .find(|attribute| attribute.attr_type().to_id_string() == UID_OID)
            .and_then(|attribute| attribute.as_str().ok())
            .map(str::to_owned);
        let names = cert
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|san| {
                san.value
                    .general_names
                    .iter()
                    .filter_map(|name| match name {
                        GeneralName::DNSName(dns) => Some((*dns).to_owned()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            not_after_unix: cert.validity().not_after.timestamp(),
            topic,
            names,
            spki: cert.public_key().raw.to_vec(),
        })
    }

    /// Whether `key_pem` holds the private key of this certificate.
    pub(crate) fn is_key(&self, key_pem: &[u8]) -> bool {
        let Ok(der) = PrivateKeyDer::from_pem_slice(key_pem) else {
            return false;
        };
        let Ok(signer) = rustls::crypto::ring::sign::any_supported_type(&der) else {
            return false;
        };
        signer
            .public_key()
            .is_some_and(|public| public.as_ref() == self.spki.as_slice())
    }

    /// Whether the certificate names `name` by a subject alternative name, exactly or by a
    /// wildcard of its first label (`*.example.com` names `mdm.example.com`, not
    /// `a.mdm.example.com`), letters compared without case.
    pub(crate) fn names_host(&self, name: &str) -> bool {
        self.names.iter().any(|held| {
            held.eq_ignore_ascii_case(name)
                || held.strip_prefix("*.").is_some_and(|suffix| {
                    name.split_once('.')
                        .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(suffix))
                })
        })
    }
}

#[cfg(test)]
#[path = "mdm_certs_tests.rs"]
mod tests;
