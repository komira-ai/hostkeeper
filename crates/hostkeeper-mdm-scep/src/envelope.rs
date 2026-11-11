//! The CMS `EnvelopedData` of a request: RSA key transport and a CBC content cipher.
use aes::{Aes128, Aes192, Aes256};
use cipher::block_padding::Pkcs7;
use cipher::{BlockModeDecrypt, KeyIvInit};
use cms::enveloped_data::{EnvelopedData, RecipientInfo};
use der::asn1::OctetString;
use der::{Any, Decode};
use rsa::rand_core::CryptoRng;
use rsa::{Pkcs1v15Encrypt, PrivateKey};

use crate::asn1::Bad;
use crate::oids;

/// The content cipher a request used; the reply uses AES of the same size, or AES-128 after DES3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cipher {
    Des3,
    Aes128,
    Aes192,
    Aes256,
}

/// The plaintext of the `EnvelopedData` in `der` (a `ContentInfo`), with the cipher it used.
///
/// The error says what failed. `Err(Err(_))` is an algorithm this server does not implement,
/// `Err(Ok(_))` an envelope that cannot be opened.
pub(crate) fn open(
    der: &[u8],
    key: &PrivateKey,
    rng: &mut impl CryptoRng,
) -> Result<(Vec<u8>, Cipher), Opened> {
    let info = cms::content_info::ContentInfo::from_der(der)
        .ok()
        .ok_or(Opened::Bad(Bad("envelope")))?;
    let inner = der::Encode::to_der(&info.content)
        .ok()
        .ok_or(Opened::Bad(Bad("envelope")))?;
    let env = EnvelopedData::from_der(&inner)
        .ok()
        .ok_or(Opened::Bad(Bad("envelope")))?;
    let Some(RecipientInfo::Ktri(ktri)) = env.recip_infos.0.iter().next() else {
        return Err(Opened::Alg("a recipient that is not RSA key transport"));
    };
    if ktri.key_enc_alg.oid != oids::RSA_ENCRYPTION {
        return Err(Opened::Alg("a key transport that is not PKCS#1 v1.5"));
    }
    let alg = &env.encrypted_content.content_enc_alg;
    let cipher = match alg.oid {
        oids::AES128_CBC => Cipher::Aes128,
        oids::AES192_CBC => Cipher::Aes192,
        oids::AES256_CBC => Cipher::Aes256,
        oids::DES_EDE3_CBC => Cipher::Des3,
        _ => return Err(Opened::Alg("a content cipher that is not AES-CBC or 3DES")),
    };
    let cek = key
        .decrypt_blinded(rng, Pkcs1v15Encrypt, ktri.enc_key.as_bytes())
        .ok()
        .ok_or(Opened::Bad(Bad("content key")))?;
    let iv: OctetString = alg
        .parameters
        .clone()
        .ok_or(Opened::Bad(Bad("content cipher parameters")))
        .and_then(|p: Any| {
            p.decode_as()
                .ok()
                .ok_or(Opened::Bad(Bad("content cipher parameters")))
        })?;
    let ciphertext = env
        .encrypted_content
        .encrypted_content
        .ok_or(Opened::Bad(Bad("no encrypted content")))?;
    let plain = decrypt(cipher, &cek, iv.as_bytes(), ciphertext.as_bytes())
        .ok_or(Opened::Bad(Bad("content")))?;
    Ok((plain, cipher))
}

/// Why an envelope was not opened.
#[derive(Debug)]
pub(crate) enum Opened {
    /// An algorithm this server does not implement.
    Alg(&'static str),
    /// Anything else.
    Bad(Bad),
}

fn decrypt(cipher: Cipher, key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    match cipher {
        Cipher::Aes128 => cbc::Decryptor::<Aes128>::new_from_slices(key, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(data)
            .ok(),
        Cipher::Aes192 => cbc::Decryptor::<Aes192>::new_from_slices(key, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(data)
            .ok(),
        Cipher::Aes256 => cbc::Decryptor::<Aes256>::new_from_slices(key, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(data)
            .ok(),
        Cipher::Des3 => cbc::Decryptor::<des::TdesEde3>::new_from_slices(key, iv)
            .ok()?
            .decrypt_padded_vec::<Pkcs7>(data)
            .ok(),
    }
}
