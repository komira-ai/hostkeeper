//! The secrets of the MDM setup (`docs/design/coordinator-mdm.md`, section 4.9): what they are,
//! how they are sealed, and the key they are sealed under.
//!
//! **Kinds.** [`SecretKind`] names each record the setup keeps: the push key and certificate, the
//! encryption key of the push request, the ABM key and certificate, the ABM token (four parts and
//! an expiry, [`AbmToken`]), the MDM CA key, the ACME account key, the DNS credential and the TLS
//! key and certificate. A kind is spelled in lower case with `_`, as the table holds it.
//!
//! **Sealing.** A value is sealed with AES-256-GCM under the master key before it reaches a store.
//! The sealed bytes are a random 12-byte nonce, then the ciphertext and its 16-byte tag. The
//! associated data is a fixed label, the kind and the version, so a row copied into another kind
//! or another version does not open ([`SealError::Open`]), and a store, a backup of it and a log
//! of it hold ciphertext only.
//!
//! **The master key.** `--mdm-secrets-key-file` names a file of 64 hex digits (256 bits). The file
//! is read under the rule of every secret file (a regular file of the coordinator's user, mode
//! `0600` or `0400`, no symlink followed). When it is absent and the store holds no sealed row, the
//! start makes it (mode `0600`, never over a file that is there); when it is absent, or the flag is
//! not given, and the store holds a sealed row, the start is refused, because a new key would
//! orphan every secret ([`MasterKey::load`]). A key that opens none of the rows the store holds is
//! refused too ([`crate::mdm_setup::MdmSetup::check`]).
//!
//! **Plaintext** lives in a [`Secret`], whose `Debug` and `Display` print nothing of it, and in the
//! master key, which prints nothing either.

use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use hostkeeper_wire::secret::{SecretError, read_secret};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use serde::Deserialize;

/// The flag that names the key file, which an error text names in place of its path.
pub const KEY_FLAG: &str = "--mdm-secrets-key-file";

/// The most bytes one secret may be: a certificate, a key and a token are each a few kilobytes.
pub const MAX_SECRET_BYTES: usize = 32 * 1024;

/// The label every seal's associated data starts with.
const AAD_LABEL: &[u8] = b"hostkeeper-mdm-secret/v1";

/// The length of the key, in bytes.
const KEY_LEN: usize = 32;

/// The length of the AES-GCM tag.
const TAG_LEN: usize = 16;

/// The most bytes the key file is read to: 64 digits and room for a newline.
const KEY_FILE_LIMIT: u64 = 128;

/// A kind of secret the setup keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SecretKind {
    /// The RSA key (PKCS#1 PEM) of the push certificate.
    PushKey,
    /// The push certificate Apple issued.
    PushCert,
    /// The encryption key of the push certificate request (the mdmcert.download route).
    PushEncryptionKey,
    /// The key of the push certificate request that is pending, and its CSR, as two PEM blocks:
    /// the key moves to [`Self::PushKey`] when the certificate Apple issues for it is uploaded.
    PushPendingKey,
    /// The RSA key (PKCS#1 PEM) the ABM token is encrypted to.
    AbmKey,
    /// The certificate of the ABM key.
    AbmCert,
    /// The ABM token: the four OAuth parts and their expiry, as [`AbmToken`] spells them.
    AbmToken,
    /// The key of the CA that issues the Macs' identities.
    MdmCaKey,
    /// The ACME account key of the MDM name's certificate.
    AcmeAccountKey,
    /// The DNS provider's API key and secret, for the ACME challenge.
    DnsCredential,
    /// The key of the MDM name's TLS certificate.
    TlsKey,
    /// The TLS certificate chain of the MDM name.
    TlsCert,
}

impl SecretKind {
    /// Every kind, in the order the status lists them.
    pub const ALL: [SecretKind; 12] = [
        Self::PushKey,
        Self::PushCert,
        Self::PushEncryptionKey,
        Self::PushPendingKey,
        Self::AbmKey,
        Self::AbmCert,
        Self::AbmToken,
        Self::MdmCaKey,
        Self::AcmeAccountKey,
        Self::DnsCredential,
        Self::TlsKey,
        Self::TlsCert,
    ];

    /// The spelling the tables and the requests use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PushKey => "push_key",
            Self::PushCert => "push_cert",
            Self::PushEncryptionKey => "push_encryption_key",
            Self::PushPendingKey => "push_pending_key",
            Self::AbmKey => "abm_key",
            Self::AbmCert => "abm_cert",
            Self::AbmToken => "abm_token",
            Self::MdmCaKey => "mdm_ca_key",
            Self::AcmeAccountKey => "acme_account_key",
            Self::DnsCredential => "dns_credential",
            Self::TlsKey => "tls_key",
            Self::TlsCert => "tls_cert",
        }
    }

    /// The kind spelled `text`, as [`Self::as_str`] spells it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// The audit action of an upload of this kind (`docs/design/coordinator-mdm.md`, 4.9).
    #[must_use]
    pub fn put_action(self) -> &'static str {
        match self {
            Self::PushKey | Self::PushCert => crate::audit_actions::MDM_PUSH_CERT_PUT,
            Self::AbmToken => crate::audit_actions::MDM_ABM_TOKEN_PUT,
            Self::TlsKey | Self::TlsCert => crate::audit_actions::MDM_TLS_PUT,
            Self::AcmeAccountKey | Self::DnsCredential => {
                crate::audit_actions::MDM_ACME_CREDENTIAL_PUT
            }
            Self::PushEncryptionKey
            | Self::PushPendingKey
            | Self::AbmKey
            | Self::AbmCert
            | Self::MdmCaKey => crate::audit_actions::MDM_SECRET_PUT,
        }
    }
}

/// Secret bytes in memory. The `Debug` and the `Display` print that there is a secret and nothing
/// of it, so that no log line, error or failed assertion shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// The secret `bytes`.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the code that uses the secret and for nothing that prints.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it holds no byte.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The ABM token: the four parts of its OAuth 1.0a session and when the access secret expires, in
/// the shape of the decrypted `.p7m` body (`access_secret_expiry`, RFC 3339).
///
/// Its `Debug` names none of the parts.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct AbmToken {
    /// The consumer key.
    pub consumer_key: String,
    /// The consumer secret.
    pub consumer_secret: String,
    /// The access token.
    pub access_token: String,
    /// The access secret.
    pub access_secret: String,
    /// When the access secret expires, as ABM wrote it.
    pub access_secret_expiry: String,
}

impl AbmToken {
    /// The token `json` holds: all five fields, none empty.
    ///
    /// # Errors
    /// [`TokenError`]: the text is no JSON object of the five fields, or a field is empty. The
    /// error names no part of the text.
    pub fn parse(json: &[u8]) -> Result<Self, TokenError> {
        let token: Self = serde_json::from_slice(json).map_err(|_| TokenError)?;
        let parts = [
            &token.consumer_key,
            &token.consumer_secret,
            &token.access_token,
            &token.access_secret,
            &token.access_secret_expiry,
        ];
        if parts.iter().any(|part| part.is_empty()) {
            return Err(TokenError);
        }
        Ok(token)
    }
}

impl fmt::Debug for AbmToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AbmToken(<redacted>)")
    }
}

/// The ABM token is not the JSON object of its five parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "the ABM token is not a JSON object of consumer_key, consumer_secret, access_token, access_secret and access_secret_expiry, each set"
)]
pub struct TokenError;

/// Why a value was not sealed, or did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// The value could not be sealed.
    #[error("the value could not be sealed")]
    Seal,
    /// The bytes do not open under this key, kind and version: they were sealed under another key,
    /// for another kind or version, or they were changed.
    #[error("the sealed value does not open for this kind and version")]
    Open,
}

/// The key every secret is sealed under. Its `Debug` prints nothing of it.
pub struct MasterKey(LessSafeKey);

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

/// Why the master key was not made or read at start. Every text names the flag, never the path or
/// the key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The file is refused by the rule of secret files, or cannot be read.
    #[error(transparent)]
    File(#[from] SecretError),
    /// The file does not hold 64 hex digits.
    #[error("{KEY_FLAG}: the file must hold 64 hex digits, the 256-bit key")]
    Format,
    /// The file is not there, and the store holds sealed secrets: a new key would orphan them.
    #[error(
        "{KEY_FLAG}: the file is not there and the store holds sealed MDM secrets; restore the key file the secrets were sealed under (a new key would orphan every one)"
    )]
    Missing,
    /// The flag is not given, and the store holds sealed secrets.
    #[error(
        "{KEY_FLAG} is not given and the store holds sealed MDM secrets; give the key file they were sealed under"
    )]
    NotGiven,
    /// The file could not be made.
    #[error("{KEY_FLAG}: the file could not be made: {0}")]
    Create(std::io::ErrorKind),
    /// The system gave no random bytes for a new key.
    #[error("{KEY_FLAG}: the system gave no random bytes for a new key")]
    NoRandom,
}

impl MasterKey {
    /// The key of `bytes`, which are 32.
    ///
    /// # Errors
    /// [`KeyError::Format`]: they are not.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        UnboundKey::new(&AES_256_GCM, bytes)
            .map(|key| Self(LessSafeKey::new(key)))
            .map_err(|_| KeyError::Format)
    }

    /// The key the flag `path` names, under the start rules of the module: read when the file is
    /// there; made when it is not and the store holds no sealed row (`sealed` is whether it does);
    /// no key when the flag is not given and none is held.
    ///
    /// # Errors
    /// [`KeyError`], as each variant says. `random` fills the bytes of a new key.
    pub fn load(
        path: Option<&Path>,
        sealed: bool,
        random: &dyn Fn(&mut [u8]) -> bool,
    ) -> Result<Option<Self>, KeyError> {
        let Some(path) = path else {
            return if sealed {
                Err(KeyError::NotGiven)
            } else {
                Ok(None)
            };
        };
        match read_secret(path, KEY_FLAG, KEY_FILE_LIMIT) {
            Ok(bytes) => Self::parse(&bytes).map(Some),
            Err(SecretError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => {
                if sealed {
                    Err(KeyError::Missing)
                } else {
                    Self::create(path, random).map(Some)
                }
            }
            Err(other) => Err(other.into()),
        }
    }

    /// The key a file's `bytes` hold: 64 hex digits and surrounding whitespace.
    fn parse(bytes: &[u8]) -> Result<Self, KeyError> {
        let text = std::str::from_utf8(bytes).map_err(|_| KeyError::Format)?;
        let key = hex::decode(text.trim()).map_err(|_| KeyError::Format)?;
        Self::from_bytes(&key)
    }

    /// Makes the file at `path` (mode `0600`, never over a file that is there) holding a new key,
    /// and the key.
    fn create(path: &Path, random: &dyn Fn(&mut [u8]) -> bool) -> Result<Self, KeyError> {
        let mut bytes = [0_u8; KEY_LEN];
        if !random(&mut bytes) {
            return Err(KeyError::NoRandom);
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| KeyError::Create(e.kind()))?;
        let text = format!("{}\n", hex::encode(bytes));
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| KeyError::Create(e.kind()))?;
        Self::from_bytes(&bytes)
    }

    /// The associated data of a seal of `kind` at `version`.
    fn aad(kind: &str, version: u32) -> Vec<u8> {
        let mut aad = AAD_LABEL.to_vec();
        aad.push(0);
        aad.extend_from_slice(kind.as_bytes());
        aad.push(0);
        aad.extend_from_slice(&version.to_be_bytes());
        aad
    }

    /// `plain` sealed for `kind` at `version` with `nonce`: the nonce, then the ciphertext and its
    /// tag.
    ///
    /// # Errors
    /// [`SealError::Seal`]: the cipher refused the value.
    pub fn seal(
        &self,
        kind: &str,
        version: u32,
        plain: &Secret,
        nonce: [u8; NONCE_LEN],
    ) -> Result<Vec<u8>, SealError> {
        let mut sealed = plain.0.clone();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(Self::aad(kind, version)),
                &mut sealed,
            )
            .map_err(|_| SealError::Seal)?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// The secret `sealed` holds, opened for `kind` at `version`.
    ///
    /// # Errors
    /// [`SealError::Open`]: it does not open under this key for that kind and version.
    pub fn open(&self, kind: &str, version: u32, sealed: &[u8]) -> Result<Secret, SealError> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(SealError::Open);
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| SealError::Open)?;
        let mut plain = body.to_vec();
        let opened = self
            .0
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(Self::aad(kind, version)),
                &mut plain,
            )
            .map_err(|_| SealError::Open)?
            .len();
        plain.truncate(opened);
        Ok(Secret(plain))
    }
}

#[cfg(test)]
#[path = "mdm_secrets_tests.rs"]
mod tests;
