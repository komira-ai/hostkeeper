//! The files of a package's release that `cargo xtask release targets` reads, made for the run
//! tests of the workflows' jobs ([`super::targets_run_tests`], [`super::hello_run_tests`]): an
//! archive per platform holding only its `manifest.toml`, each archive's bundle (unsigned, its
//! statement listing the archive's SHA-256), and the canonical release index of the archives;
//! for hostkeeper and hello, their native packages ([`native_files`]): a `.deb` per Linux
//! architecture holding only its `control`, and in place of the `.pkg` bytes of its own, each
//! with its bundle; for hello's failing build, its `.deb`s ([`fail_ready_files`]). And
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

/// The failing build of hello at the release `release`, by name: its `.deb` per Linux
/// architecture, named for the service version (`hello_<release>-fail-ready_<arch>.deb`) and
/// holding the Debian version `<release>~fail.ready`, each with its bundle. Its `.pkg` is not
/// released.
pub fn fail_ready_files(release: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
        let name = fail_ready_name(release, platform);
        let arch = if platform == Platform::LinuxX86_64 {
            "amd64"
        } else {
            "arm64"
        };
        let bytes = deb("hello", &format!("{release}~fail.ready"), arch);
        files.insert(format!("{name}.sigstore.json"), bundle(&bytes));
        files.insert(name, bytes);
    }
    files
}

/// The release asset name of hello's failing build at `release` on the Linux `platform`.
fn fail_ready_name(release: &str, platform: Platform) -> String {
    native_name("hello", &format!("{release}-fail-ready"), platform)
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

/// The targets `release targets` must write for `files` (from [`package_files`] of `service` and
/// [`native_files`]) of the release `release`: per Linux platform, the native entry alone (an
/// `apt` entry of the repository `komira-testing` pinned by the `.deb`'s SHA-256), its manifest
/// the archive's; no list for macOS, while no daemon announces `macos-pkg`. As [`sorted`] JSON
/// text.
pub fn targets(service: &str, release: &str, files: &BTreeMap<String, Vec<u8>>) -> String {
    lists(|platform| {
        let bytes = &files[&native_name(service, release, platform)];
        native_entry(service, platform, bytes, "komira-testing", release)
    })
}

/// The targets `release targets --fail-ready` must write for hello's failing build at the
/// release `release`, from `files` ([`fail_ready_files`]): per Linux platform, an `apt` entry of
/// the repository `komira-testing-fail` at the Debian version `<release>~fail.ready`, pinned by
/// the failing build's `.deb`. As [`sorted`] JSON text.
pub fn fail_ready_targets(release: &str, files: &BTreeMap<String, Vec<u8>>) -> String {
    lists(|platform| {
        let bytes = &files[&fail_ready_name(release, platform)];
        let version = format!("{release}~fail.ready");
        native_entry("hello", platform, bytes, "komira-testing-fail", &version)
    })
}

/// The lists of the Linux platforms, each the one entry `entry` makes, as [`sorted`] JSON text.
fn lists(entry: impl Fn(Platform) -> Value) -> String {
    let mut lists = serde_json::Map::new();
    for platform in [Platform::LinuxArm64, Platform::LinuxX86_64] {
        lists.insert(platform.to_string(), json!({"entries": [entry(platform)]}));
    }
    sorted(Value::Object(lists)).to_string()
}

/// The native entry `release targets` must write for `service`'s `.deb` `bytes` on the Linux
/// `platform`: the repository entry `repository` at the Debian version `package_version`.
fn native_entry(
    service: &str,
    platform: Platform,
    bytes: &[u8],
    repository: &str,
    package_version: &str,
) -> Value {
    json!({
        "installer": "apt",
        "digest": sha256(bytes),
        "manifest": {
            "package": service,
            "hook": {
                "timeoutSecs": {"cutover": "60", "verify": "30", "rollback": "60", "cleanup": "30"},
                "verifyDeadlineSecs": "120",
            },
            "run": [run_unit(service, platform)],
            "minHostkeeper": FLOOR,
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        },
        "repository": repository,
        "packageVersion": package_version,
    })
}
