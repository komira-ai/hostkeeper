//! The reply of mdmcert.download: the request signed for the push certificate, emailed as an
//! attachment (`docs/design/coordinator-mdm.md`, section 4.4; micromdm's `mdmctl
//! mdmcert.download -decrypt`).
//!
//! The attachment is the hex text of a PKCS#7 (CMS) `EnvelopedData` encrypted to the encryption
//! certificate of the request: the content key is RSA-encrypted (PKCS#1 v1.5, `rsaEncryption`) to
//! that certificate's key, and the content is encrypted with it. The operator uploads the
//! attachment as the mail has it, hex text, or as raw bytes, and [`decode_reply`] takes either.
//!
//! Content ciphers read: AES-128, AES-192 and AES-256 in CBC, DES-EDE3-CBC and DES-CBC. Which one
//! the service uses is not known (the client names none, it hands the bytes to a PKCS#7 reader
//! that takes all of these), so a cipher outside the list is refused with its identifier in the
//! error, and AES-GCM and RSA-OAEP are not read.

use aes::{Aes128, Aes192, Aes256};
use cbc::Decryptor;
use cbc::cipher::block_padding::Pkcs7;
use des::{Des, TdesEde3};
use rand_core::OsRng;
use rsa::Pkcs1v15Encrypt;

use crate::ber::{self, Tlv};
use crate::identity::{self, IdentityError};

const ENVELOPED_DATA: &str = "1.2.840.113549.1.7.3";
const RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const AES_128_CBC: &str = "2.16.840.1.101.3.4.1.2";
const AES_192_CBC: &str = "2.16.840.1.101.3.4.1.22";
const AES_256_CBC: &str = "2.16.840.1.101.3.4.1.42";
const DES_EDE3_CBC: &str = "1.2.840.113549.3.7";
const DES_CBC: &str = "1.3.14.3.2.7";

/// Why a reply was not decrypted. None of these holds any of the bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplyError {
    /// The upload is not a PKCS#7 enveloped-data message.
    #[error("the reply is not a PKCS#7 enveloped message: {0}")]
    NotEnveloped(String),
    /// The decryption key is not readable.
    #[error("the encryption key is not readable: {0}")]
    Key(String),
    /// No recipient of the message is opened by the key.
    #[error("the reply is not encrypted to this request's key")]
    NotForThisKey,
    /// The key or the content is encrypted with something this reader does not take.
    #[error("the reply uses an algorithm that is not read: {0}")]
    Unsupported(String),
    /// The content did not decrypt to padded plaintext.
    #[error("the reply's content does not decrypt")]
    Content,
}

impl From<ber::BerError> for ReplyError {
    fn from(why: ber::BerError) -> Self {
        Self::NotEnveloped(why.to_string())
    }
}

impl From<IdentityError> for ReplyError {
    fn from(why: IdentityError) -> Self {
        Self::Key(why.to_string())
    }
}

/// The CMS bytes of an upload: hex text (as the email attachment is) is decoded, and anything else
/// is taken as the raw bytes.
#[must_use]
pub fn decode_reply(upload: &[u8]) -> Vec<u8> {
    let text: Vec<u8> = upload
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !text.is_empty()
        && text.iter().all(u8::is_ascii_hexdigit)
        && let Ok(raw) = hex::decode(&text)
    {
        return raw;
    }
    upload.to_vec()
}

/// Decrypts the reply `upload` (hex text or raw bytes) with the RSA key `key_pem`.
///
/// # Errors
/// [`ReplyError`].
pub fn decrypt(upload: &[u8], key_pem: &[u8]) -> Result<Vec<u8>, ReplyError> {
    let key = identity::read_key(key_pem)?;
    let key = identity::rsa_key(&key)?;
    let message = decode_reply(upload);
    let (info, _) = ber::read(&message)?;
    let parts = sequence(&info)?;
    let [kind, wrapped, ..] = parts.as_slice() else {
        return Err(not_enveloped("no content"));
    };
    if ber::oid(kind)? != ENVELOPED_DATA {
        return Err(not_enveloped("the content type is not enveloped data"));
    }
    if !wrapped.is_context(0) {
        return Err(not_enveloped("no content"));
    }
    let (enveloped, _) = ber::read(wrapped.content)?;
    let fields = sequence(&enveloped)?;
    let recipients = fields
        .iter()
        .find(|field| field.is_universal(ber::SET))
        .ok_or_else(|| not_enveloped("no recipients"))?;
    let encrypted = fields
        .iter()
        .skip_while(|field| !field.is_universal(ber::SET))
        .nth(1)
        .filter(|field| field.is_universal(ber::SEQUENCE))
        .ok_or_else(|| not_enveloped("no encrypted content"))?;

    let mut content_key = None;
    for recipient in ber::children(recipients.content)? {
        if !recipient.is_universal(ber::SEQUENCE) {
            continue;
        }
        let transport = sequence(&recipient)?;
        let [_, _, algorithm, wrapped_key, ..] = transport.as_slice() else {
            continue;
        };
        let algorithm_id = ber::oid(&sequence(algorithm)?[0])?;
        if algorithm_id != RSA_ENCRYPTION {
            return Err(ReplyError::Unsupported(algorithm_id));
        }
        let wrapped_key = ber::octets(wrapped_key)?;
        if let Ok(opened) = key.decrypt_blinded(&mut OsRng, Pkcs1v15Encrypt, &wrapped_key) {
            content_key = Some(opened);
            break;
        }
    }
    let content_key = content_key.ok_or(ReplyError::NotForThisKey)?;
    decrypt_content(encrypted, &content_key)
}

/// The children of the SEQUENCE `tlv`.
fn sequence<'a>(tlv: &Tlv<'a>) -> Result<Vec<Tlv<'a>>, ReplyError> {
    if !tlv.is_universal(ber::SEQUENCE) {
        return Err(not_enveloped("a sequence was expected"));
    }
    Ok(ber::children(tlv.content)?)
}

fn not_enveloped(why: &str) -> ReplyError {
    ReplyError::NotEnveloped(why.to_owned())
}

/// The plaintext of the `EncryptedContentInfo` `info` under `key`.
fn decrypt_content(info: &Tlv<'_>, key: &[u8]) -> Result<Vec<u8>, ReplyError> {
    let parts = sequence(info)?;
    let [_, algorithm, rest @ ..] = parts.as_slice() else {
        return Err(not_enveloped("no content algorithm"));
    };
    let ciphertext = rest
        .iter()
        .find(|part| part.is_context(0))
        .ok_or_else(|| not_enveloped("no encrypted content"))
        .and_then(|part| ber::octets(part).map_err(ReplyError::from))?;
    let algorithm = sequence(algorithm)?;
    let [id, params, ..] = algorithm.as_slice() else {
        return Err(not_enveloped("no content cipher parameters"));
    };
    let id = ber::oid(id)?;
    let iv = ber::octets(params)?;
    let plain = match id.as_str() {
        AES_128_CBC => cbc_open::<Decryptor<Aes128>>(key, &iv, &ciphertext),
        AES_192_CBC => cbc_open::<Decryptor<Aes192>>(key, &iv, &ciphertext),
        AES_256_CBC => cbc_open::<Decryptor<Aes256>>(key, &iv, &ciphertext),
        DES_EDE3_CBC => cbc_open::<Decryptor<TdesEde3>>(key, &iv, &ciphertext),
        DES_CBC => cbc_open::<Decryptor<Des>>(key, &iv, &ciphertext),
        _ => return Err(ReplyError::Unsupported(id)),
    };
    plain.ok_or(ReplyError::Content)
}

/// `ciphertext` decrypted by the CBC decryptor `D` under `key` and `iv`, with PKCS#7 padding off;
/// none when the key or iv do not suit the cipher or the padding is wrong.
fn cbc_open<D>(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>>
where
    D: cbc::cipher::BlockDecryptMut + cbc::cipher::KeyIvInit,
{
    D::new_from_slices(key, iv)
        .ok()?
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .ok()
}

#[cfg(test)]
#[path = "p7m_tests.rs"]
mod tests;
