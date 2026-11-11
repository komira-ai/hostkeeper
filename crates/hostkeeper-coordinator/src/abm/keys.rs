//! The RSA key pair of the ABM server entry (`docs/design/coordinator-mdm.md`, 4.5): the key the
//! `.p7m` of ABM's token is encrypted to, and the self-signed certificate of it that ABM takes as
//! the entry's public key.
//!
//! Takeover keeps NanoDEP's pair as it is: a PKCS#1 PEM key (`BEGIN RSA PRIVATE KEY`) and its
//! certificate, whose validity ABM does not look at (the one in use expired on 2026-11-07 and the
//! token still works). Only when that pair cannot be read out of NanoDEP does the coordinator make
//! a new one ([`generate`]) and the operator adds its certificate to ABM.

use rsa::pkcs1::{DecodeRsaPrivateKey as _, EncodeRsaPrivateKey as _};
use rsa::pkcs8::{DecodePrivateKey as _, DecodePublicKey as _, EncodePrivateKey as _, LineEnding};
use rsa::{PrivateKey, PublicKey};

/// The modulus of a key the coordinator makes. 2048 is what NanoDEP's pair has and Apple takes.
pub(crate) const KEY_BITS: usize = 2048;

/// Why a key or a certificate was refused. No text names a byte of the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum KeyError {
    /// The text is no RSA private key in PEM (PKCS#1 `RSA PRIVATE KEY`, or PKCS#8).
    #[error("not an RSA private key in PEM")]
    NotAKey,
    /// The text is no certificate in PEM.
    #[error("not a certificate in PEM")]
    NotACertificate,
    /// The certificate is not made for this key.
    #[error("the certificate is not the one of this key")]
    Mismatch,
    /// A key could not be made or written.
    #[error("the key could not be made")]
    Make,
}

/// The error of a step that could not make or write a key, whatever its own error.
fn make<E>(_: E) -> KeyError {
    KeyError::Make
}

/// The error of text that is no key.
fn not_a_key<E>(_: E) -> KeyError {
    KeyError::NotAKey
}

/// The error of text that is no certificate.
fn not_a_certificate<E>(_: E) -> KeyError {
    KeyError::NotACertificate
}

/// The error of a certificate whose key is not the one asked.
fn mismatch<E>(_: E) -> KeyError {
    KeyError::Mismatch
}

/// The RSA private key of `pem`.
///
/// # Errors
/// [`KeyError::NotAKey`].
pub(crate) fn parse_key(pem: &[u8]) -> Result<PrivateKey, KeyError> {
    let text = std::str::from_utf8(pem).map_err(not_a_key)?;
    PrivateKey::from_pkcs1_pem(text)
        .or_else(|_| PrivateKey::from_pkcs8_pem(text))
        .map_err(not_a_key)
}

/// A new key of `bits` bits.
///
/// # Errors
/// [`KeyError::Make`]: the system gave no random bytes, or `bits` is below 1024.
pub(crate) fn generate(bits: usize) -> Result<PrivateKey, KeyError> {
    PrivateKey::new(&mut getrandom::SysRng, bits).map_err(make)
}

/// `key` as a PKCS#1 PEM, the way Apple's tooling and NanoDEP write it.
///
/// # Errors
/// [`KeyError::Make`].
pub(crate) fn pkcs1_pem(key: &PrivateKey) -> Result<String, KeyError> {
    key.to_pkcs1_pem(LineEnding::LF)
        .map(|pem| pem.to_string())
        .map_err(make)
}

/// A self-signed certificate of `key`, in PEM, valid from 1 January of the year before `now_unix` to 1 January ten
/// years after its year: ABM takes the certificate as a carrier of the public key.
///
/// # Errors
/// [`KeyError::Make`].
pub(crate) fn self_signed(key: &PrivateKey, now_unix: u64) -> Result<String, KeyError> {
    let der = key.to_pkcs8_der().map_err(make)?;
    let pkcs8 = rustls::pki_types::PrivatePkcs8KeyDer::from(der.as_bytes());
    let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &rcgen::PKCS_RSA_SHA256)
        .map_err(make)?;
    let mut params = rcgen::CertificateParams::new(Vec::new()).map_err(make)?;
    let year = year_of(now_unix);
    params.not_before = rcgen::date_time_ymd(year - 1, 1, 1);
    params.not_after = rcgen::date_time_ymd(year + 10, 1, 1);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "hostkeeper-mdm-abm");
    let cert = params.self_signed(&pair).map_err(make)?;
    Ok(cert.pem())
}

/// The calendar year (UTC) of `unix` seconds.
pub(crate) fn year_of(unix: u64) -> i32 {
    // Howard Hinnant's civil-from-days, for the year only.
    let days = i64::try_from(unix / 86_400).unwrap_or(0) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400 + i64::from(month >= 10);
    i32::try_from(year).unwrap_or(1970)
}

/// Checks that `cert_pem` is a certificate and, when `key` is given, that it is made for it.
///
/// # Errors
/// [`KeyError::NotACertificate`]; [`KeyError::Mismatch`].
pub(crate) fn check_certificate(cert_pem: &[u8], key: Option<&PrivateKey>) -> Result<(), KeyError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem).map_err(not_a_certificate)?;
    let cert = pem.parse_x509().map_err(not_a_certificate)?;
    let Some(key) = key else { return Ok(()) };
    let public = PublicKey::from_public_key_der(cert.public_key().raw).map_err(mismatch)?;
    if public == key.to_public_key() {
        Ok(())
    } else {
        Err(KeyError::Mismatch)
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
