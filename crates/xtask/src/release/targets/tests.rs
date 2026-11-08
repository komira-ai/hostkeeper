use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::Parser;
use hostkeeper_config::index::ReleaseIndex;
use prost_reflect::DeserializeOptions;
use serde_json::json;

use super::*;
use crate::release::contract::{Commit, Platform, Service, bundle_of};
use crate::release::fakes::{FakeHistory, Fakes, scratch};
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

/// The release of `package`, every archive built as the release builds it.
fn built(dist: &Path, package: Package) {
    for platform in package.platforms() {
        build(dist, package, *platform);
    }
    attest_and_index(dist, package);
}

fn request(dist: &Path, package: Package, output: Option<PathBuf>) -> Request {
    Request {
        package,
        version: release(),
        dir: dist.to_path_buf(),
        repo: REPO.parse().expect("a repository"),
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

/// The entry the verb must write for the archive `name` of `dist`, whose manifest needs
/// `floor`.
fn expected_entry(dist: &Path, name: &str, version_dir: &str, floor: &str) -> Value {
    let bundle = bundle_of(name);
    let bundle_bytes = std::fs::metadata(dist.join(&bundle))
        .expect("the bundle")
        .len();
    let bytes = std::fs::metadata(dist.join(name))
        .expect("the archive")
        .len();
    let asset = format!("{REPO}/releases/download/v0.1.5");
    json!({"entries": [{
        "installer": "hostkeeper-archive",
        "locations": [
            {"store": "channel", "locator": format!("{version_dir}/{name}")},
            {"store": "github", "locator": format!("{asset}/{name}")},
        ],
        "digest": file_sha256(&dist.join(name)).expect("hash"),
        "bytes": bytes.to_string(),
        "attestation": {
            "digest": file_sha256(&dist.join(&bundle)).expect("hash"),
            "bytes": bundle_bytes.to_string(),
            "locations": [
                {"store": "channel", "locator": format!("{version_dir}/{bundle}")},
                {"store": "github", "locator": format!("{asset}/{bundle}")},
            ],
        },
        "manifest": {"minHostkeeper": floor},
    }]})
}

/// Reads `targets` back as the coordinator does (proto3 JSON, no unknown field), checks them by
/// its rules, and holds every platform's archive entry to the index `index_name` of `dist`: the
/// agreement the coordinator requires of a set with both.
fn read_back_as_the_coordinator(dist: &Path, service: &str, version: &str, targets: &Value) {
    let descriptor = TARGET_LIST.clone().expect("the descriptor");
    let options = DeserializeOptions::new().deny_unknown_fields(true);
    let mut lists = BTreeMap::new();
    for (platform, list) in targets.as_object().expect("an object") {
        let read: TargetList =
            DynamicMessage::deserialize_with_options(descriptor.clone(), list.clone(), &options)
                .expect("proto3 JSON")
                .transcode_to()
                .expect("a TargetList");
        lists.insert(platform.clone(), read);
    }
    let service = ServiceName::new(service).expect("a service");
    target::check(&service, &lists).expect("the coordinator's checks");
    let name = Package::ALL
        .into_iter()
        .find(|p| p.service().name() == service.as_str() && p.version_text(&release()) == version)
        .expect("a package")
        .index_name();
    let text = std::fs::read_to_string(dist.join(name)).expect("the index");
    let version = ServiceVersion::new(version).expect("a version");
    let index = ReleaseIndex::parse(&text, &service, &version).expect("the index");
    let from_index: BTreeMap<String, (String, u64)> = index
        .packages()
        .iter()
        .map(|p| {
            let listed = (p.sha256.as_str().to_owned(), p.bytes);
            (p.platform.as_str().to_owned(), listed)
        })
        .collect();
    let from_targets: BTreeMap<String, (String, u64)> = lists
        .iter()
        .map(|(platform, list)| {
            let [entry] = list.entries.as_slice() else {
                panic!("not one entry for {platform}");
            };
            assert_eq!(entry.installer, "hostkeeper-archive");
            (platform.clone(), (entry.digest.clone(), entry.bytes))
        })
        .collect();
    assert_eq!(from_targets, from_index);
}

#[test]
fn hostkeeper_gets_one_archive_entry_per_platform_the_index_lists() {
    // Catches: an entry of other bytes than the index lists (the coordinator refuses the set
    // `index_invalid`), a platform left out, the bundle's size or digest not the bundle's own, a
    // location the daemon cannot fetch from (the channel path or the release asset misspelt),
    // the channel not first, or a manifest that carries more than `min_hostkeeper` (the daemon
    // refuses it `manifest_invalid`) or another floor than the archive's own.
    let (_root, dist) = dist("targets-hostkeeper");
    built(&dist, Package::Hostkeeper);
    let floor = release().min(OLDEST_READER).to_string();
    let lines = targets(&request(&dist, Package::Hostkeeper, None)).expect("the targets");
    let got = answered(&lines);
    let entry = |platform: &str| {
        let name = format!("hostkeeper-0.1.5-{platform}.tar.zst");
        expected_entry(&dist, &name, "hostkeeper/0.1.5", &floor)
    };
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry("linux-arm64"),
            "linux-x86_64": entry("linux-x86_64"),
            "macos-arm64": entry("macos-arm64"),
        })
    );
    read_back_as_the_coordinator(&dist, "hostkeeper", "0.1.5", &got);
}

#[test]
fn hello_and_its_failing_build_are_found_under_the_releases_tag() {
    // Catches: the failing build's assets looked for under a tag of its own version
    // (`v0.1.5-fail-ready`, which does not exist), its channel path under the release's version,
    // or hello's floor not read from its own manifest (`0.1.0`, not hostkeeper's).
    let (_root, dist) = dist("targets-hello");
    built(&dist, Package::Hello);
    built(&dist, Package::HelloFailReady);
    for (package, version) in [
        (Package::Hello, "0.1.5"),
        (Package::HelloFailReady, "0.1.5-fail-ready"),
    ] {
        let got = answered(&targets(&request(&dist, package, None)).expect("the targets"));
        let entry = |platform: &str| {
            let name = format!("hello-{version}-{platform}.tar.zst");
            expected_entry(&dist, &name, &format!("hello/{version}"), "0.1.0")
        };
        assert_eq!(
            got,
            json!({
                "linux-arm64": entry("linux-arm64"),
                "linux-x86_64": entry("linux-x86_64"),
                "macos-arm64": entry("macos-arm64"),
            }),
            "{version}"
        );
        read_back_as_the_coordinator(&dist, "hello", version, &got);
    }
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

/// The manifest of mdm's archive, as the package writes it, with `extra` appended.
fn mdm_manifest(name: &str, version: &str, platform: &str, floor: &str) -> String {
    format!(
        "schema = 1\nname = \"{name}\"\nversion = \"{version}\"\nplatform = \"{platform}\"\n\
         min_hostkeeper = \"{floor}\"\n"
    )
}

/// The release of mdm in `dist` with `archive` as its one archive, attested and indexed.
fn mdm_release(dist: &Path, archive: &[u8]) {
    let name = Package::Mdm.package_name(&release(), Platform::LinuxX86_64);
    std::fs::write(dist.join(name), archive).expect("write the archive");
    attest_and_index(dist, Package::Mdm);
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
fn mdm_gets_its_one_platform_and_the_floor_its_manifest_names() {
    // Catches: a platform the package is not released for given an entry, or a floor taken from
    // anywhere but the archive's own manifest.
    let manifest = mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1.3");
    let archive = archive_of(&[(
        "manifest.toml",
        tar::EntryType::Regular,
        manifest.as_bytes(),
    )]);
    let (_root, dist) = dist("targets-mdm");
    mdm_release(&dist, &archive);
    let got = answered(&targets(&request(&dist, Package::Mdm, None)).expect("the targets"));
    assert_eq!(
        got,
        json!({"linux-x86_64": expected_entry(&dist, MDM_ARCHIVE, "mdm/0.1.5", "0.1.3")})
    );
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
    // Catches: an archive of other bytes than the index lists, a missing or wrong bundle, a
    // missing archive, or a bundle over the daemon's cap given an entry; or the verb stopping at
    // the first problem, so a release with several is fixed one run at a time.
    let (_root, dist) = dist("targets-every-problem");
    built(&dist, Package::Hello);
    let name = |p: Platform| Package::Hello.package_name(&release(), p);
    // linux-arm64: other bytes than the index lists (its manifest is not read).
    std::fs::write(dist.join(name(Platform::LinuxArm64)), b"other bytes").expect("rewrite");
    // linux-x86_64: no archive and no bundle.
    std::fs::remove_file(dist.join(name(Platform::LinuxX86_64))).expect("remove");
    std::fs::remove_file(dist.join(bundle_of(&name(Platform::LinuxX86_64)))).expect("remove");
    // macos-arm64: a bundle over 1 MiB.
    let mac_bundle = bundle_of(&name(Platform::MacosArm64));
    std::fs::write(dist.join(&mac_bundle), vec![b' '; (1 << 20) + 1]).expect("rewrite");
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
            "hello-0.1.5-linux-x86_64.tar.zst: missing, but the index lists it".to_owned(),
            "hello-0.1.5-linux-x86_64.tar.zst.sigstore.json: missing: every archive entry carries \
             its attestation bundle"
                .to_owned(),
            format!(
                "{mac_bundle}: longer than 1048576 bytes, the most the daemon reads of a bundle"
            ),
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

/// One valid entry of hello for linux-x86_64, as the verb writes it.
fn an_entry() -> Target {
    let digest = "a".repeat(64);
    Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: locations("hello/0.1.5/a.tar.zst", "o/r/releases/download/v0.1.5", "a"),
        digest: digest.clone(),
        bytes: 1,
        attestation: Some(Blob {
            digest,
            bytes: 1,
            locations: locations("hello/0.1.5/b", "o/r/releases/download/v0.1.5", "b"),
        }),
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            ..Manifest::default()
        }),
    }
}

#[test]
fn entries_the_coordinator_or_the_daemon_would_refuse_are_never_written() {
    // Catches: entries sent on that the coordinator refuses (`target_invalid`, failing the
    // whole set) or the daemon refuses (`manifest_invalid`: a `hostkeeper-archive` manifest
    // carries `min_hostkeeper` alone), found only once CI has called.
    let hello = ServiceName::new("hello").expect("a service");
    let good = [(IndexPlatform::LinuxX86_64, an_entry())];
    assert_eq!(checked(&hello, &good), Ok(()));

    let mut long = an_entry();
    long.locations[1].locator = format!("o/{}", "r".repeat(2048));
    assert_eq!(
        checked(&hello, &[(IndexPlatform::LinuxX86_64, long)]),
        Err(Error::Failed(format!(
            "target_invalid: entry 0 of linux-x86_64: the locator \"o/{}\" is 2050 bytes, more \
             than 1024",
            "r".repeat(2048)
        )))
    );
    let mut more = an_entry();
    if let Some(manifest) = more.manifest.as_mut() {
        manifest.run = vec!["hello.service".to_owned()];
    }
    assert_eq!(
        checked(&hello, &[(IndexPlatform::LinuxX86_64, more)]),
        Err(Error::Failed(
            "manifest_invalid: linux-x86_64: the manifest of a hostkeeper-archive entry may not have run"
                .to_owned()
        ))
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
    // it, or `--fail-ready` taken for a service with no failing build.
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
