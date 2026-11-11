//! The `.p7m` ABM hands over for a server entry (`docs/design/coordinator-mdm.md`, 4.5): a CMS
//! enveloped-data message (RFC 5652), as DER or as S/MIME text (headers, then base64), encrypted
//! to the public key of the entry's certificate. Opened with the entry's private key it holds the
//! token as a JSON object of the four OAuth parts and `access_secret_expiry`, framed as text
//! (`-----BEGIN MDM SERVER TOKEN-----` on Apple's side); [`token_json`] takes the object out of
//! whatever frames it.
//!
//! This is the fallback path of the takeover, for a key the coordinator made. It rests on the CMS
//! and S/MIME standards and on a round trip with a synthetic token; no `.p7m` Apple made was
//! available to read (spike abm, "Failed / not done"). The key is transported with RSAES-PKCS1-v1_5
//! or RSAES-OAEP (SHA-1 or SHA-256), the content is AES-CBC (128, 192 or 256) or 3DES-CBC.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt as _, KeyIvInit as _};
use cms::content_info::ContentInfo;
use cms::der::Decode as _;
use cms::der::asn1::{ObjectIdentifier, OctetString};
use cms::enveloped_data::{EnvelopedData, RecipientInfo};
use rsa::{Oaep, Pkcs1v15Encrypt, PrivateKey};

/// `enveloped-data`.
const ENVELOPED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.3");
/// `rsaEncryption`: the key is transported with PKCS#1 v1.5 padding.
const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
/// `id-RSAES-OAEP`.
const RSAES_OAEP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.7");
const AES_128_CBC: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.2");
const AES_192_CBC: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.22");
const AES_256_CBC: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.42");
const DES_EDE3_CBC: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.3.7");

/// Why a `.p7m` did not give its token. No text names a byte of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum P7mError {
    /// It is not a CMS enveloped-data message.
    #[error("not a CMS enveloped-data message (.p7m)")]
    NotCms,
    /// The key of no recipient opens for the key given.
    #[error("the message is not encrypted to this key")]
    WrongKey,
    /// The message uses a cipher this release does not read.
    #[error("the message's cipher is not one this release reads")]
    Cipher,
    /// It opened, and holds no JSON object.
    #[error("the message holds no token")]
    NoToken,
}

/// The token JSON inside `p7m`, opened with `key`.
///
/// # Errors
/// [`P7mError`].
pub(crate) fn token_json(p7m: &[u8], key: &PrivateKey) -> Result<Vec<u8>, P7mError> {
    let der = der_of(p7m)?;
    let info = ContentInfo::from_der(&der).map_err(|_| P7mError::NotCms)?;
    if info.content_type != ENVELOPED_DATA {
        return Err(P7mError::NotCms);
    }
    let enveloped = info
        .content
        .decode_as::<EnvelopedData>()
        .map_err(|_| P7mError::NotCms)?;
    let encrypted = &enveloped.encrypted_content;
    let content = encrypted
        .encrypted_content
        .as_ref()
        .ok_or(P7mError::NotCms)?;
    let iv = encrypted
        .content_enc_alg
        .parameters
        .as_ref()
        .and_then(|any| any.decode_as::<OctetString>().ok())
        .ok_or(P7mError::NotCms)?;
    let cipher = encrypted.content_enc_alg.oid;
    if ![AES_128_CBC, AES_192_CBC, AES_256_CBC, DES_EDE3_CBC].contains(&cipher) {
        return Err(P7mError::Cipher);
    }
    for recipient in enveloped.recip_infos.0.iter() {
        let RecipientInfo::Ktri(transport) = recipient else {
            continue;
        };
        let Some(cek) = open_key(key, transport.key_enc_alg.oid, transport.enc_key.as_bytes())
        else {
            continue;
        };
        // A wrong key opens to bytes the cipher refuses (RSA PKCS#1 v1.5 answers implicitly), and
        // so is the end of this recipient, not of the message.
        if let Some(plain) = decrypt(cipher, &cek, iv.as_bytes(), content.as_bytes()) {
            return object_in(&plain).ok_or(P7mError::NoToken);
        }
    }
    Err(P7mError::WrongKey)
}

/// The DER of `bytes`: as it is, or the base64 of an S/MIME or PEM wrapper.
fn der_of(bytes: &[u8]) -> Result<Vec<u8>, P7mError> {
    if bytes.first() == Some(&0x30) {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| P7mError::NotCms)?;
    // S/MIME: headers, a blank line, the body. PEM armour lines are dropped.
    let body = match text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
    {
        Some((head, rest)) if head.contains(':') => rest,
        _ => text,
    };
    let joined: String = body
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .flat_map(|line| line.split_whitespace())
        .collect();
    STANDARD.decode(joined).map_err(|_| P7mError::NotCms)
}

/// The content-encryption key `encrypted` holds for `key`, by the algorithm `algorithm`.
fn open_key(key: &PrivateKey, algorithm: ObjectIdentifier, encrypted: &[u8]) -> Option<Vec<u8>> {
    if algorithm == RSA_ENCRYPTION {
        key.decrypt(Pkcs1v15Encrypt, encrypted).ok()
    } else if algorithm == RSAES_OAEP {
        key.decrypt(Oaep::<sha1::Sha1>::new(), encrypted)
            .or_else(|_| key.decrypt(Oaep::<sha2::Sha256>::new(), encrypted))
            .ok()
    } else {
        None
    }
}

/// `content` decrypted by `cipher` with `cek` and `iv`; none when the key or IV length does not
/// fit the cipher or the padding is wrong.
fn decrypt(cipher: ObjectIdentifier, cek: &[u8], iv: &[u8], content: &[u8]) -> Option<Vec<u8>> {
    match cipher {
        c if c == AES_128_CBC => cbc::Decryptor::<aes::Aes128>::new_from_slices(cek, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(content)
            .ok(),
        c if c == AES_192_CBC => cbc::Decryptor::<aes::Aes192>::new_from_slices(cek, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(content)
            .ok(),
        c if c == AES_256_CBC => cbc::Decryptor::<aes::Aes256>::new_from_slices(cek, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(content)
            .ok(),
        _ => cbc::Decryptor::<des::TdesEde3>::new_from_slices(cek, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(content)
            .ok(),
    }
}

/// The first JSON object in `text`, from its first `{` to its last `}`.
fn object_in(text: &[u8]) -> Option<Vec<u8>> {
    let from = text.iter().position(|b| *b == b'{')?;
    let to = text.iter().rposition(|b| *b == b'}')?;
    (from < to).then(|| text[from..=to].to_vec())
}

#[cfg(test)]
#[path = "p7m_tests.rs"]
mod tests;
