//! The release index: which package file serves each platform of one version of a service. It
//! sits beside the packages in the channel (`{channel}/{S}/{V}/index.json`, with
//! `{S}-{V}-{platform}.tar.zst` and its `.sigstore.json` bundle) and is a release asset of
//! hostkeeper itself. A release holds two: `index.json` of the hostkeeper packages and
//! `hello-index.json` of the hello packages ([`Service::index_name`]), the same schema with
//! `service` `hostkeeper` or `hello`:
//!
//! ```json
//! {"packages":[{"bytes":N,"file":"S-V-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"<64 hex>"}],
//!  "schema":1,"service":"S","version":"V"}
//! ```
//!
//! It is written canonically ([`ReleaseIndex::to_json`]: keys sorted, no white space, packages
//! sorted by platform) and read strictly ([`ReleaseIndex::parse`]: exactly these fields, schema
//! 1, the service and version asked for, each platform once and in order, at least one). The
//! index is a lookup, not a trust anchor, and is not signed: the daemon checks the size and the
//! SHA-256 of the file it names, then the archive's own attestation and its manifest, so a
//! tampered index can only cause a refusal.
//!
//! This module holds the type locally under the name the shared `hostkeeper-config` type takes;
//! the release tooling switches to that type when it lands.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::contract::{PACKAGE_SUFFIX, Platform, Service, Version};
use super::{Error, file_sha256};

/// The one schema of the index.
pub(crate) const SCHEMA: u64 = 1;

/// The fields of the index, and of each of its packages: nothing else is read.
const FIELDS: [&str; 4] = ["packages", "schema", "service", "version"];
const PACKAGE_FIELDS: [&str; 4] = ["bytes", "file", "platform", "sha256"];

/// One package of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub(crate) platform: Platform,
    /// `{S}-{V}-{platform}.tar.zst`, in the same directory as the index.
    pub(crate) file: String,
    /// The SHA-256 of the file, 64 lowercase hex digits.
    pub(crate) sha256: String,
    /// The size of the file.
    pub(crate) bytes: u64,
}

/// The release index of one version of one service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseIndex {
    pub(crate) service: String,
    pub(crate) version: Version,
    /// Sorted by platform name, each platform once, at least one.
    pub(crate) packages: Vec<IndexEntry>,
}

/// Why a text is not the release index asked for (the daemon's refusal `index_invalid`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("index_invalid: {0}")]
pub(crate) struct IndexInvalid(pub(crate) String);

/// The file name of the package of `service` at `version` for `platform`.
pub(crate) fn package_file(service: &str, version: &Version, platform: Platform) -> String {
    format!("{service}-{version}-{}{PACKAGE_SUFFIX}", platform.name())
}

/// `value` as an object with exactly the fields `fields`, or why not; `what` names it.
fn object<'v>(
    value: &'v Value,
    fields: &[&str],
    what: &str,
) -> Result<&'v Map<String, Value>, IndexInvalid> {
    let object = value
        .as_object()
        .ok_or_else(|| IndexInvalid(format!("{what} is not a JSON object")))?;
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    if keys != fields {
        return Err(IndexInvalid(format!(
            "{what} has the fields {keys:?}, not exactly {fields:?}"
        )));
    }
    Ok(object)
}

/// The text of the field `key` of `object`, or why it has none.
fn text<'v>(
    object: &'v Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'v str, IndexInvalid> {
    object[key]
        .as_str()
        .ok_or_else(|| IndexInvalid(format!("{what}: {key} is not a string")))
}

/// Whether `text` is 64 lowercase hex digits.
fn is_sha256(text: &str) -> bool {
    (text.len() == 64) & text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The package entry `value`, the `n`th of the index of `service` at `version`.
fn entry(
    value: &Value,
    n: usize,
    service: &str,
    version: &Version,
) -> Result<IndexEntry, IndexInvalid> {
    let what = format!("packages[{n}]");
    let object = object(value, &PACKAGE_FIELDS, &what)?;
    let name = text(object, "platform", &what)?;
    let platform = Platform::of_name(name)
        .ok_or_else(|| IndexInvalid(format!("{what}: {name:?} is not a platform")))?;
    let file = text(object, "file", &what)?;
    let expected = package_file(service, version, platform);
    if file != expected {
        return Err(IndexInvalid(format!(
            "{what}: the file is {file:?}, not {expected:?}"
        )));
    }
    let sha256 = text(object, "sha256", &what)?;
    if !is_sha256(sha256) {
        return Err(IndexInvalid(format!(
            "{what}: {sha256:?} is not a SHA-256 (64 lowercase hex digits)"
        )));
    }
    let bytes = object["bytes"]
        .as_u64()
        .ok_or_else(|| IndexInvalid(format!("{what}: bytes is not a whole number")))?;
    Ok(IndexEntry {
        platform,
        file: file.to_owned(),
        sha256: sha256.to_owned(),
        bytes,
    })
}

impl ReleaseIndex {
    /// The canonical text of the index: compact JSON with every object's keys sorted (the
    /// `serde_json` map is ordered by key) and the packages in the order they are held.
    pub(crate) fn to_json(&self) -> String {
        let packages: Vec<Value> = self
            .packages
            .iter()
            .map(|e| {
                json!({
                    "bytes": e.bytes,
                    "file": e.file,
                    "platform": e.platform.name(),
                    "sha256": e.sha256,
                })
            })
            .collect();
        json!({
            "packages": packages,
            "schema": SCHEMA,
            "service": self.service,
            "version": self.version.to_string(),
        })
        .to_string()
    }

    /// Reads `text` as the index of `service` at `version`, refusing anything that is not
    /// exactly one: other or missing fields, another schema, service or version, a file name
    /// that is not the package's, a digest that is not a SHA-256, no package, or the platforms
    /// not in strictly increasing order (which refuses a platform listed twice).
    pub(crate) fn parse(
        text_of_index: &str,
        service: &str,
        version: &Version,
    ) -> Result<ReleaseIndex, IndexInvalid> {
        let value: Value = serde_json::from_str(text_of_index)
            .map_err(|e| IndexInvalid(format!("not JSON: {e}")))?;
        let object = object(&value, &FIELDS, "the index")?;
        if object["schema"].as_u64() != Some(SCHEMA) {
            return Err(IndexInvalid(format!(
                "the schema is {}, not {SCHEMA}",
                object["schema"]
            )));
        }
        let found = (
            text(object, "service", "the index")?,
            text(object, "version", "the index")?,
        );
        let wanted = (service, version.to_string());
        if (found.0 != wanted.0) | (found.1 != wanted.1) {
            return Err(IndexInvalid(format!(
                "the index is of {} {}, not {} {}",
                found.0, found.1, wanted.0, wanted.1
            )));
        }
        let list = object["packages"]
            .as_array()
            .ok_or_else(|| IndexInvalid("packages is not a list".to_owned()))?;
        let packages = list
            .iter()
            .enumerate()
            .map(|(n, v)| entry(v, n, service, version))
            .collect::<Result<Vec<_>, _>>()?;
        let names: Vec<&str> = packages.iter().map(|e| e.platform.name()).collect();
        let increasing = names.windows(2).all(|w| w[0] < w[1]);
        if names.is_empty() | !increasing {
            return Err(IndexInvalid(format!(
                "the platforms {names:?} are not one or more, each once, in order"
            )));
        }
        Ok(ReleaseIndex {
            service: service.to_owned(),
            version: *version,
            packages,
        })
    }

    /// The index of the packages of `service` at `version` in `dir`, for every platform of
    /// `platforms` whose package file is there, sorted by platform name. None there is a
    /// refusal.
    pub(crate) fn of_dir(
        dir: &Path,
        service: &str,
        version: &Version,
        platforms: &[Platform],
    ) -> Result<ReleaseIndex, Error> {
        let mut sorted = platforms.to_vec();
        sorted.sort_by_key(|p| p.name());
        sorted.dedup();
        let mut packages = Vec::new();
        for platform in sorted {
            let file = package_file(service, version, platform);
            let path = dir.join(&file);
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::metadata(&path)
                .map_err(Error::io(format!("read {}", path.display())))?
                .len();
            packages.push(IndexEntry {
                platform,
                sha256: file_sha256(&path)?,
                file,
                bytes,
            });
        }
        if packages.is_empty() {
            return Err(Error::Failed(format!(
                "{} holds no package of {service} {version}",
                dir.display()
            )));
        }
        Ok(ReleaseIndex {
            service: service.to_owned(),
            version: *version,
            packages,
        })
    }
}

/// The `release index --service S --version V --dir D` verb: writes `D/<index name of S>`, the
/// index of the packages of `service` at `version` in `dir`, for every platform whose package is
/// there.
pub(crate) fn write_index(
    dir: &Path,
    service: Service,
    version: &Version,
) -> Result<Vec<String>, Error> {
    let index = ReleaseIndex::of_dir(dir, service.name(), version, &Platform::ALL)?;
    let path = dir.join(service.index_name());
    std::fs::write(&path, index.to_json())
        .map_err(Error::io(format!("write {}", path.display())))?;
    let platforms: Vec<&str> = index.packages.iter().map(|e| e.platform.name()).collect();
    Ok(vec![format!(
        "{}: the packages of {} {version} for {}",
        path.display(),
        service.name(),
        platforms.join(", ")
    )])
}

#[cfg(test)]
mod tests;
