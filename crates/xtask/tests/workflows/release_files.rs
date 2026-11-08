//! The files of a package's release that `cargo xtask release targets` reads, made for the run
//! tests of the workflows' jobs ([`super::channel_run_tests`], [`super::hello_run_tests`]): an
//! archive per platform holding only its `manifest.toml`, each archive's bundle (unsigned, its
//! statement listing the archive's SHA-256), and the canonical release index of the archives.
//! And the targets the verb must write for them, written here from the files themselves.

use std::collections::BTreeMap;

use base64::Engine as _;
use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::{PackageEntry, ReleaseIndex};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use serde_json::{Value, json};
use sha2::Digest;

/// The repository whose releases the run tests read.
pub const REPO: &str = "komira-ai/hostkeeper";

/// The `min_hostkeeper` each archive's manifest names.
pub const FLOOR: &str = "0.1.7";

/// The SHA-256 of `bytes`, lower-case hex.
pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// `value` with the keys of every object in sorted order, so that two JSON texts of one value
/// compare equal whatever order each wrote its keys in.
pub fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let by_key: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(by_key.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The archive of `service` at the package version `version` for `platform`: a zstd tar whose
/// one entry is the `manifest.toml` that `release targets` reads.
fn archive(service: &str, version: &str, platform: Platform) -> Vec<u8> {
    let manifest = format!(
        "name = \"{service}\"\nversion = \"{version}\"\nplatform = \"{platform}\"\nmin_hostkeeper = \"{FLOOR}\"\n"
    );
    let mut header = tar::Header::new_ustar();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_data(&mut header, "manifest.toml", manifest.as_bytes())
        .expect("append the manifest");
    let tar = builder.into_inner().expect("finish the tar");
    ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest)
}

/// An unsigned bundle whose statement lists `bytes`' SHA-256 as its one subject's.
fn bundle(bytes: &[u8]) -> Vec<u8> {
    let statement = json!({"subject": [{"digest": {"sha256": sha256(bytes)}}]});
    let payload = base64::engine::general_purpose::STANDARD.encode(statement.to_string());
    json!({"dsseEnvelope": {"payload": payload}})
        .to_string()
        .into_bytes()
}

/// The archive name of `service` at the package version `version` for `platform`.
pub fn archive_name(service: &str, version: &str, platform: Platform) -> String {
    format!("{service}-{version}-{platform}.tar.zst")
}

/// The files of the package of `service` at the package version `version`, by name: an archive
/// and its bundle per platform, and the canonical release index of the archives as
/// `index_name`.
pub fn package_files(service: &str, version: &str, index_name: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut entries = Vec::new();
    for platform in Platform::ALL {
        let name = archive_name(service, version, platform);
        let bytes = archive(service, version, platform);
        entries.push(PackageEntry {
            platform,
            sha256: Sha256Hex::parse(&sha256(&bytes)).expect("a SHA-256"),
            bytes: bytes.len() as u64,
        });
        files.insert(format!("{name}.sigstore.json"), bundle(&bytes));
        files.insert(name, bytes);
    }
    let name = ServiceName::new(service).expect("a service");
    let index = ReleaseIndex::new(name, Version::new(version).expect("a version"), entries)
        .expect("an index");
    files.insert(index_name.to_owned(), index.to_json().into_bytes());
    files
}

/// The targets `release targets` must write for `files` (from [`package_files`] of `service` at
/// the package version `version`), released under the tag `v<release>`: one
/// `hostkeeper-archive` entry per platform, the channel's path of each file first, then its
/// release asset, its size and SHA-256, its bundle's, and the manifest's `min_hostkeeper`. As
/// [`sorted`] JSON text.
pub fn targets(
    service: &str,
    version: &str,
    release: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> String {
    let asset = format!("{REPO}/releases/download/v{release}");
    let locations = |name: &str| {
        json!([
            {"store": "channel", "locator": format!("{service}/{version}/{name}")},
            {"store": "github", "locator": format!("{asset}/{name}")},
        ])
    };
    let mut lists = serde_json::Map::new();
    for platform in Platform::ALL {
        let name = archive_name(service, version, platform);
        let attestation = format!("{name}.sigstore.json");
        let entry = json!({
            "installer": "hostkeeper-archive",
            "locations": locations(&name),
            "digest": sha256(&files[&name]),
            "bytes": files[&name].len().to_string(),
            "attestation": {
                "digest": sha256(&files[&attestation]),
                "bytes": files[&attestation].len().to_string(),
                "locations": locations(&attestation),
            },
            "manifest": {"minHostkeeper": FLOOR},
        });
        lists.insert(platform.to_string(), json!({"entries": [entry]}));
    }
    sorted(Value::Object(lists)).to_string()
}
