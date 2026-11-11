//! The MDM identity CA: its own RSA key (never the host CA's, design 4.2) and the certificates it
//! issues, one per Mac serial.
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use const_oid::db::rfc5280::ID_KP_CLIENT_AUTH;
use der::{Decode, Encode};
use rsa::PrivateKey;
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::rand_core::CryptoRng;
use sha2::Sha256;
use spki::SubjectPublicKeyInfoOwned;
use x509_cert::Certificate;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::certificate::TbsCertificate;
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages};
use x509_cert::ext::{Extension, ToExtension};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::{Time, Validity};

use crate::Error;

const SECONDS_PER_DAY: u64 = 86_400;
const MAX_SERIAL: usize = 64;

/// What the CA issues under; every number is a setting with its default (design 4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The `O=` of every identity's subject.
    pub org: String,
    /// How long an identity is valid (`identity_days`).
    pub identity_days: u32,
    /// How far before now the validity starts, for a Mac whose clock is behind.
    pub backdate_s: u64,
    /// The shortest RSA key a request may present.
    pub min_key_bits: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            org: "hostkeeper".to_owned(),
            identity_days: 1095,
            backdate_s: 3600,
            min_key_bits: 2048,
        }
    }
}

/// The CA: certificate, private key and issuing policy.
pub struct Ca {
    cert: Certificate,
    cert_der: Vec<u8>,
    key: PrivateKey,
    policy: Policy,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ca")
            .field("subject", self.cert.tbs_certificate().subject())
            .field("key", &"<private>")
            .finish()
    }
}

/// Which certificate a profile builds.
enum Kind {
    Root,
    Identity,
}

struct Profile {
    kind: Kind,
    issuer: Name,
    subject: Name,
}

impl BuilderProfile for Profile {
    fn get_issuer(&self, _subject: &Name) -> Name {
        self.issuer.clone()
    }

    fn get_subject(&self) -> Name {
        self.subject.clone()
    }

    fn build_extensions(
        &self,
        _spk: spki::SubjectPublicKeyInfoRef<'_>,
        _issuer_spk: spki::SubjectPublicKeyInfoRef<'_>,
        _tbs: &TbsCertificate,
    ) -> x509_cert::builder::Result<Vec<Extension>> {
        let subject = &self.subject;
        Ok(match self.kind {
            Kind::Root => vec![
                BasicConstraints {
                    ca: true,
                    path_len_constraint: Some(0),
                }
                .to_extension(subject, &[])?,
                KeyUsage(KeyUsages::KeyCertSign | KeyUsages::CRLSign).to_extension(subject, &[])?,
            ],
            Kind::Identity => vec![
                BasicConstraints {
                    ca: false,
                    path_len_constraint: None,
                }
                .to_extension(subject, &[])?,
                KeyUsage(KeyUsages::DigitalSignature | KeyUsages::KeyEncipherment)
                    .to_extension(subject, &[])?,
                ExtendedKeyUsage(vec![ID_KP_CLIENT_AUTH]).to_extension(subject, &[])?,
            ],
        })
    }
}

fn time(at: Option<SystemTime>) -> Result<Time, Error> {
    at.and_then(|at| Time::try_from(at).ok())
        .ok_or(Error::Build("a validity time"))
}

/// The validity from `backdate_s` before `now` for `days`; a value no time can hold is an error,
/// not a panic, since both come from settings.
fn validity(now: SystemTime, backdate_s: u64, days: u32) -> Result<Validity, Error> {
    Ok(Validity::new(
        time(now.checked_sub(Duration::from_secs(backdate_s)))?,
        time(now.checked_add(Duration::from_secs(u64::from(days) * SECONDS_PER_DAY)))?,
    ))
}

impl Ca {
    /// A CA from its certificate (DER) and its key (PKCS#8, DER), as stored.
    pub fn from_der(cert_der: &[u8], key_pkcs8_der: &[u8], policy: Policy) -> Result<Self, Error> {
        let cert = Certificate::from_der(cert_der)
            .ok()
            .ok_or(Error::CaMaterial("certificate"))?;
        let key = PrivateKey::from_pkcs8_der(key_pkcs8_der)
            .ok()
            .ok_or(Error::CaMaterial("key"))?;
        Ok(Self {
            cert,
            cert_der: cert_der.to_vec(),
            key,
            policy,
        })
    }

    /// A new CA: an RSA key of `bits` and a self-signed certificate named `subject` (an RFC 4514
    /// string such as `CN=hostkeeper MDM CA,O=Komira`), valid for `days` from `now`.
    pub fn generate(
        rng: &mut impl CryptoRng,
        bits: usize,
        subject: &str,
        now: SystemTime,
        days: u32,
        policy: Policy,
    ) -> Result<Self, Error> {
        let key = PrivateKey::new(rng, bits)
            .ok()
            .ok_or(Error::CaMaterial("key size"))?;
        let name = Name::from_str(subject)
            .ok()
            .ok_or(Error::CaMaterial("subject"))?;
        let spki = spki_of(&key)?;
        let validity = validity(now, policy.backdate_s, days)?;
        let profile = Profile {
            kind: Kind::Root,
            issuer: name.clone(),
            subject: name,
        };
        let cert = sign(profile, validity, spki, &key, rng)?;
        let cert_der = cert
            .to_der()
            .ok()
            .ok_or(Error::Build("the CA certificate"))?;
        Ok(Self {
            cert,
            cert_der,
            key,
            policy,
        })
    }

    /// The DER of the CA certificate.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The private key as PKCS#8 DER, for the store to seal. The caller zeroizes what it holds.
    pub fn key_pkcs8_der(&self) -> Result<Vec<u8>, Error> {
        self.key
            .to_pkcs8_der()
            .map(|der| der.as_bytes().to_vec())
            .ok()
            .ok_or(Error::Build("the CA key"))
    }

    pub(crate) fn private_key(&self) -> &PrivateKey {
        &self.key
    }

    pub(crate) fn policy(&self) -> &Policy {
        &self.policy
    }

    pub(crate) fn certificate(&self) -> &Certificate {
        &self.cert
    }

    /// The identity of the Mac with `serial` for `spki`: subject `CN=<serial>,O=<org>,
    /// OU=hostkeeper-mdm`, signed by this CA, as DER.
    pub(crate) fn issue(
        &self,
        serial: &str,
        spki: &SubjectPublicKeyInfoOwned,
        now: SystemTime,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, Error> {
        if serial.is_empty()
            || serial.len() > MAX_SERIAL
            || !serial.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(Error::BadSerial { len: serial.len() });
        }
        let subject = Name::from_str(&format!(
            "CN={serial},O={},OU=hostkeeper-mdm",
            escape(&self.policy.org)
        ))
        .ok()
        .ok_or(Error::Build("the subject"))?;
        let validity = validity(now, self.policy.backdate_s, self.policy.identity_days)?;
        let profile = Profile {
            kind: Kind::Identity,
            issuer: self.cert.tbs_certificate().subject().clone(),
            subject,
        };
        sign(profile, validity, spki.clone(), &self.key, rng)?
            .to_der()
            .ok()
            .ok_or(Error::Build("the identity"))
    }
}

/// An RFC 4514 attribute value with its special characters escaped.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn spki_of(key: &PrivateKey) -> Result<SubjectPublicKeyInfoOwned, Error> {
    use rsa::pkcs8::EncodePublicKey;
    let der = key
        .to_public_key()
        .to_public_key_der()
        .ok()
        .ok_or(Error::Build("the public key"))?;
    SubjectPublicKeyInfoOwned::from_der(der.as_bytes())
        .ok()
        .ok_or(Error::Build("the public key"))
}

fn sign(
    profile: Profile,
    validity: Validity,
    spki: SubjectPublicKeyInfoOwned,
    key: &PrivateKey,
    rng: &mut impl CryptoRng,
) -> Result<Certificate, Error> {
    let serial = SerialNumber::generate(rng);
    let signer = SigningKey::<Sha256>::new(key.clone());
    CertificateBuilder::new(profile, serial, validity, spki)
        .ok()
        .ok_or(Error::Build("the certificate"))?
        .build::<_, Signature>(&signer)
        .ok()
        .ok_or(Error::Build("the certificate"))
}
