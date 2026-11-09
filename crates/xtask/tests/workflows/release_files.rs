//! The files of a package's release that `cargo xtask release targets` reads, made for the run
//! tests of the workflows' jobs ([`super::channel_run_tests`], [`super::hello_run_tests`]): an
//! archive per platform holding only its `manifest.toml`, each archive's bundle (unsigned, its
//! statement listing the archive's SHA-256), and the canonical release index of the archives;
//! for hostkeeper and hello, their native packages ([`native_files`]): a `.deb` per Linux
//! architecture holding only its `control`, and in place of the `.pkg` bytes of its own, each
//! with its bundle. And
//! the targets the verb must write for them, written here from the files themselves.

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

/// The unit (on Linux) or job (on macOS) of the slot `a` of `service` on `platform`, which the
/// archive's manifest runs.
fn run_unit(service: &str, platform: Platform) -> String {
    match platform {
        Platform::MacosArm64 => format!("{service}.a"),
        _ => format!("{service}@a.service"),
    }
}

/// The archive of `service` at the package version `version` for `platform`: a zstd tar whose
/// one entry is the `manifest.toml` that `release targets` reads, with the hook and the run unit
/// a native entry's manifest is made of.
fn archive(service: &str, version: &str, platform: Platform) -> Vec<u8> {
    let run = run_unit(service, platform);
    let manifest = format!(
        "name = \"{service}\"\nversion = \"{version}\"\nplatform = \"{platform}\"\nmin_hostkeeper = \"{FLOOR}\"\n\n\
         [hook]\ncommand = [\"bin/{service}\", \"update-hook\"]\nverify_deadline_secs = 120\n\n\
         [hook.timeout_secs]\ncutover = 60\nverify = 30\nrollback = 60\ncleanup = 30\n\n\
         [units]\nrun = [\"{run}\"]\n"
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

/// The native package of `service` at `version` for `platform`: `<service>_<v>_<arch>.deb` on
/// Linux, `<service>-<v>-macos-arm64.pkg` on macOS.
pub fn native_name(service: &str, version: &str, platform: Platform) -> String {
    match platform {
        Platform::LinuxX86_64 => format!("{service}_{version}_amd64.deb"),
        Platform::LinuxArm64 => format!("{service}_{version}_arm64.deb"),
        Platform::MacosArm64 => format!("{service}-{version}-macos-arm64.pkg"),
    }
}

/// A `.deb` of `service` at `version` for `arch` with nothing but its `control`: the `ar`
/// archive whose `control.tar.gz` holds `./control`, all `release targets` reads of it.
fn deb(service: &str, version: &str, arch: &str) -> Vec<u8> {
    let control = format!("Package: {service}\nVersion: {version}\nArchitecture: {arch}\n");
    let mut header = tar::Header::new_ustar();
    header.set_size(control.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_data(&mut header, "./control", control.as_bytes())
        .expect("append the control");
    let tar = builder.into_inner().expect("finish the tar");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar).expect("gzip the tar");
    let gz = gz.finish().expect("finish the gzip");
    let mut ar = b"!<arch>\n".to_vec();
    let header = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "control.tar.gz",
        0,
        0,
        0,
        644,
        gz.len()
    );
    ar.extend(header.as_bytes());
    ar.extend(&gz);
    if gz.len() % 2 == 1 {
        ar.push(b'\n');
    }
    ar
}

/// The native packages of `service` at the release `version`, by name: a `.deb` per Linux
/// architecture and the `.pkg`, each with its bundle.
pub fn native_files(service: &str, version: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for platform in Platform::ALL {
        let name = native_name(service, version, platform);
        let bytes = match platform {
            Platform::LinuxX86_64 => deb(service, version, "amd64"),
            Platform::LinuxArm64 => deb(service, version, "arm64"),
            Platform::MacosArm64 => format!("the .pkg of {service}").into_bytes(),
        };
        files.insert(format!("{name}.sigstore.json"), bundle(&bytes));
        files.insert(name, bytes);
    }
    files
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
/// the package version `version`, and with `native` [`native_files`] too), released under the tag
/// `v<release>`: per platform, with `native` the native entry first (an `apt` entry of the
/// repository `komira-testing` pinned by the `.deb`'s SHA-256, or a `macos-pkg` entry of the
/// store `komira-testing` at the installers' testing path), its manifest the archive's; then one
/// `hostkeeper-archive` entry, the channel's path of each file first, then its release asset,
/// its size and SHA-256, its bundle's, and the manifest's `min_hostkeeper`. As [`sorted`] JSON
/// text.
pub fn targets(
    service: &str,
    version: &str,
    release: &str,
    files: &BTreeMap<String, Vec<u8>>,
    native: bool,
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
        let mut entries = Vec::new();
        if native {
            entries.push(native_entry(service, release, platform, files));
        }
        entries.push(entry);
        lists.insert(platform.to_string(), json!({"entries": entries}));
    }
    sorted(Value::Object(lists)).to_string()
}

/// The native entry `release targets` must write for `service`'s native package of `files` at
/// the release `release` on `platform`.
fn native_entry(
    service: &str,
    release: &str,
    platform: Platform,
    files: &BTreeMap<String, Vec<u8>>,
) -> Value {
    let name = native_name(service, release, platform);
    let bytes = &files[&name];
    let package = match platform {
        Platform::MacosArm64 => format!("dev.hostkeeper.{service}"),
        _ => service.to_owned(),
    };
    let manifest = json!({
        "package": package,
        "hook": {
            "timeoutSecs": {"cutover": "60", "verify": "30", "rollback": "60", "cleanup": "30"},
            "verifyDeadlineSecs": "120",
        },
        "run": [run_unit(service, platform)],
        "minHostkeeper": FLOOR,
        "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
    });
    match platform {
        Platform::MacosArm64 => json!({
            "installer": "macos-pkg",
            "locations": [{"store": "komira-testing", "locator": format!("hostkeeper/{release}/{name}")}],
            "digest": sha256(bytes),
            "bytes": bytes.len().to_string(),
            "manifest": manifest,
        }),
        _ => json!({
            "installer": "apt",
            "digest": sha256(bytes),
            "manifest": manifest,
            "repository": "komira-testing",
            "packageVersion": release,
        }),
    }
}
