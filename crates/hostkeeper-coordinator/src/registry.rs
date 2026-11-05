//! The package digest of a release, and the registry of every version CI has set for a service
//! with the digest of its package (design section 10: the desired state carries "version and
//! release digest per service, and the digest of every version CI has set").
//!
//! A digest is the SHA-256 of the package, 64 lowercase hex characters: the form the operator
//! API's `package_digest` has. The registry is kept in the state file beside the service's
//! desired state ([`crate::desired`]) and is written in the same replacement of that file, so
//! the two are never apart. It never forgets a version on its own: it holds at most
//! [`MAX_REGISTRY`] versions per service, and only a new version past that drops one, the
//! oldest that the caller does not keep (the desired version, the last good one and those
//! marked bad are kept). The oldest is the lowest by [`Version::release_numbers`], then by text.

use std::collections::BTreeMap;

use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

/// The most versions the registry of one service holds.
pub const MAX_REGISTRY: usize = 4096;

/// The SHA-256 of a package, as 64 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

/// The text is not a digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a digest is 64 lowercase hex characters")]
pub struct DigestError;

impl Digest {
    /// `text` as a digest.
    ///
    /// # Errors
    /// [`DigestError`]: not 64 characters of `0-9a-f`.
    pub fn parse(text: &str) -> Result<Self, DigestError> {
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if text.len() == 64 && text.bytes().all(hex) {
            Ok(Self(text.to_owned()))
        } else {
            Err(DigestError)
        }
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The versions CI has set for one service, each with the digest of its package, by version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Registry(BTreeMap<String, Digest>);

/// Why a registry as read is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Not an object of versions to digests.
    #[error("digests: {0}")]
    Malformed(String),
    /// A key is not a version.
    #[error("digests: {0:?} is not a version")]
    Version(String),
}

impl Registry {
    /// The registry in `value`, an object of versions to digests.
    ///
    /// # Errors
    /// [`RegistryError`]: not such an object, a key that is no version or a value that is no
    /// digest.
    pub fn read(value: serde_json::Value) -> Result<Self, RegistryError> {
        let map: BTreeMap<String, Digest> = serde_json::from_value(value)
            .map_err(|why| RegistryError::Malformed(why.to_string()))?;
        if let Some(bad) = map.keys().find(|key| Version::parse(key).is_err()) {
            return Err(RegistryError::Version(bad.clone()));
        }
        Ok(Self(map))
    }

    /// The digest CI set with `version`, if it set one.
    #[must_use]
    pub fn get(&self, version: &Version) -> Option<&Digest> {
        self.0.get(version.as_str())
    }

    /// How many versions it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Records `digest` for `version`. Past [`MAX_REGISTRY`] versions, the oldest one that
    /// `keep` does not keep is dropped; when `keep` keeps every one, none is dropped and the
    /// registry holds more than the cap.
    pub fn record(&mut self, version: &Version, digest: Digest, keep: impl Fn(&Version) -> bool) {
        self.0.insert(version.as_str().to_owned(), digest);
        while self.0.len() > MAX_REGISTRY {
            let oldest = self
                .0
                .keys()
                .filter_map(|text| Version::parse(text).ok())
                .filter(|v| v != version && !keep(v))
                .min_by(|a, b| {
                    a.release_numbers()
                        .cmp(&b.release_numbers())
                        .then_with(|| a.as_str().cmp(b.as_str()))
                });
            let Some(oldest) = oldest else {
                return;
            };
            self.0.remove(oldest.as_str());
        }
    }
}
