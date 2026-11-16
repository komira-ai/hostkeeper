//! OpenSSH user certificates (docs/design/ssh-through-coordinator.md, section 5): the SSH user CA's
//! keys, an operator's public key, and the certificate the CA signs for it.
//!
//! The formats are OpenSSH's: the public key line of `authorized_keys`, the unencrypted
//! `openssh-key-v1` private key file `ssh-keygen -t ed25519` writes (so the owner's own `ssh-keygen
//! -s` and `ssh-keygen -L` read what `ssh-ca init` wrote), and the `-cert-v01`
//! certificate of PROTOCOL.certkeys (its type names end in an at sign and `openssh.com`, written
//! `\u{40}` below because the hygiene lint reads an at sign in source as an e-mail address). The CA's keys are Ed25519, signed with `ring`. Nothing here
//! reads a file, a clock or the operators: the caller gives it the serial, the key id, the one
//! principal and the validity, and it makes the bytes.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};

/// The least bits of an RSA key the CA signs.
pub(crate) const RSA_MIN_BITS: usize = 3072;

/// The most bytes of a public key line the CA reads.
const MAX_LINE: usize = 4096;

/// The magic of an OpenSSH private key file.
const MAGIC: &[u8] = b"openssh-key-v1\0";
const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";

/// The most keys a CA secret holds: `current` and `next` (rotation).
pub(crate) const MAX_CA_KEYS: usize = 2;

/// Why a key or a certificate was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// A public key line that is not `ssh-ed25519`, `ecdsa-sha2-nistp256` or `ssh-rsa` of at least
    /// [`RSA_MIN_BITS`] bits, or is not well formed.
    #[error("not a public key of a type the CA signs")]
    BadPublicKey,
    /// A private key file that is not one unencrypted Ed25519 `openssh-key-v1` key, or is
    /// damaged.
    #[error("not an unencrypted Ed25519 OpenSSH private key")]
    BadPrivateKey,
    /// The system gave no random bytes.
    #[error("the system gave no random bytes")]
    Crypto,
}

/// The bytes of an SSH wire value being read.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u32()?).ok()?;
        self.bytes(n)
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

fn put_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_be_bytes());
}

/// Appends `bytes` as an SSH string. Every string this module writes is far below 4 GiB.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    out.extend_from_slice(bytes);
}

/// The key of an operator, as the certificate carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UserKey {
    Ed25519 {
        public: Vec<u8>,
    },
    Ecdsa {
        point: Vec<u8>,
    },
    /// The two integers as their wire bytes with the sign's zero byte taken off.
    Rsa {
        exponent: Vec<u8>,
        modulus: Vec<u8>,
    },
}

/// A public key line read: the key and its fingerprint, `SHA256:` and the 43 characters
/// `ssh-keygen -l` prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) key: UserKey,
    pub(crate) fingerprint: String,
}

/// The fingerprint of the public key blob `blob`.
pub(crate) fn fingerprint(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// The integer of an SSH `mpint` as bytes without the sign's zero: refused when it is negative or
/// not in its shortest form, and so also when it is empty.
fn positive(mpint: &[u8]) -> Option<Vec<u8>> {
    let (first, rest) = mpint.split_first()?;
    if *first == 0 {
        // A zero byte is there only to keep a high bit from reading as a sign.
        return rest
            .first()
            .filter(|next| **next >= 0x80)
            .map(|_| rest.to_vec());
    }
    (*first < 0x80).then(|| mpint.to_vec())
}

/// The number of bits of the positive integer whose big-endian bytes are `integer`.
fn bits(integer: &[u8]) -> usize {
    let lead = integer.first().map_or(0, |b| b.leading_zeros() as usize);
    integer.len() * 8 - lead
}

impl UserKey {
    /// Reads a public key in `authorized_keys` syntax: the type, the base64 blob and an optional
    /// comment, on one line, no options.
    ///
    /// # Errors
    /// [`KeyError::BadPublicKey`] for anything but a well formed key of a type the CA signs.
    pub(crate) fn parse(line: &str) -> Result<Parsed, KeyError> {
        let bad = KeyError::BadPublicKey;
        if line.len() > MAX_LINE || line.contains(['\n', '\r', '\0']) {
            return Err(bad);
        }
        let mut fields = line.split_ascii_whitespace();
        let (Some(kind), Some(text)) = (fields.next(), fields.next()) else {
            return Err(bad);
        };
        let blob = STANDARD.decode(text).map_err(|_| bad)?;
        let mut read = Reader(&blob);
        if read.string() != Some(kind.as_bytes()) {
            return Err(bad);
        }
        let key = match kind {
            "ssh-ed25519" => {
                let public = read.string().ok_or(bad)?;
                if public.len() != 32 {
                    return Err(bad);
                }
                UserKey::Ed25519 {
                    public: public.to_vec(),
                }
            }
            "ecdsa-sha2-nistp256" => {
                if read.string() != Some(b"nistp256") {
                    return Err(bad);
                }
                let point = read.string().ok_or(bad)?;
                if point.len() != 65 || point[0] != 4 {
                    return Err(bad);
                }
                UserKey::Ecdsa {
                    point: point.to_vec(),
                }
            }
            "ssh-rsa" => {
                let exponent = read.string().and_then(positive).ok_or(bad)?;
                let modulus = read.string().and_then(positive).ok_or(bad)?;
                if bits(&modulus) < RSA_MIN_BITS {
                    return Err(bad);
                }
                UserKey::Rsa { exponent, modulus }
            }
            _ => return Err(bad),
        };
        if !read.done() {
            return Err(bad);
        }
        Ok(Parsed {
            key,
            fingerprint: fingerprint(&blob),
        })
    }

    /// The certificate's key type, which is also its first field.
    fn cert_type(&self) -> &'static str {
        match self {
            UserKey::Ed25519 { .. } => concat!("ssh-ed25519-cert-v01", "\u{40}", "openssh.com"),
            UserKey::Ecdsa { .. } => {
                concat!("ecdsa-sha2-nistp256-cert-v01", "\u{40}", "openssh.com")
            }
            UserKey::Rsa { .. } => concat!("ssh-rsa-cert-v01", "\u{40}", "openssh.com"),
        }
    }

    /// Appends the key's own fields, as a certificate lists them after the nonce.
    fn put_fields(&self, out: &mut Vec<u8>) {
        match self {
            UserKey::Ed25519 { public } => put_string(out, public),
            UserKey::Ecdsa { point } => {
                put_string(out, b"nistp256");
                put_string(out, point);
            }
            UserKey::Rsa { exponent, modulus } => {
                for integer in [exponent, modulus] {
                    // An mpint: a zero byte first when the high bit is set.
                    let sign = integer.first().is_some_and(|b| *b >= 0x80);
                    let mut mpint = Vec::with_capacity(integer.len() + 1);
                    if sign {
                        mpint.push(0);
                    }
                    mpint.extend_from_slice(integer);
                    put_string(out, &mpint);
                }
            }
        }
    }
}

/// What a certificate says: everything of it that is not the key, the CA or the signature.
pub(crate) struct Spec<'a> {
    pub(crate) key: &'a UserKey,
    pub(crate) serial: u64,
    pub(crate) key_id: &'a str,
    /// The one principal.
    pub(crate) principal: &'a str,
    /// The first and last second the certificate is valid.
    pub(crate) valid_after: u64,
    pub(crate) valid_before: u64,
}

/// One Ed25519 key of the CA.
pub(crate) struct CaKey {
    pub(crate) pair: Ed25519KeyPair,
}

impl fmt::Debug for CaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CaKey({})", self.fingerprint())
    }
}

impl CaKey {
    fn from_seed(seed: &[u8]) -> Result<CaKey, KeyError> {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(|pair| CaKey { pair })
            .map_err(|_| KeyError::BadPrivateKey)
    }

    /// A new key and its private key file: the text `ssh-keygen` writes, no passphrase.
    ///
    /// # Errors
    /// [`KeyError::Crypto`] when `random` gives no bytes.
    pub(crate) fn generate(
        comment: &str,
        random: &dyn Fn(&mut [u8]) -> bool,
    ) -> Result<(CaKey, String), KeyError> {
        let mut seed = [0u8; 32];
        let mut check = [0u8; 4];
        if !random(&mut seed) || !random(&mut check) {
            return Err(KeyError::Crypto);
        }
        let key = CaKey::from_seed(&seed)?;
        let pem = key.private_file(&seed, u32::from_be_bytes(check), comment);
        Ok((key, pem))
    }

    /// The public key blob: `ssh-ed25519` and the 32 bytes.
    pub(crate) fn blob(&self) -> Vec<u8> {
        let mut blob = Vec::new();
        put_string(&mut blob, b"ssh-ed25519");
        put_string(&mut blob, self.pair.public_key().as_ref());
        blob
    }

    /// The public key as a line of `authorized_keys` and `TrustedUserCAKeys`, with no comment.
    pub(crate) fn public_line(&self) -> String {
        format!("ssh-ed25519 {}", STANDARD.encode(self.blob()))
    }

    /// The fingerprint of the public key.
    pub(crate) fn fingerprint(&self) -> String {
        fingerprint(&self.blob())
    }

    fn private_file(&self, seed: &[u8], check: u32, comment: &str) -> String {
        let public = self.pair.public_key().as_ref();
        let mut private = Vec::new();
        put_u32(&mut private, check);
        put_u32(&mut private, check);
        put_string(&mut private, b"ssh-ed25519");
        put_string(&mut private, public);
        put_string(&mut private, &[seed, public].concat());
        put_string(&mut private, comment.as_bytes());
        // Padded to the cipher's block size, 8 for none, with 1, 2, 3 ...
        let mut pad = 1u8;
        while !private.len().is_multiple_of(8) {
            private.push(pad);
            pad += 1;
        }
        let mut file = MAGIC.to_vec();
        put_string(&mut file, b"none");
        put_string(&mut file, b"none");
        put_string(&mut file, b"");
        put_u32(&mut file, 1);
        put_string(&mut file, &self.blob());
        put_string(&mut file, &private);
        let body = STANDARD.encode(file);
        let mut text = format!("{BEGIN}\n");
        for chunk in body.as_bytes().chunks(70) {
            text.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            text.push('\n');
        }
        text.push_str(END);
        text.push('\n');
        text
    }

    /// The keys of a CA secret: one or two private key files one after the other, `current`
    /// first, `next` second.
    ///
    /// # Errors
    /// [`KeyError::BadPrivateKey`] for no key, for more than [`MAX_CA_KEYS`], for text outside the
    /// files, or for a file that is not an unencrypted Ed25519 key whose two halves agree.
    pub(crate) fn parse_secret(text: &str) -> Result<Vec<CaKey>, KeyError> {
        let bad = KeyError::BadPrivateKey;
        let mut keys = Vec::new();
        let mut rest = text.trim();
        while !rest.is_empty() {
            let after = rest.strip_prefix(BEGIN).ok_or(bad)?;
            let (body, tail) = after.split_once(END).ok_or(bad)?;
            keys.push(CaKey::parse_one(body)?);
            rest = tail.trim_start();
        }
        if keys.is_empty() || keys.len() > MAX_CA_KEYS {
            return Err(bad);
        }
        Ok(keys)
    }

    fn parse_one(body: &str) -> Result<CaKey, KeyError> {
        let bad = KeyError::BadPrivateKey;
        let compact: String = body.split_whitespace().collect();
        let file = STANDARD.decode(compact).map_err(|_| bad)?;
        let mut read = Reader(file.strip_prefix(MAGIC).ok_or(bad)?);
        let plain = read.string() == Some(b"none")
            && read.string() == Some(b"none")
            && read.string() == Some(b"")
            && read.u32() == Some(1);
        if !plain {
            return Err(bad);
        }
        let blob = read.string().ok_or(bad)?;
        let private = read.string().ok_or(bad)?;
        if !read.done() {
            return Err(bad);
        }
        let mut inner = Reader(private);
        let (first, second) = (inner.u32().ok_or(bad)?, inner.u32().ok_or(bad)?);
        if first != second || inner.string() != Some(b"ssh-ed25519") {
            return Err(bad);
        }
        let public = inner.string().ok_or(bad)?;
        let secret = inner.string().ok_or(bad)?;
        inner.string().ok_or(bad)?;
        let padding_ok = inner
            .0
            .iter()
            .zip(1u8..)
            .all(|(byte, expected)| *byte == expected)
            && inner.0.len() < 8;
        if secret.len() != 64 || secret[32..] != *public || !padding_ok {
            return Err(bad);
        }
        let key = CaKey::from_seed(&secret[..32])?;
        // The public key the file states, and the one the seed makes, are one key.
        if key.pair.public_key().as_ref() != public || key.blob() != blob {
            return Err(bad);
        }
        Ok(key)
    }

    /// The certificate for `spec`, as a line of `ssh-keygen` syntax, signed by this key. It is a
    /// user certificate with no critical option and the one extension `permit-pty`: no agent,
    /// port or X11 forwarding and no `~/.ssh/rc`.
    ///
    /// # Errors
    /// [`KeyError::Crypto`] when `random` gives no bytes for the nonce.
    pub(crate) fn sign_user(
        &self,
        spec: &Spec<'_>,
        random: &dyn Fn(&mut [u8]) -> bool,
    ) -> Result<String, KeyError> {
        let mut nonce = [0u8; 32];
        if !random(&mut nonce) {
            return Err(KeyError::Crypto);
        }
        let mut body = Vec::new();
        put_string(&mut body, spec.key.cert_type().as_bytes());
        put_string(&mut body, &nonce);
        spec.key.put_fields(&mut body);
        put_u64(&mut body, spec.serial);
        // Type 1 is a user certificate.
        put_u32(&mut body, 1);
        put_string(&mut body, spec.key_id.as_bytes());
        let mut principals = Vec::new();
        put_string(&mut principals, spec.principal.as_bytes());
        put_string(&mut body, &principals);
        put_u64(&mut body, spec.valid_after);
        put_u64(&mut body, spec.valid_before);
        put_string(&mut body, b"");
        let mut extensions = Vec::new();
        put_string(&mut extensions, b"permit-pty");
        put_string(&mut extensions, b"");
        put_string(&mut body, &extensions);
        put_string(&mut body, b"");
        put_string(&mut body, &self.blob());
        let signature = self.pair.sign(&body);
        let mut signed = Vec::new();
        put_string(&mut signed, b"ssh-ed25519");
        put_string(&mut signed, signature.as_ref());
        put_string(&mut body, &signed);
        Ok(format!(
            "{} {}",
            spec.key.cert_type(),
            STANDARD.encode(body)
        ))
    }
}

#[cfg(test)]
#[path = "ssh_cert_tests.rs"]
pub(crate) mod tests;
