//! The DNS provider of the ACME challenge (`docs/design/coordinator-mdm.md`, section 4.10).
//!
//! **The port.** [`DnsProvider`] publishes one TXT value at a name of the zone, says whether the
//! provider reports it, and removes it. A provider whose API replaces every record of a name when
//! it writes one (GoDaddy's `PUT` does) must read what is there and keep it, which is the
//! provider's code and not the caller's.
//!
//! **The zone is a setting** (`dns_zone`), never worked out by taking a label off the name: the
//! name `mdm.internal.example.net` lives in the zone `example.net`, and its challenge record is
//! `_acme-challenge.mdm.internal` in that zone ([`challenge_record`]).
//!
//! **The credential** is one sealed record (`dns_credential`), JSON that names the provider and
//! the kind of credential it is: `sso-key` (a key and a secret) now, and `bearer` (a token) for
//! the provider's personal access token later ([`DnsCredential`]). Its text is never printed.

use std::sync::Arc;

use hostkeeper_mdm_push::BoxFuture;
use serde::Deserialize;

/// What a provider is asked to publish: the value of the TXT record at `name` in `zone`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxtRecord {
    /// The zone the provider hosts, as the setting spells it.
    pub zone: String,
    /// The name inside the zone, as [`challenge_record`] spells it.
    pub name: String,
    /// The TXT value.
    pub value: String,
    /// The record's time to live, in seconds.
    pub ttl_s: u32,
}

/// Why a provider call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DnsError {
    /// The provider refused the credential (401 or 403): it is wrong, expired or has no right to
    /// the zone. Retrying cannot help until an operator uploads another.
    #[error("the DNS provider refused the credential (status {status})")]
    Rejected {
        /// The status.
        status: u16,
    },
    /// Anything else: no reply, another status, a reply that is not understood.
    #[error("the DNS provider call failed: {0}")]
    Failed(String),
}

/// A DNS provider's TXT records for the challenge.
pub trait DnsProvider: Send + Sync {
    /// Makes `record.value` one of the TXT values at the name, keeping the others there.
    ///
    /// # Errors
    /// [`DnsError`].
    fn publish<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>>;

    /// Whether the provider reports `record.value` among the TXT values at the name.
    ///
    /// # Errors
    /// [`DnsError`].
    fn is_published<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<bool, DnsError>>;

    /// Removes `record.value` from the TXT values at the name, keeping the others.
    ///
    /// # Errors
    /// [`DnsError`].
    fn remove<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>>;
}

/// Opens the provider a credential names.
pub trait DnsProviders: Send + Sync {
    /// The provider `credential` is for.
    ///
    /// # Errors
    /// [`CredentialError::UnknownProvider`].
    fn open(&self, credential: &DnsCredential) -> Result<Arc<dyn DnsProvider>, CredentialError>;
}

/// Why a credential is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// The record is not the JSON of a credential this release reads.
    #[error("the DNS credential is not understood: {0}")]
    Invalid(String),
    /// The provider it names is not one this release has.
    #[error("no DNS provider called {0}")]
    UnknownProvider(String),
}

/// What authenticates the coordinator to the provider.
#[derive(Clone, PartialEq, Eq)]
pub enum Authorization {
    /// A key and a secret (GoDaddy's `sso-key`).
    SsoKey {
        /// The key.
        key: String,
        /// The secret.
        secret: String,
    },
    /// A bearer token (a personal access token).
    Bearer {
        /// The token.
        token: String,
    },
}

impl Authorization {
    /// The value of the `Authorization` header.
    #[must_use]
    pub fn header(&self) -> String {
        match self {
            Self::SsoKey { key, secret } => format!("sso-key {key}:{secret}"),
            Self::Bearer { token } => format!("Bearer {token}"),
        }
    }
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SsoKey { .. } => "Authorization::SsoKey(..)",
            Self::Bearer { .. } => "Authorization::Bearer(..)",
        })
    }
}

/// The DNS credential: the provider and what authenticates to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsCredential {
    /// The provider's name (`godaddy`).
    pub provider: String,
    /// What authenticates.
    pub authorization: Authorization,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    provider: String,
    kind: String,
    key: Option<String>,
    secret: Option<String>,
    token: Option<String>,
}

/// The longest field of a credential accepted.
const MAX_FIELD: usize = 512;

impl DnsCredential {
    /// The credential spelled in `json`: `{"provider":"godaddy","kind":"sso-key","key":"..",
    /// "secret":".."}` or `{"provider":"godaddy","kind":"bearer","token":".."}`.
    ///
    /// # Errors
    /// [`CredentialError::Invalid`]; the text of an error never holds a value of the record.
    pub fn parse(json: &[u8]) -> Result<Self, CredentialError> {
        let invalid = |why: &str| CredentialError::Invalid(why.to_owned());
        let wire: Wire = serde_json::from_slice(json)
            .map_err(|_| invalid("it is not the JSON of a credential"))?;
        // A header value: printable ASCII, no space, no control character.
        let field = |value: Option<String>, what: &str| -> Result<String, CredentialError> {
            let value = value.ok_or_else(|| invalid(&format!("{what} is missing")))?;
            let clean = !value.is_empty()
                && value.len() <= MAX_FIELD
                && value.bytes().all(|b| b.is_ascii_graphic() && b != b':');
            clean.then_some(value).ok_or_else(|| {
                invalid(&format!("{what} is empty, too long or has a bad character"))
            })
        };
        let authorization = match wire.kind.as_str() {
            "sso-key" => Authorization::SsoKey {
                key: field(wire.key, "the key")?,
                secret: field(wire.secret, "the secret")?,
            },
            "bearer" => Authorization::Bearer {
                token: field(wire.token, "the token")?,
            },
            _ => return Err(invalid("the kind is neither sso-key nor bearer")),
        };
        if wire.provider.is_empty() {
            return Err(invalid("the provider is empty"));
        }
        Ok(Self {
            provider: wire.provider,
            authorization,
        })
    }
}

/// The name of the challenge record of `name` inside `zone`: `_acme-challenge.` and the labels of
/// `name` before the zone. None when `name` is neither the zone nor a name under it.
#[must_use]
pub fn challenge_record(name: &str, zone: &str) -> Option<String> {
    if name == zone {
        return Some("_acme-challenge".to_owned());
    }
    let inside = name.strip_suffix(zone)?.strip_suffix('.')?;
    (!inside.is_empty()).then(|| format!("_acme-challenge.{inside}"))
}

#[cfg(test)]
#[path = "dns_tests.rs"]
mod tests;
