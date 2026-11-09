//! The files of a package's release that `cargo xtask release targets` reads, made for the run
//! tests of the workflows' jobs ([`super::targets_run_tests`], [`super::hello_run_tests`]): for
//! hostkeeper and hello, their native packages ([`native_files`]): a `.deb` per Linux
//! architecture holding only its `control`, and in place of the `.pkg` bytes of its own, each
//! with its bundle (unsigned, its statement listing the file's SHA-256); for hello's failing
//! build, its `.deb`s ([`fail_ready_files`]). And the targets the verb must write for them,
//! written here from the files themselves and the manifest each package has always carried.

use std::collections::BTreeMap;

use base64::Engine as _;
use hostkeeper_config::platform::Platform;
use serde_json::{Value, json};
use sha2::Digest;

/// The repository whose releases the run tests read.
pub const REPO: &str = "komira-ai/hostkeeper";

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

/// The targets `release targets` must write for `files` (from [`native_files`] of `service`) of
/// the release `release`: per Linux platform, the native entry alone (an `apt` entry of the
/// repository `komira-testing` pinned by the `.deb`'s SHA-256), its manifest the package's own;
/// no list for macOS, while no daemon announces `macos-pkg`. As [`sorted`] JSON text.
pub fn targets(service: &str, release: &str, files: &BTreeMap<String, Vec<u8>>) -> String {
    lists(|platform| {
        let bytes = &files[&native_name(service, release, platform)];
        native_entry(service, release, bytes, "komira-testing", release)
    })
}

/// The targets `release targets --fail-ready` must write for hello's failing build at the
/// release `release`, from `files` ([`fail_ready_files`]): per Linux platform, an `apt` entry of
/// the repository `komira-testing-fail` at the Debian version `<release>~fail.ready`, pinned by
/// the failing build's `.deb`, its manifest hello's. As [`sorted`] JSON text.
pub fn fail_ready_targets(release: &str, files: &BTreeMap<String, Vec<u8>>) -> String {
    lists(|platform| {
        let bytes = &files[&fail_ready_name(release, platform)];
        let version = format!("{release}~fail.ready");
        native_entry("hello", release, bytes, "komira-testing-fail", &version)
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

/// The native entry `release targets` must write for `service`'s `.deb` `bytes` of the release
/// `release`: the repository entry `repository` at the Debian version `package_version`.
fn native_entry(
    service: &str,
    release: &str,
    bytes: &[u8],
    repository: &str,
    package_version: &str,
) -> Value {
    json!({
        "installer": "apt",
        "digest": sha256(bytes),
        "manifest": manifest(service, release),
        "repository": repository,
        "packageVersion": package_version,
    })
}

/// The manifest of the native entry of `service` at the release `release`, as the package has
/// always carried it: hostkeeper's, with the unit file of its `.deb` and the floor of the oldest
/// release that reads every file this one writes (or the release's own version when that is
/// older), or hello's, with its two slots, its readiness URL and its params.
fn manifest(service: &str, release: &str) -> Value {
    let bounds = json!({"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"});
    if service == "hello" {
        return json!({
            "package": "hello",
            "hook": {
                "timeoutSecs": {
                    "prepare": "30", "cutover": "60", "verify": "30", "rollback": "60",
                    "cleanup": "30", "uninstall": "60",
                },
                "verifyDeadlineSecs": "120",
            },
            "run": ["hello@a.service", "hello@b.service"],
            "healthUrl": "http://127.0.0.1:9310/readyz",
            "params": {
                "port": {"integer": "9310"},
                "record_dir": {"text": "/var/lib/updater/hooks/hello"},
                "units": {"text": "systemd"},
            },
            "minHostkeeper": "0.1.0",
            "installerBounds": bounds,
        });
    }
    let oldest = hostkeeper_steps::OLDEST_READER;
    let oldest = (oldest.major, oldest.minor, oldest.patch);
    let parts: Vec<u64> = release
        .split('.')
        .map(|part| part.parse().expect("a release is X.Y.Z"))
        .collect();
    let own = (parts[0], parts[1], parts[2]);
    let floor = own.min(oldest);
    json!({
        "package": service,
        "hook": {
            "timeoutSecs": {
                "prepare": "300", "cutover": "300", "verify": "300", "rollback": "300",
                "cleanup": "300",
            },
            "verifyDeadlineSecs": "600",
        },
        "run": [format!("{service}.service")],
        "minHostkeeper": format!("{}.{}.{}", floor.0, floor.1, floor.2),
        "installerBounds": bounds,
    })
}
