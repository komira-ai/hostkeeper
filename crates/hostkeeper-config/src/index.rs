//! The release index: `index.json` beside the packages of one version of a service in the release
//! channel, and the paths of the channel (docs/design/hostkeeper-design.md, section 5.5).
//!
//! ```text
//! {channel}/{S}/{V}/index.json
//! {channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst
//! {channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst.sigstore.json
//! ```
//!
//! ```json
//! {"packages":[{"bytes":1048576,"file":"tierstore-1.5.0-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"<64 hex>"}],"schema":1,"service":"tierstore","version":"1.5.0"}
//! ```
//!
//! The index is a lookup, not a trust anchor: it is not signed. It says which file holds the
//! package of each platform and the size and SHA-256 that file must have. The archive's own
//! attestation is what the daemon trusts, and the archive's manifest must name the service,
//! version and platform the daemon asked for, so a tampered index can at most cause a refusal.
//!
//! [`ReleaseIndex::parse`] is strict: it refuses an unknown or duplicate key, a schema other than
//! [`SCHEMA`], a service or version other than the one asked for, an empty package list, a
//! platform outside the three, a platform listed twice, packages out of platform order, a `file`
//! other than the name the paths below give, and a `sha256` that is not 64 hex digits.
//! [`ReleaseIndex::to_json`] writes the canonical form: keys sorted, packages sorted by platform,
//! no white space.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::daemon::Sha256Hex;
use crate::names::{ServiceName, Version};
use crate::platform::{Platform, UnknownPlatform};

/// The name of the index in a version's directory of the channel.
pub const INDEX_FILE: &str = "index.json";
/// The one schema this parser reads and the writer writes.
pub const SCHEMA: u64 = 1;
/// The reason token of every refusal of the index itself.
pub const INDEX_INVALID: &str = "index_invalid";
/// The reason token of an index with no package for the host's platform.
pub const NO_PACKAGE_FOR_PLATFORM: &str = "no_package_for_platform";

/// `{S}/{V}/index.json`: the index of version `version` of `service`, relative to the channel.
pub fn index_path(service: &ServiceName, version: &Version) -> String {
    format!("{service}/{version}/{INDEX_FILE}")
}

/// `{S}-{V}-{platform}.tar.zst`: the file name of a package, the `file` of its index entry.
fn package_file(service: &ServiceName, version: &Version, platform: Platform) -> String {
    format!("{service}-{version}-{platform}.tar.zst")
}

/// `{S}/{V}/{S}-{V}-{platform}.tar.zst`: the package of `platform`, relative to the channel.
pub fn package_path(service: &ServiceName, version: &Version, platform: Platform) -> String {
    format!(
        "{service}/{version}/{}",
        package_file(service, version, platform)
    )
}

/// `{S}/{V}/{S}-{V}-{platform}.tar.zst.sigstore.json`: the attestation bundle of the package of
/// `platform`, relative to the channel.
pub fn attestation_path(service: &ServiceName, version: &Version, platform: Platform) -> String {
    format!("{}.sigstore.json", package_path(service, version, platform))
}

/// Why an index was refused, or has no package for a platform. [`IndexError::reason`] is the
/// token a step record reports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    /// Not JSON, or a key missing, unknown, repeated or of the wrong type.
    #[error("the release index is not an index of schema {SCHEMA}: {0}")]
    Syntax(String),
    /// `found` is the JSON text of the `schema` value.
    #[error("the release index has schema {found}; this daemon reads schema {SCHEMA} only")]
    Schema { found: String },
    #[error("the release index is for the service {found:?}, not {expected:?}")]
    ServiceMismatch { expected: String, found: String },
    #[error("the release index is for the version {found:?}, not {expected:?}")]
    VersionMismatch { expected: String, found: String },
    #[error("the release index lists no package")]
    NoPackages,
    #[error("the release index lists a package for {0}")]
    UnknownPlatform(#[from] UnknownPlatform),
    #[error("the release index lists the platform {0} more than once")]
    DuplicatePlatform(Platform),
    #[error("the release index lists {after} after {before}; packages are sorted by platform")]
    Unsorted { before: Platform, after: Platform },
    #[error("the release index names the file {found:?} for {platform}, not {expected:?}")]
    FileName {
        platform: Platform,
        expected: String,
        found: String,
    },
    #[error("the release index's sha256 for {platform} is {found:?}, not 64 hex digits")]
    Digest { platform: Platform, found: String },
    #[error("the release index has no package for {0}")]
    NoPackageForPlatform(Platform),
}

impl IndexError {
    /// [`NO_PACKAGE_FOR_PLATFORM`] for [`IndexError::NoPackageForPlatform`], [`INDEX_INVALID`]
    /// for every other refusal.
    pub fn reason(&self) -> &'static str {
        match self {
            IndexError::NoPackageForPlatform(_) => NO_PACKAGE_FOR_PLATFORM,
            IndexError::Syntax(_)
            | IndexError::Schema { .. }
            | IndexError::ServiceMismatch { .. }
            | IndexError::VersionMismatch { .. }
            | IndexError::NoPackages
            | IndexError::UnknownPlatform(_)
            | IndexError::DuplicatePlatform(_)
            | IndexError::Unsorted { .. }
            | IndexError::FileName { .. }
            | IndexError::Digest { .. } => INDEX_INVALID,
        }
    }
}

/// The package of one platform: its size and SHA-256. Its file is the one [`package_path`]
/// names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageEntry {
    pub platform: Platform,
    pub sha256: Sha256Hex,
    pub bytes: u64,
}

/// An index that passed every check: one service and version, at least one package, each
/// platform at most once, sorted by platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIndex {
    service: ServiceName,
    version: Version,
    packages: Vec<PackageEntry>,
}

/// The first pass: the schema alone, so a document of another schema is reported as one.
#[derive(Deserialize)]
struct Probe {
    schema: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndex {
    // Judged by the probe; listed so the key is known.
    #[serde(rename = "schema")]
    _schema: serde::de::IgnoredAny,
    service: String,
    version: String,
    packages: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    platform: String,
    file: String,
    sha256: String,
    bytes: u64,
}

/// `text` as a JSON string, escaped.
fn json_string(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

impl ReleaseIndex {
    /// The index of `packages`, sorted by platform. Refuses an empty list and a platform listed
    /// twice.
    pub fn new(
        service: ServiceName,
        version: Version,
        mut packages: Vec<PackageEntry>,
    ) -> Result<ReleaseIndex, IndexError> {
        packages.sort_by_key(|entry| entry.platform);
        let mut seen = BTreeSet::new();
        if let Some(twice) = packages.iter().find(|entry| !seen.insert(entry.platform)) {
            return Err(IndexError::DuplicatePlatform(twice.platform));
        }
        if packages.is_empty() {
            return Err(IndexError::NoPackages);
        }
        Ok(ReleaseIndex {
            service,
            version,
            packages,
        })
    }

    /// Reads `text` as the index of version `version` of `service`, applying every check of the
    /// module in this order: JSON, schema, keys and types, service, version, then each package
    /// in turn (platform, order, file, sha256), then that there is one.
    pub fn parse(
        text: &str,
        service: &ServiceName,
        version: &Version,
    ) -> Result<ReleaseIndex, IndexError> {
        let syntax = |e: serde_json::Error| IndexError::Syntax(e.to_string());
        let probe: Probe = serde_json::from_str(text).map_err(syntax)?;
        if probe.schema != SCHEMA {
            return Err(IndexError::Schema {
                found: probe.schema.to_string(),
            });
        }
        let raw: RawIndex = serde_json::from_str(text).map_err(syntax)?;
        if raw.service != service.as_str() {
            return Err(IndexError::ServiceMismatch {
                expected: service.to_string(),
                found: raw.service,
            });
        }
        if raw.version != version.as_str() {
            return Err(IndexError::VersionMismatch {
                expected: version.to_string(),
                found: raw.version,
            });
        }
        let mut packages: Vec<PackageEntry> = Vec::new();
        for entry in raw.packages {
            let platform: Platform = entry.platform.parse()?;
            if let Some(before) = packages.last().map(|last| last.platform) {
                if before == platform {
                    return Err(IndexError::DuplicatePlatform(platform));
                }
                if before > platform {
                    return Err(IndexError::Unsorted {
                        before,
                        after: platform,
                    });
                }
            }
            let file = package_file(service, version, platform);
            if entry.file != file {
                return Err(IndexError::FileName {
                    platform,
                    expected: file,
                    found: entry.file,
                });
            }
            let sha256 = Sha256Hex::parse(&entry.sha256).ok_or_else(|| IndexError::Digest {
                platform,
                found: entry.sha256.clone(),
            })?;
            packages.push(PackageEntry {
                platform,
                sha256,
                bytes: entry.bytes,
            });
        }
        ReleaseIndex::new(service.clone(), version.clone(), packages)
    }

    /// The canonical JSON of the index: keys sorted, packages sorted by platform, no white space
    /// and no final newline. [`ReleaseIndex::parse`] reads it back to the same index.
    pub fn to_json(&self) -> String {
        let packages: Vec<String> = self
            .packages
            .iter()
            .map(|entry| {
                format!(
                    r#"{{"bytes":{},"file":{},"platform":{},"sha256":{}}}"#,
                    entry.bytes,
                    json_string(&package_file(&self.service, &self.version, entry.platform)),
                    json_string(entry.platform.as_str()),
                    json_string(entry.sha256.as_str()),
                )
            })
            .collect();
        format!(
            r#"{{"packages":[{}],"schema":{SCHEMA},"service":{},"version":{}}}"#,
            packages.join(","),
            json_string(self.service.as_str()),
            json_string(self.version.as_str()),
        )
    }

    /// The package of `platform`, or [`IndexError::NoPackageForPlatform`].
    pub fn entry_for(&self, platform: Platform) -> Result<&PackageEntry, IndexError> {
        self.packages
            .iter()
            .find(|entry| entry.platform == platform)
            .ok_or(IndexError::NoPackageForPlatform(platform))
    }

    pub fn service(&self) -> &ServiceName {
        &self.service
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The packages, sorted by platform, at least one.
    pub fn packages(&self) -> &[PackageEntry] {
        &self.packages
    }
}
