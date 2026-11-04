//! The platforms and the release index (the CEO's ruling of the release layout): a version of a
//! service is one package per OS and CPU family, and `index.json` beside them says which file
//! serves each platform:
//!
//! ```text
//! {channel}/{S}/{V}/index.json
//! {channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst
//! {channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst.sigstore.json
//! ```
//!
//! ```json
//! {"packages":[{"bytes":N,"file":"S-V-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"<64 hex>"}],
//!  "schema":1,"service":"S","version":"V"}
//! ```
//!
//! The index is read strictly ([`ReleaseIndex::parse`]): exactly these fields, schema 1, the
//! service and version asked for, each file named `{S}-{V}-{platform}.tar.zst`, each digest 64
//! lower-case hex digits, at least one package, each platform once and in order. It is a lookup,
//! not a trust anchor, and is not signed: the fetch checks the size and the SHA-256 of the file
//! it names, the proof checks the archive's own attestation, and the manifest step checks that
//! the archive names the service, version and platform asked for, so a tampered index can only
//! cause a refusal.
//!
//! These types are held here under the names the shared `hostkeeper-config` types take; this
//! crate switches to those when they land.

use serde_json::{Map, Value};

use hostkeeper_config::daemon::{HttpsUrl, Sha256Hex};
use hostkeeper_config::names::{ServiceName, Version};

use crate::ports::Url;

/// The name of the release index in a version's directory.
pub const INDEX_FILE: &str = "index.json";
/// The name every package ends in. The compression is design Q10, decided as zstd; this crate
/// never decompresses, so the suffix is all it fixes.
pub const ARCHIVE_SUFFIX: &str = ".tar.zst";
/// The most bytes of a release index.
pub const MAX_INDEX_BYTES: u64 = 64 * 1024;
/// The one schema of the index.
pub const SCHEMA: u64 = 1;

/// The fields of the index and of each of its packages, in order: nothing else is read.
const FIELDS: [&str; 4] = ["packages", "schema", "service", "version"];
const PACKAGE_FIELDS: [&str; 4] = ["bytes", "file", "platform", "sha256"];

/// An OS and CPU family. The set is closed; a daemon knows its own from the target it was
/// compiled for ([`Platform::current`]), never from a file or the running system. The
/// instruction-set levels inside a family are the package's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    LinuxX86_64,
    LinuxArm64,
    MacosArm64,
}

impl Platform {
    /// Every platform, in the order of their names.
    pub const ALL: [Platform; 3] = [
        Platform::LinuxArm64,
        Platform::LinuxX86_64,
        Platform::MacosArm64,
    ];

    /// The platform's name, in file names, the index and a manifest.
    pub const fn name(self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::MacosArm64 => "macos-arm64",
        }
    }

    /// The platform `name` names, none for any other text.
    pub fn of_name(name: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The platform this program was compiled for.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub const fn current() -> Platform {
        Platform::LinuxX86_64
    }

    /// The platform this program was compiled for.
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    pub const fn current() -> Platform {
        Platform::LinuxArm64
    }

    /// The platform this program was compiled for.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub const fn current() -> Platform {
        Platform::MacosArm64
    }
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
compile_error!("hostkeeper runs on linux-x86_64, linux-arm64 and macos-arm64 only");

/// One package of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub platform: Platform,
    /// `{S}-{V}-{platform}.tar.zst`, beside the index.
    pub file: String,
    pub sha256: Sha256Hex,
    /// The size of the file.
    pub bytes: u64,
}

/// The release index of one version of one service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIndex {
    pub service: ServiceName,
    pub version: Version,
    /// Sorted by platform name, each platform once, at least one.
    pub packages: Vec<IndexEntry>,
}

/// Why a text is not the release index asked for (the refusal `index_invalid`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("index_invalid: {0}")]
pub struct IndexInvalid(pub String);

/// The URL of the directory of `service` at `version`: `<channel>/<service>/<version>`. One `/`
/// at the end of the channel is dropped; nothing else is changed. The grammars of a service name
/// and a version allow no `/`, so neither can add a segment.
fn version_dir(channel: &HttpsUrl, service: &ServiceName, version: &Version) -> String {
    let base = channel.as_str();
    let base = base.strip_suffix('/').unwrap_or(base);
    format!("{base}/{service}/{version}")
}

/// The URL of the release index of `service` at `version`.
pub fn index_url(channel: &HttpsUrl, service: &ServiceName, version: &Version) -> Url {
    Url::new(format!(
        "{}/{INDEX_FILE}",
        version_dir(channel, service, version)
    ))
}

/// The file name of the package of `service` at `version` for `platform`.
pub fn package_file(service: &ServiceName, version: &Version, platform: Platform) -> String {
    format!("{service}-{version}-{}{ARCHIVE_SUFFIX}", platform.name())
}

/// The URL of the package of `service` at `version` for `platform`. The one place it is built.
pub fn package_url(
    channel: &HttpsUrl,
    service: &ServiceName,
    version: &Version,
    platform: Platform,
) -> Url {
    Url::new(format!(
        "{}/{}",
        version_dir(channel, service, version),
        package_file(service, version, platform)
    ))
}

fn invalid(why: String) -> IndexInvalid {
    IndexInvalid(why)
}

/// `value` as an object with exactly the fields `fields`; `what` names it.
fn object<'v>(
    value: &'v Value,
    fields: &[&str],
    what: &str,
) -> Result<&'v Map<String, Value>, IndexInvalid> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid(format!("{what} is not a JSON object")))?;
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    (keys == fields).then_some(object).ok_or_else(|| {
        invalid(format!(
            "{what} has the fields {keys:?}, not exactly {fields:?}"
        ))
    })
}

/// The text of the field `key` of `object`.
fn text<'v>(
    object: &'v Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'v str, IndexInvalid> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{what}: {key} is not a string")))
}

/// Whether `text` is 64 lower-case hex digits.
fn is_sha256(text: &str) -> bool {
    (text.len() == 64) & text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The `n`th package entry of the index of `service` at `version`.
fn entry(
    value: &Value,
    n: usize,
    service: &ServiceName,
    version: &Version,
) -> Result<IndexEntry, IndexInvalid> {
    let what = format!("packages[{n}]");
    let object = object(value, &PACKAGE_FIELDS, &what)?;
    let name = text(object, "platform", &what)?;
    let platform = Platform::of_name(name)
        .ok_or_else(|| invalid(format!("{what}: {name:?} is not a platform")))?;
    let file = text(object, "file", &what)?;
    let expected = package_file(service, version, platform);
    if file != expected {
        return Err(invalid(format!(
            "{what}: the file is {file:?}, not {expected:?}"
        )));
    }
    let sha256 = text(object, "sha256", &what)?;
    let sha256 = Sha256Hex::parse(sha256)
        .filter(|_| is_sha256(sha256))
        .ok_or_else(|| {
            invalid(format!(
                "{what}: {sha256:?} is not a SHA-256 (64 lower-case hex digits)"
            ))
        })?;
    let bytes = object
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{what}: bytes is not a whole number")))?;
    Ok(IndexEntry {
        platform,
        file: expected,
        sha256,
        bytes,
    })
}

impl ReleaseIndex {
    /// Reads `bytes` as the index of `service` at `version`, refusing anything that is not
    /// exactly one: not JSON, other or missing fields, another schema, service or version, a
    /// file name that is not the package's, a digest that is not a SHA-256, no package, or the
    /// platforms not in strictly increasing order (which refuses a platform listed twice).
    pub fn parse(
        bytes: &[u8],
        service: &ServiceName,
        version: &Version,
    ) -> Result<ReleaseIndex, IndexInvalid> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| invalid(format!("not JSON: {e}")))?;
        let object = object(&value, &FIELDS, "the index")?;
        let schema = object.get("schema").and_then(Value::as_u64);
        if schema != Some(SCHEMA) {
            return Err(invalid(format!("the schema is not {SCHEMA}")));
        }
        let found = (
            text(object, "service", "the index")?,
            text(object, "version", "the index")?,
        );
        if (found.0 != service.as_str()) | (found.1 != version.as_str()) {
            return Err(invalid(format!(
                "the index is of {} {}, not {service} {version}",
                found.0, found.1
            )));
        }
        let list = object
            .get("packages")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("packages is not a list".to_owned()))?;
        let packages = list
            .iter()
            .enumerate()
            .map(|(n, value)| entry(value, n, service, version))
            .collect::<Result<Vec<_>, _>>()?;
        let names: Vec<&str> = packages.iter().map(|e| e.platform.name()).collect();
        let increasing = names.windows(2).all(|pair| pair[0] < pair[1]);
        if names.is_empty() | !increasing {
            return Err(invalid(format!(
                "the platforms {names:?} are not one or more, each once, in order"
            )));
        }
        Ok(ReleaseIndex {
            service: service.clone(),
            version: version.clone(),
            packages,
        })
    }

    /// The package for `platform`, if the index lists one.
    pub fn package(&self, platform: Platform) -> Option<&IndexEntry> {
        self.packages.iter().find(|e| e.platform == platform)
    }
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
