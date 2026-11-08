//! The package digest of a release, and the registry of every version CI has set for a service
//! with its release (design section 10: the desired state carries "version and release digest
//! per service, and the digest of every version CI has set").
//!
//! A digest is the SHA-256 of the package, 64 lowercase hex characters: the form the operator
//! API's `package_digest` has. A [`Release`] is what CI registered for a version: one package
//! digest (a fleet of one platform; the registry writes it as that digest's text, as it always
//! did), or the record of a release index ([`IndexRecord`]: the SHA-256 of the index and the
//! package digest it lists for each platform, written as an object), from which each host is
//! ordered the package of its own platform. The registry is kept in the state file beside the
//! service's desired state ([`crate::desired`]) and is written in the same replacement of that
//! file, so the two are never apart. It never forgets a version on its own: it holds at most
//! [`MAX_REGISTRY`] versions per service, and only a new version past that drops one, the
//! oldest that the caller does not keep (the desired version, the last good one and those
//! marked bad are kept). The oldest is the lowest by [`Version::release_numbers`], then by text.

use std::collections::BTreeMap;

use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_wire::Platform;
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

/// A platform as a release index spells it (`linux-x86_64`, `linux-arm64`, `macos-arm64`): the
/// key of [`IndexRecord::packages`]. A host's platform is the wire's ([`Platform`], which spells
/// the Mac `darwin-arm64`); [`index_platform`] is the one place that maps one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexKey(IndexPlatform);

/// The platform, in the index's spelling, that the wire's `platform` is.
#[must_use]
pub const fn index_platform(platform: Platform) -> IndexPlatform {
    match platform {
        Platform::LinuxX86_64 => IndexPlatform::LinuxX86_64,
        Platform::LinuxArm64 => IndexPlatform::LinuxArm64,
        Platform::DarwinArm64 => IndexPlatform::MacosArm64,
    }
}

impl IndexKey {
    /// The key of `platform`.
    #[must_use]
    pub const fn of(platform: IndexPlatform) -> Self {
        Self(platform)
    }
}

impl Serialize for IndexKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for IndexKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// What the coordinator keeps of a release index it checked: the SHA-256 of the index, and the
/// SHA-256 of the package it lists for each platform. The index's text is not kept: a host
/// fetches the index itself and checks its SHA-256 against the one its order names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexRecord {
    /// The SHA-256 of the index's text.
    pub digest: Digest,
    /// The package digest of each platform the index lists.
    pub packages: BTreeMap<IndexKey, Digest>,
}

/// The package a host is ordered of a release: its digest, and the digest of the index it came
/// from, when it came from one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// The SHA-256 of the package of the host's platform.
    pub digest: Digest,
    /// The SHA-256 of the release index the digest was taken from; none for a release of one
    /// package digest.
    pub index_digest: Option<Digest>,
}

impl IndexRecord {
    /// The package the index lists for `platform`, with the index's digest.
    #[must_use]
    pub fn package_for(&self, platform: Platform) -> Option<Package> {
        self.packages
            .get(&IndexKey::of(index_platform(platform)))
            .map(|digest| Package {
                digest: digest.clone(),
                index_digest: Some(self.digest.clone()),
            })
    }
}

/// What CI registered for a version: one package digest, or a release index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Release {
    /// One package, ordered to every host whatever its platform.
    Package(Digest),
    /// A release index: each host is ordered the package of its own platform.
    Index(IndexRecord),
}

impl Release {
    /// The package a host of `platform` is ordered of this release; none when the release is an
    /// index that lists no package for the platform.
    #[must_use]
    pub fn package_for(&self, platform: Platform) -> Option<Package> {
        match self {
            Self::Package(digest) => Some(Package {
                digest: digest.clone(),
                index_digest: None,
            }),
            Self::Index(record) => record.package_for(platform),
        }
    }
}

/// The versions CI has set for one service, each with its release, by version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Registry(BTreeMap<String, Release>);

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
    /// The registry in `value`, an object of versions to releases.
    ///
    /// # Errors
    /// [`RegistryError`]: not such an object, a key that is no version or a value that is
    /// neither a digest nor the record of an index.
    pub fn read(value: serde_json::Value) -> Result<Self, RegistryError> {
        let map: BTreeMap<String, Release> = serde_json::from_value(value)
            .map_err(|why| RegistryError::Malformed(why.to_string()))?;
        if let Some(bad) = map.keys().find(|key| Version::parse(key).is_err()) {
            return Err(RegistryError::Version(bad.clone()));
        }
        Ok(Self(map))
    }

    /// The release CI set with `version`, if it set one.
    #[must_use]
    pub fn get(&self, version: &Version) -> Option<&Release> {
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

    /// Records `release` for `version`. Past [`MAX_REGISTRY`] versions, the oldest one that
    /// `keep` does not keep is dropped; when `keep` keeps every one, none is dropped and the
    /// registry holds more than the cap.
    pub fn record(&mut self, version: &Version, release: Release, keep: impl Fn(&Version) -> bool) {
        self.0.insert(version.as_str().to_owned(), release);
        while self.0.len() > MAX_REGISTRY {
            let Some(oldest) = oldest_droppable(self.0.keys(), version, &keep) else {
                return;
            };
            self.0.remove(oldest.as_str());
        }
    }
}

/// Of the versions `keys`, the oldest that is not `version` and that `keep` does not keep: the
/// lowest release numbers, then the lowest text. None when every other one is kept. The registry
/// and the releases ([`crate::releases::record`]) drop a version past their cap by this rule.
pub(crate) fn oldest_droppable<'a>(
    keys: impl Iterator<Item = &'a String>,
    version: &Version,
    keep: &dyn Fn(&Version) -> bool,
) -> Option<Version> {
    keys.filter_map(|text| Version::parse(text).ok())
        .filter(|v| v != version && !keep(v))
        .min_by(|a, b| {
            a.release_numbers()
                .cmp(&b.release_numbers())
                .then_with(|| a.as_str().cmp(b.as_str()))
        })
}
