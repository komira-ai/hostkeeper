use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::Parser;
use hostkeeper_config::index::ReleaseIndex;
use prost_reflect::DeserializeOptions;
use serde_json::json;

use super::*;
use crate::release::contract::{Commit, Service};
use crate::release::fakes::{FakeHistory, Fakes, scratch};
use crate::release::native::{deb as native_deb, hello as native_hello};
use crate::release::package::{self, OLDEST_READER};
use crate::release::ports::Adapters;
use crate::release::{Verb, file_sha256, index};
use crate::testdir::TestDir;

const REPO: &str = "komira-ai/hostkeeper";
const UNIT: &str = include_str!("../../../tests/release/hostkeeper.service");

fn release() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A scratch directory and the release's asset directory in it.
fn dist(name: &str) -> (TestDir, PathBuf) {
    let root = scratch(name);
    let dist = root.join("dist");
    std::fs::create_dir(&dist).expect("make dist/");
    (root, dist)
}

/// Builds `package`'s archive for `platform` into `dist` as `release package` does.
fn build(dist: &Path, package: Package, platform: Platform) {
    let root = dist.parent().expect("the scratch directory");
    std::fs::write(root.join("binary"), b"a program").expect("write the binary");
    std::fs::write(root.join("unit"), UNIT).expect("write the unit");
    std::fs::write(root.join("root.json"), b"{}\n").expect("write the trusted root");
    let hostkeeper = package == Package::Hostkeeper;
    let request = package::Request {
        service: package.service(),
        platform,
        version: release(),
        commit: "0".repeat(40).parse::<Commit>().expect("a commit"),
        binary: root.join("binary"),
        unit: (hostkeeper & (platform != Platform::MacosArm64)).then(|| root.join("unit")),
        fail_ready: package == Package::HelloFailReady,
        trusted_root: hostkeeper.then(|| root.join("root.json")),
        archives: None,
        out: dist.to_path_buf(),
    };
    package::package_dated(&request, || Ok(1_800_000_000)).expect("build the package");
}

/// An unsigned bundle whose statement lists `digest` as its one subject's SHA-256.
fn bundle_listing(digest: &str) -> Vec<u8> {
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": "package", "digest": {"sha256": digest}}],
    });
    json!({"dsseEnvelope": {"payload": STANDARD.encode(statement.to_string())}})
        .to_string()
        .into_bytes()
}

/// Writes beside each archive of `package` in `dist` a bundle listing its SHA-256, then the
/// package's index.
fn attest_and_index(dist: &Path, package: Package) {
    for platform in package.platforms() {
        let name = package.package_name(&release(), *platform);
        let digest = file_sha256(&dist.join(&name)).expect("hash the archive");
        std::fs::write(dist.join(bundle_of(&name)), bundle_listing(&digest))
            .expect("write the bundle");
    }
    index::write_index(dist, package, &release()).expect("write the index");
}

/// The release of `package`, every archive built as the release builds it, and for hostkeeper,
/// hello and mdm their native packages ([`natives`]).
fn built(dist: &Path, package: Package) {
    for platform in package.platforms() {
        build(dist, package, *platform);
    }
    attest_and_index(dist, package);
    if package
        .native_name(&release(), Platform::LinuxX86_64)
        .is_some()
    {
        natives(dist, package);
    }
}

/// Writes each native package of `package` into `dist` with a bundle listing its SHA-256: a
/// `.deb` as `release deb` builds it (hello's files; hostkeeper's and mdm's with their program
/// alone, as nfpm's staged files and mdm's pinned servers are not at hand), and in place of the
/// `.pkg`, which only a Mac builds, bytes of its own.
fn natives(dist: &Path, package: Package) {
    let service = package.service().name();
    for platform in package.platforms() {
        // The failing build has no `.pkg`.
        let Some(name) = package.native_name(&release(), *platform) else {
            continue;
        };
        let bytes = match native_deb::arch(*platform) {
            Some(arch) => {
                let program = |path: &str| {
                    vec![native_hello::File {
                        path: path.to_owned(),
                        mode: 0o755,
                        data: b"a program".to_vec(),
                    }]
                };
                let files = match package {
                    Package::Hello => native_hello::deb_files(b"a program".to_vec(), false),
                    Package::HelloFailReady => native_hello::deb_files(b"a program".to_vec(), true),
                    Package::Hostkeeper => program("/usr/bin/hostkeeper"),
                    _ => program("/usr/lib/mdm/mdm"),
                };
                let version = package.deb_version(&release());
                native_deb::build(service, &version, arch, "a service", &files, 1_800_000_000)
                    .expect("build the .deb")
            }
            None => format!("the .pkg of {service}").into_bytes(),
        };
        std::fs::write(dist.join(&name), &bytes).expect("write the native package");
        let digest = file_sha256(&dist.join(&name)).expect("hash the native package");
        std::fs::write(dist.join(bundle_of(&name)), bundle_listing(&digest))
            .expect("write its bundle");
    }
}

/// The hook bounds, units, health URL and params of hello's archive manifest for `units`
/// (`systemd` or `launchd`), as a native entry's manifest names them in JSON.
fn hello_manifest(package: &str, run: [&str; 2], units: &str) -> Value {
    json!({
        "package": package,
        "hook": {
            "command": ["/usr/lib/hello/hello", "update-hook"],
            "timeoutSecs": {
                "prepare": "30", "cutover": "60", "verify": "30", "rollback": "60",
                "cleanup": "30", "uninstall": "60",
            },
            "verifyDeadlineSecs": "120",
        },
        "run": run,
        "healthUrl": "http://127.0.0.1:9310/readyz",
        "params": {
            "port": {"integer": "9310"},
            "record_dir": {"text": "/var/lib/updater/hooks/hello"},
            "units": {"text": units},
        },
        "minHostkeeper": "0.1.0",
        "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
    })
}

/// The native entry the verb must write for hello's `.deb` of `dist` on the Linux `platform`.
fn expected_native(dist: &Path, platform: Platform) -> Value {
    let name = Package::Hello
        .native_name(&release(), platform)
        .expect("a native package");
    let digest = file_sha256(&dist.join(&name)).expect("hash");
    json!({
        "installer": "apt",
        "digest": digest,
        "manifest": hello_manifest("hello", ["hello@a.service", "hello@b.service"], "systemd"),
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    })
}

/// The list of `native` alone, as the verb writes every platform's.
fn alone(native: Value) -> Value {
    json!({"entries": [native]})
}

fn request(dist: &Path, package: Package, output: Option<PathBuf>) -> Request {
    Request {
        package,
        version: release(),
        dir: dist.to_path_buf(),
        output,
    }
}

/// The JSON of the `targets=` line the verb answered.
fn answered(lines: &[String]) -> Value {
    let [line] = lines else {
        panic!("not one line: {lines:?}");
    };
    let json = line.strip_prefix("targets=").expect("a targets= line");
    serde_json::from_str(json).expect("JSON")
}

/// Reads `targets` back as the coordinator does (proto3 JSON, no unknown field), checks them by
/// its rules for `service`, and holds every platform's list to one `apt` entry: no
/// `hostkeeper-archive` entry, which a release no longer lists, and no list for macOS.
fn read_back_as_the_coordinator(service: &str, targets: &Value) {
    let descriptor = TARGET_LIST.clone().expect("the descriptor");
    let options = DeserializeOptions::new().deny_unknown_fields(true);
    let mut lists = BTreeMap::new();
    let object = targets.as_object().expect("an object");
    assert!(!object.contains_key("macos-arm64"), "{targets}");
    for (platform, list) in object {
        let read: TargetList =
            DynamicMessage::deserialize_with_options(descriptor.clone(), list.clone(), &options)
                .expect("proto3 JSON")
                .transcode_to()
                .expect("a TargetList");
        let installers: Vec<&str> = read.entries.iter().map(|e| e.installer.as_str()).collect();
        assert_eq!(installers, ["apt"], "{platform}");
        lists.insert(platform.clone(), read);
    }
    let service = ServiceName::new(service).expect("a service");
    target::check(&service, &lists).expect("the coordinator's checks");
}

/// The native entry the verb must write for hostkeeper's own `.deb` on the Linux `platform` of
/// `dist`: the daemon's `.deb` from the testing suite, with the archive's hook bounds, its run
/// unit, and its floor `floor`.
fn expected_hostkeeper_native(dist: &Path, platform: Platform, floor: &str) -> Value {
    let name = Package::Hostkeeper
        .native_name(&release(), platform)
        .expect("a native package");
    json!({
        "installer": "apt",
        "digest": file_sha256(&dist.join(&name)).expect("hash"),
        "manifest": {
            "package": "hostkeeper",
            "hook": {
                "timeoutSecs": {
                    "prepare": "300", "cutover": "300", "verify": "300", "rollback": "300",
                    "cleanup": "300",
                },
                "verifyDeadlineSecs": "600",
            },
            "run": ["hostkeeper.service"],
            "minHostkeeper": floor,
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        },
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    })
}

#[test]
fn hostkeeper_lists_its_deb_alone_on_linux_and_nothing_for_macos() {
    // Catches: hostkeeper's list with its archive entry still in it (stage 9 sets native entries
    // alone, and the channel that serves the archive ends) or without its native entry, a list
    // for macOS (no daemon announces `macos-pkg`), a `.deb` entry that is not a repository entry
    // of the testing suite pinned by the daemon's `.deb`'s own SHA-256 at the release's version,
    // a native manifest with another floor than the archive's (`OLDEST_READER`), a hook command
    // (its daemon runs built-in steps), or no run unit.
    let (_root, dist) = dist("targets-hostkeeper");
    built(&dist, Package::Hostkeeper);
    let floor = release().min(OLDEST_READER).to_string();
    let lines = targets(&request(&dist, Package::Hostkeeper, None)).expect("the targets");
    let got = answered(&lines);
    let entry = |platform: Platform| alone(expected_hostkeeper_native(&dist, platform, &floor));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
        })
    );
    read_back_as_the_coordinator("hostkeeper", &got);
}

#[test]
fn hostkeepers_native_entries_name_the_package_its_daemon_takes_as_its_own() {
    // Catches: a native entry of hostkeeper whose package its daemon does not take as its own
    // package (a `.deb` package other than `hostkeeper`), so the daemon would take the bootstrap
    // install for another service's; a run unit that nfpm.yaml does not install in the daemon's
    // own `.deb`; or a hook command on it (its daemon runs built-in steps).
    let (_root, dist) = dist("targets-hostkeeper-own");
    built(&dist, Package::Hostkeeper);
    let got = answered(&targets(&request(&dist, Package::Hostkeeper, None)).expect("the targets"));
    for (platform, list) in got.as_object().expect("an object") {
        let native = &list["entries"][0]["manifest"];
        let package = native["package"].as_str().expect("a package");
        assert_eq!(package, "hostkeeper", "{platform}");
        assert_eq!(
            native["hook"].get("command"),
            None,
            "{platform}: hostkeeper's daemon runs built-in steps"
        );
        let run = native["run"].as_array().expect("run units");
        assert!(!run.is_empty(), "{platform}");
        let nfpm = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/nfpm.yaml"),
        )
        .expect("read nfpm.yaml");
        for unit in run {
            let file = format!(
                "dst: /usr/lib/systemd/system/{}\n",
                unit.as_str().expect("a unit")
            );
            assert!(nfpm.contains(&file), "nfpm.yaml installs no {file}");
        }
    }
}

#[test]
fn hello_lists_its_deb_alone_on_linux_and_nothing_for_macos() {
    // Catches: hello's list with its archive entry still in it or without its native entry, a
    // list for macOS, a `.deb` entry that is not a repository entry of the testing suite pinned
    // by the `.deb`'s own SHA-256 at the release's version, and a native manifest that is not
    // the archive's own (its hook bounds, run units, health URL, params and floor) with the hook
    // command naming the program the `.deb` installs, the package's name and the installer's
    // bounds.
    let (_root, dist) = dist("targets-hello");
    built(&dist, Package::Hello);
    let got = answered(&targets(&request(&dist, Package::Hello, None)).expect("the targets"));
    let entry = |platform: Platform| alone(expected_native(&dist, platform));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
        })
    );
    read_back_as_the_coordinator("hello", &got);
}

#[test]
fn the_line_is_appended_to_the_output_file() {
    // Catches: the output file overwritten (GITHUB_OUTPUT holds the other steps' outputs), or
    // the line written there differing from the one printed.
    let (root, dist) = dist("targets-output");
    built(&dist, Package::Hello);
    let output = root.join("github_output");
    std::fs::write(&output, "before=1\n").expect("write the output file");
    let lines =
        targets(&request(&dist, Package::Hello, Some(output.clone()))).expect("the targets");
    let written = std::fs::read_to_string(&output).expect("read the output file");
    assert_eq!(written, format!("before=1\n{}\n", lines[0]));
}

/// A zstd tar of `entries` (path, type, bytes), as a hand-made archive.
fn archive_of(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, kind, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(*kind);
        header.set_mode(0o444);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *bytes)
            .expect("append");
    }
    package::compress(&builder.into_inner().expect("the tar"))
}

/// The manifest of mdm's archive as the package writes it
/// (`hostkeeper_mdm_core::package::manifest`), naming `name`, `version`, `platform` and `floor`.
fn mdm_manifest(name: &str, version: &str, platform: &str, floor: &str) -> String {
    hostkeeper_mdm_core::package::manifest(version, floor)
        .replacen("name = \"mdm\"", &format!("name = \"{name}\""), 1)
        .replacen(
            "platform = \"linux-x86_64\"",
            &format!("platform = \"{platform}\""),
            1,
        )
}

/// The release of mdm in `dist` with `archive` as its one archive, attested and indexed, and its
/// native package.
fn mdm_release(dist: &Path, archive: &[u8]) {
    let name = Package::Mdm.package_name(&release(), Platform::LinuxX86_64);
    std::fs::write(dist.join(name), archive).expect("write the archive");
    attest_and_index(dist, Package::Mdm);
    natives(dist, Package::Mdm);
}

/// The answer of the verb for mdm's release in a new `dist` whose one archive is `archive`.
fn mdm_with(name: &str, archive: &[u8]) -> Result<Vec<String>, Error> {
    let (_root, dist) = dist(name);
    mdm_release(&dist, archive);
    targets(&request(&dist, Package::Mdm, None))
}

const MDM_ARCHIVE: &str = "mdm-0.1.5-linux-x86_64.tar.zst";

fn refused(why: &str) -> Result<Vec<String>, Error> {
    Err(Error::Refused(vec![format!("{MDM_ARCHIVE}: {why}")]))
}

#[test]
fn mdm_gets_its_one_platform_its_native_entry_alone_and_the_floor_its_manifest_names() {
    // Catches: a platform the package is not released for given an entry (mdm's .deb is amd64
    // alone), mdm's list without its native entry or with the archive's, a native manifest
    // that is not mdm's archive manifest (its hook bounds, run units, health URL and params), or a
    // floor taken from anywhere but the archive's own manifest.
    let manifest = mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1.3");
    let archive = archive_of(&[(
        "manifest.toml",
        tar::EntryType::Regular,
        manifest.as_bytes(),
    )]);
    let (_root, dist) = dist("targets-mdm");
    mdm_release(&dist, &archive);
    let got = answered(&targets(&request(&dist, Package::Mdm, None)).expect("the targets"));
    let deb = "mdm_0.1.5_amd64.deb";
    let native = json!({
        "installer": "apt",
        "digest": file_sha256(&dist.join(deb)).expect("hash"),
        "manifest": {
            "package": "mdm",
            "hook": {
                "command": ["/usr/lib/mdm/mdm", "update-hook"],
                "timeoutSecs": {
                    "prepare": "300", "cutover": "120", "verify": "60", "rollback": "120",
                    "cleanup": "30", "uninstall": "120",
                },
                "verifyDeadlineSecs": "600",
            },
            "run": ["mdm-scep.service", "mdm-nanohub.service", "mdm-depserver.service"],
            "healthUrl": "http://127.0.0.1:9004/version",
            "params": {
                "record_dir": {"text": "/var/lib/updater/hooks/mdm"},
                "settings": {"text": "/etc/mdm/settings"},
            },
            "minHostkeeper": "0.1.3",
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        },
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    });
    assert_eq!(got, json!({"linux-x86_64": alone(native)}));
    read_back_as_the_coordinator("mdm", &got);
}

#[test]
fn an_archive_whose_manifest_does_not_name_this_release_is_refused() {
    // Catches: the floor copied from a manifest of another service, version or platform (an
    // archive put under the wrong name), or a manifest with no floor or a malformed one sent on
    // to the coordinator, which would refuse the whole set.
    let regular = tar::EntryType::Regular;
    let cases = [
        (
            mdm_manifest("hello", "0.1.5", "linux-x86_64", "0.1.3"),
            "manifest.toml names the name hello, not mdm",
        ),
        (
            mdm_manifest("mdm", "0.1.4", "linux-x86_64", "0.1.3"),
            "manifest.toml names the version 0.1.4, not 0.1.5",
        ),
        (
            mdm_manifest("mdm", "0.1.5", "linux-arm64", "0.1.3"),
            "manifest.toml names the platform linux-arm64, not linux-x86_64",
        ),
        (
            mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1"),
            "manifest.toml: min_hostkeeper \"0.1\" is not major.minor.patch: three whole \
             numbers without leading zeros or a suffix",
        ),
        (
            "schema = 1\nname = \"mdm\"\nversion = \"0.1.5\"\nplatform = \"linux-x86_64\"\n"
                .to_owned(),
            "manifest.toml has no string min_hostkeeper",
        ),
        (
            "schema = 1\nname = 7\n".to_owned(),
            "manifest.toml has no string name",
        ),
    ];
    for (n, (manifest, why)) in cases.iter().enumerate() {
        let archive = archive_of(&[("manifest.toml", regular, manifest.as_bytes())]);
        let got = mdm_with(&format!("targets-manifest-{n}"), &archive);
        assert_eq!(got, refused(why), "{manifest}");
    }
}

#[test]
fn an_archive_with_no_readable_manifest_is_refused() {
    // Catches: an archive that is not hostkeeper's (no top-level manifest.toml, one nested, a
    // link or a directory in its place, a manifest too long, not UTF-8 or not TOML) given an
    // entry the daemon could never install.
    let regular = tar::EntryType::Regular;
    let long = vec![b'#'; usize::try_from(MAX_MANIFEST_BYTES).expect("fits") + 1];
    let exact = {
        let mut text = mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1.3").into_bytes();
        let cap = usize::try_from(MAX_MANIFEST_BYTES).expect("fits");
        text.resize(cap, b'\n');
        text
    };
    let cases: [(Vec<u8>, &str); 7] = [
        (
            archive_of(&[("bin/mdm", regular, b"a program")]),
            "no manifest.toml at the top of the archive",
        ),
        (
            archive_of(&[("files/manifest.toml", regular, b"schema = 1\n")]),
            "no manifest.toml at the top of the archive",
        ),
        (
            archive_of(&[("manifest.toml", tar::EntryType::Directory, b"")]),
            "manifest.toml is not a regular file",
        ),
        (
            archive_of(&[("manifest.toml", regular, &long)]),
            "manifest.toml is longer than 65536 bytes",
        ),
        (
            archive_of(&[("manifest.toml", regular, b"\xff\xfe")]),
            "manifest.toml is not UTF-8",
        ),
        (archive_of(&[("manifest.toml", regular, b"name = [")]), ""),
        (archive_of(&[("manifest.toml", regular, &exact)]), "ok"),
    ];
    for (n, (archive, why)) in cases.iter().enumerate() {
        let got = mdm_with(&format!("targets-archive-{n}"), archive);
        match *why {
            // A manifest of exactly the cap is read.
            "ok" => assert!(got.is_ok(), "{got:?}"),
            // The TOML parser's own message follows the prefix.
            "" => {
                let Err(Error::Refused(problems)) = got else {
                    panic!("not refused: {got:?}");
                };
                assert_eq!(problems.len(), 1);
                let prefix = format!("{MDM_ARCHIVE}: manifest.toml is not TOML: ");
                assert!(problems[0].starts_with(&prefix), "{problems:?}");
            }
            why => assert_eq!(got, refused(why), "case {n}"),
        }
    }
}

#[test]
fn a_file_that_is_not_a_zstd_tar_is_refused() {
    // Catches: an archive that is not a zstd frame, or a zstd frame that holds no tar, read as
    // one with no manifest instead of named for what it is.
    let not_zstd = mdm_with("targets-not-zstd", b"plain bytes, no zstd frame");
    let Err(Error::Refused(problems)) = not_zstd else {
        panic!("not refused: {not_zstd:?}");
    };
    let prefix = format!("{MDM_ARCHIVE}: not a zstd archive: ");
    assert!(problems[0].starts_with(&prefix), "{problems:?}");

    let not_tar = mdm_with("targets-not-tar", &package::compress(&[7_u8; 1024]));
    let Err(Error::Refused(problems)) = not_tar else {
        panic!("not refused: {not_tar:?}");
    };
    let prefix = format!("{MDM_ARCHIVE}: not a tar archive: ");
    assert!(problems[0].starts_with(&prefix), "{problems:?}");
}

#[test]
fn every_problem_of_the_release_is_named_at_once() {
    // Catches: an archive of other bytes than the index lists read for its manifest, a missing
    // archive, or a native package's bundle over the daemon's cap given an entry; or the verb
    // stopping at the first problem, so a release with several is fixed one run at a time. An
    // archive's own bundle is no longer read: no entry carries it.
    let (_root, dist) = dist("targets-every-problem");
    built(&dist, Package::Hello);
    let name = |p: Platform| Package::Hello.package_name(&release(), p);
    // linux-arm64: other bytes than the index lists (its manifest is not read).
    std::fs::write(dist.join(name(Platform::LinuxArm64)), b"other bytes").expect("rewrite");
    // linux-x86_64: no archive and no archive bundle.
    std::fs::remove_file(dist.join(name(Platform::LinuxX86_64))).expect("remove");
    std::fs::remove_file(dist.join(bundle_of(&name(Platform::LinuxX86_64)))).expect("remove");
    // linux-arm64 too: its .deb's bundle over 1 MiB. macos-arm64: no .pkg and no bundle, which
    // are not read, since macOS has no list.
    let deb = Package::Hello
        .native_name(&release(), Platform::LinuxArm64)
        .expect("a .deb");
    let pkg_bundle = bundle_of(&deb);
    std::fs::write(dist.join(&pkg_bundle), vec![b' '; (1 << 20) + 1]).expect("rewrite");
    let pkg = Package::Hello
        .native_name(&release(), Platform::MacosArm64)
        .expect("a .pkg");
    std::fs::remove_file(dist.join(&pkg)).expect("remove");
    std::fs::remove_file(dist.join(bundle_of(&pkg))).expect("remove");
    let index = std::fs::read_to_string(dist.join("hello-index.json")).expect("the index");
    let listed = ReleaseIndex::parse(
        &index,
        &ServiceName::new("hello").expect("a service"),
        &ServiceVersion::new("0.1.5").expect("a version"),
    )
    .expect("the index");
    let arm = &listed.packages()[0];
    let got = targets(&request(&dist, Package::Hello, None));
    assert_eq!(
        got,
        Err(Error::Refused(vec![
            format!(
                "hello-0.1.5-linux-arm64.tar.zst: package_digest_mismatch: 11 bytes with SHA-256 \
                 {}, but the index lists {} bytes with SHA-256 {}",
                file_sha256(&dist.join(name(Platform::LinuxArm64))).expect("hash"),
                arm.bytes,
                arm.sha256.as_str()
            ),
            format!(
                "{pkg_bundle}: longer than 1048576 bytes, the most the daemon reads of a bundle"
            ),
            "hello-0.1.5-linux-x86_64.tar.zst: missing, but the index lists it".to_owned(),
        ]))
    );
}

#[test]
fn an_index_missing_or_short_of_a_platform_is_refused() {
    // Catches: a release whose index leaves a platform out given targets without it, so every
    // host of that platform shows `no_target` though the release built for it; or a missing
    // index read as a release of no platform.
    let (_root, dist) = dist("targets-index");
    let missing = targets(&request(&dist, Package::Hello, None));
    assert_eq!(
        missing,
        Err(Error::Io {
            what: format!("read {}", dist.join("hello-index.json").display()),
            why: "No such file or directory (os error 2)".to_owned(),
        })
    );
    built(&dist, Package::Hello);
    let short = index::ReleaseIndex::of_dir(&dist, "hello", "0.1.5", &[Platform::LinuxX86_64])
        .expect("an index of one platform");
    std::fs::write(dist.join("hello-index.json"), short.to_json()).expect("rewrite the index");
    assert_eq!(
        targets(&request(&dist, Package::Hello, None)),
        Err(Error::Failed(format!(
            "{}: lists linux-x86_64, not every platform the package is released for: \
             linux-arm64, linux-x86_64, macos-arm64",
            dist.join("hello-index.json").display()
        )))
    );
}

/// One valid entry of hello for linux-x86_64, as the verb writes it: its `.deb` from the
/// testing suite.
fn an_entry() -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "a".repeat(64),
        manifest: Some(Manifest {
            package: Some("hello".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    cutover: 60,
                    verify: 30,
                    rollback: 60,
                    cleanup: 30,
                    ..StepBounds::default()
                }),
                verify_deadline_secs: 120,
                ..Hook::default()
            }),
            min_hostkeeper: "0.1.0".to_owned(),
            installer_bounds: Some(INSTALLER_BOUNDS),
            ..Manifest::default()
        }),
        repository: Some(crate::release::contract::TESTING_ENTRY.to_owned()),
        package_version: Some("0.1.5".to_owned()),
        ..Target::default()
    }
}

#[test]
fn entries_the_coordinator_or_the_daemon_would_refuse_are_never_written() {
    // Catches: entries sent on that the coordinator refuses (`target_invalid`, failing the
    // whole set) or the daemon refuses (`manifest_invalid`), found only once CI has called.
    let hello = ServiceName::new("hello").expect("a service");
    let one = |entry: Target| [(IndexPlatform::LinuxX86_64, vec![(Installer::Apt, entry)])];
    assert_eq!(checked(&hello, &one(an_entry())), Ok(()));

    let mut long = an_entry();
    long.repository = Some("r".repeat(2048));
    let Err(Error::Failed(why)) = checked(&hello, &one(long)) else {
        panic!("a repository name too long is written");
    };
    assert!(
        why.starts_with("target_invalid: entry 0 of linux-x86_64: "),
        "{why}"
    );

    let mut more = an_entry();
    if let Some(manifest) = more.manifest.as_mut() {
        manifest.hook = None;
    }
    let Err(Error::Failed(why)) = checked(&hello, &one(more)) else {
        panic!("a manifest with no hook is written");
    };
    assert!(
        why.starts_with("manifest_invalid: linux-x86_64: apt: "),
        "{why}"
    );
}

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: Verb,
}

fn verb(dist: &Path, extra: &[&str]) -> Verb {
    let dir = dist.display().to_string();
    let argv = [
        "release",
        "targets",
        "--version",
        "0.1.5",
        "--dir",
        dir.as_str(),
        "--repo",
        REPO,
    ];
    Args::try_parse_from(argv.iter().chain(extra))
        .map(|a| a.verb)
        .expect("parse the argv")
}

#[test]
fn the_verb_runs_without_adapters_and_refuses_a_failing_build_of_hostkeeper() {
    // Catches: the verb building the real adapters (and so needing `gh` and `git`) though it
    // reads files only, `run_with` failing it instead of handing it back, its flags not reaching
    // it (`--repo` is still taken, and changes nothing), or `--fail-ready` taken for a service
    // with no failing build.
    let (_root, dist) = dist("targets-verb");
    built(&dist, Package::Hello);
    let connected = std::cell::Cell::new(false);
    let hello = crate::release::run(verb(&dist, &["--service", "hello"]), || {
        connected.set(true);
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(hello, targets(&request(&dist, Package::Hello, None)));
    assert!(hello.is_ok());
    assert!(!connected.get());

    let fakes = Fakes::over(FakeHistory::default());
    let lent = crate::release::run_with(verb(&dist, &["--service", "hello"]), &fakes.ports());
    assert_eq!(lent, hello);

    let fail_ready = crate::release::run(verb(&dist, &["--fail-ready"]), || {
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(
        fail_ready,
        Err(Error::Failed(package::FAIL_READY_ONLY_HELLO.to_owned()))
    );
    let service = Args::try_parse_from(["release", "targets", "--service", "tierstore"]);
    assert_eq!(
        service.err().map(|e| e.kind()),
        Some(clap::error::ErrorKind::InvalidValue)
    );
    assert_eq!(Service::default(), Service::Hostkeeper);
}

mod fail_ready;
mod native;
