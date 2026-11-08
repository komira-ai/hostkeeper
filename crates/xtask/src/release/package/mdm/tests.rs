use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest, ManifestRefusal};
use hostkeeper_config::names::{ServiceName, Version as ConfigVersion};
use hostkeeper_mdm_core::package::{
    DEPSERVER, HEALTH_URL, NANOHUB, RECORD_DIR, RUN, SCEP, SETTINGS_FILE, UNITS, manifest,
};
use hostkeeper_mdm_core::pins::{Pins, Tool};
use sha2::{Digest, Sha256};

use crate::release::contract::{Commit, Package, Platform, Service, Version};
use crate::release::fakes::scratch;
use crate::release::file_sha256;
use crate::release::package::tests::{Unseal, built, history, read_back, shape_problems, unpack};
use crate::release::package::{
    ARCHIVES_ONLY_MDM, Error, MDM_NEEDS_ARCHIVES, MDM_PLATFORM, MDM_REFUSED, Request, check_unit,
    package, package_dated, package_pinned,
};
use crate::release::ports::GitHistory;

/// `bin/mdm`; nothing reads it as a program.
const BINARY: &[u8] = b"the mdm program";

/// The version of the fixture pins.
const PINNED: &str = "1.0.0";

/// What the fixture archive of `tool` holds for the member installed as `name`.
fn member_bytes(name: &str) -> Vec<u8> {
    format!("the {name} binary\n").into_bytes()
}

/// A gzip-compressed tar of `members`.
fn tar_gz(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in members {
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o755);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, name, data.as_slice())
            .expect("append");
    }
    let tar = builder.into_inner().expect("the tar");
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(&tar).expect("gzip");
    gz.finish().expect("gzip")
}

/// The archive of `tool` as the fixture pins it: every member the tool's pin names.
fn archive_of(tool: Tool) -> Vec<u8> {
    let members: Vec<(String, Vec<u8>)> = tool
        .members(PINNED)
        .into_iter()
        .map(|(member, name)| (member, member_bytes(name)))
        .collect();
    tar_gz(&members)
}

/// Pins of every tool over the archives `archive` gives, each a `.tar.gz` of `example.com`,
/// with the archives written into `dir`.
fn pins_in(dir: &Path, archive: impl Fn(Tool) -> Vec<u8>) -> Pins {
    std::fs::create_dir_all(dir).expect("the archives directory");
    let mut text = String::new();
    for tool in Tool::ALL {
        let bytes = archive(tool);
        std::fs::write(dir.join(format!("{tool}.tar.gz")), &bytes).expect("write an archive");
        let url = format!("https://example.com/{tool}/v{PINNED}/{tool}.tar.gz");
        let sum = hex::encode(Sha256::digest(&bytes));
        text.push_str(&format!("{tool} {PINNED} {url} {sum}\n"));
    }
    Pins::parse(&text).expect("the fixture pins")
}

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A request for the mdm package written under `dir`/`out`, the binary written beside it and the
/// archives in `dir/archives`.
fn request(dir: &Path, commit: &Commit, out: &str) -> Request {
    std::fs::write(dir.join("mdm"), BINARY).expect("write the binary");
    Request {
        service: Service::Mdm,
        platform: Platform::LinuxX86_64,
        version: version(),
        commit: commit.clone(),
        binary: dir.join("mdm"),
        unit: None,
        fail_ready: false,
        trusted_root: None,
        archives: Some(dir.join("archives")),
        out: dir.join(out),
    }
}

/// Builds the package of `r` with `pins`, dated `mtime`.
fn build(r: &Request, pins: &Pins, mtime: u64) -> Result<Vec<String>, Error> {
    package_pinned(r, || Ok(mtime), || Ok(pins.clone()))
}

/// Every installed name of the tools the package carries.
const TOOL_BINARIES: [&str; 6] = [
    "caddy",
    "depserver",
    "depsyncer",
    "lego",
    "nanohub",
    "scepserver",
];

#[test]
fn the_package_holds_its_manifest_its_units_and_the_pinned_tools_at_their_modes() {
    // Catches: a tool of the package missing, one the package does not carry (mdmctl) added, a
    // tool's bytes other than its archive member's, a unit other than hostkeeper-mdm-core's
    // byte for byte, a manifest other than its, a mode other than 0555 for a binary or 0444 for
    // a unit or the manifest, or an entry that breaks the archive's shape.
    let dir = scratch("mdm-package-shape");
    let pins = pins_in(&dir.join("archives"), archive_of);
    let (_, commit) = history();
    let r = request(&dir, &commit, "out");
    let lines = build(&r, &pins, 1_700_000_000).expect("package");
    let bytes = built(&r);
    assert_eq!(shape_problems(&bytes, 1_700_000_000), Vec::<String>::new());
    let seen = read_back(&bytes);
    let listed: Vec<(String, u32)> = seen.iter().map(|e| (e.name.clone(), e.mode)).collect();
    let mut want = vec![("bin/".to_owned(), 0o555)];
    let mut bins: Vec<&str> = TOOL_BINARIES.to_vec();
    bins.push("mdm");
    bins.sort();
    want.extend(bins.iter().map(|b| (format!("bin/{b}"), 0o555)));
    want.push(("manifest.toml".to_owned(), 0o444));
    want.push(("units/".to_owned(), 0o555));
    want.extend(UNITS.iter().map(|u| (format!("units/{}", u.name), 0o444)));
    assert_eq!(listed, want);
    let data = |name: &str| {
        seen.iter()
            .find(|e| e.name == name)
            .map(|e| e.data.clone())
            .expect(name)
    };
    assert_eq!(data("bin/mdm"), BINARY);
    for name in TOOL_BINARIES {
        assert_eq!(data(&format!("bin/{name}")), member_bytes(name), "{name}");
    }
    for unit in UNITS {
        assert_eq!(data(&format!("units/{}", unit.name)), unit.text.as_bytes());
    }
    assert_eq!(
        data("manifest.toml"),
        manifest("0.1.5", "0.1.5").into_bytes()
    );
    let name = "mdm-0.1.5-linux-x86_64.tar.zst";
    let hex = file_sha256(&r.out.join(name)).expect("hash");
    assert_eq!(
        lines,
        [format!("{name}: sha256 {hex}"), format!("sha256={hex}")]
    );
    assert_eq!(
        std::fs::read_to_string(r.out.join(format!("{name}.sha256"))).expect("digest file"),
        format!("{hex}  {name}\n")
    );
}

#[test]
fn the_package_is_the_same_bytes_for_the_same_inputs() {
    // Catches: an archive whose bytes depend on the run (the order the archives are read in, a
    // clock), so its attestation could not be reproduced.
    let dir = scratch("mdm-package-deterministic");
    let pins = pins_in(&dir.join("archives"), archive_of);
    let (_, commit) = history();
    let (a, b) = (request(&dir, &commit, "a"), request(&dir, &commit, "b"));
    build(&a, &pins, 7).expect("package a");
    build(&b, &pins, 7).expect("package b");
    assert_eq!(built(&a), built(&b));
}

#[test]
fn the_package_is_dated_the_commit_time_of_git() {
    // Catches: the mdm package dated by anything but the commit built, as the other packages
    // are.
    let dir = scratch("mdm-package-dated");
    let (history, commit) = history();
    let r = request(&dir, &commit, "out");
    // No archive is there: the built-in pins are asked for and their archives are missing.
    let err = package(&history, &r).expect_err("no archive");
    assert!(
        matches!(&err, Error::Io { what, .. } if what.contains("the pinned archive of nanohub")),
        "{err:?}"
    );
    let pins = pins_in(&dir.join("archives"), archive_of);
    let r = request(&dir, &commit, "out2");
    package_pinned(
        &r,
        || Ok(history.commit_time(&commit)?),
        || Ok(pins.clone()),
    )
    .expect("package");
    let mtime = history.commit_time(&commit).expect("the commit time");
    assert!(read_back(&built(&r)).iter().all(|e| e.mtime == mtime));
}

/// The manifest of the fixture package unpacked into a scratch directory, loaded as hostkeeper
/// 0.1.`running`.
fn loaded(name: &str, running: u64) -> Result<Manifest, ManifestRefusal> {
    loaded_at(name, "0.1.5", running)
}

/// [`loaded`] for a package built at `version`.
fn loaded_at(name: &str, version: &str, running: u64) -> Result<Manifest, ManifestRefusal> {
    let dir = scratch(name);
    let pins = pins_in(&dir.join("archives"), archive_of);
    let (_, commit) = history();
    let mut r = request(&dir, &commit, "out");
    r.version = version.parse().expect("a version");
    build(&r, &pins, 1).expect("package");
    let version_dir = scratch(&format!("{name}-unpacked"));
    unpack(&built(&r), &version_dir);
    let expected = Expected {
        name: ServiceName::new("mdm").expect("a service name"),
        version: ConfigVersion::new(version).expect("a version"),
        platform: hostkeeper_config::platform::Platform::LinuxX86_64,
    };
    let running = HostkeeperVersion::parse(&format!("0.1.{running}")).expect("a version");
    Manifest::load(&version_dir, &expected, &running)
}

#[test]
fn the_manifest_loads_with_the_daemons_loader_and_names_the_run_units_the_hook_and_the_params() {
    // Catches: an mdm package the daemon's loader refuses (a unit file whose name is not the
    // service's, a unit kind it does not link, a run unit missing from units/, a hook without
    // bounds, a health URL that is not loopback), or one whose hook, run units or params differ
    // from the ones `bin/mdm` reads.
    let manifest = loaded("mdm-package-loader", 53).expect("the manifest loads");
    assert_eq!(manifest.name.as_str(), "mdm");
    assert_eq!(manifest.run, RUN);
    assert_eq!(manifest.run, [SCEP, NANOHUB, DEPSERVER]);
    let files: BTreeSet<String> = UNITS.iter().map(|u| u.name.to_owned()).collect();
    assert_eq!(manifest.unit_files, files);
    assert_eq!(manifest.hook.command, ["bin/mdm", "update-hook"]);
    assert_eq!(manifest.health_url.as_deref(), Some(HEALTH_URL));
    assert_eq!(
        manifest.params.get("settings").and_then(|v| v.as_str()),
        Some(SETTINGS_FILE)
    );
    assert_eq!(
        manifest.params.get("record_dir").and_then(|v| v.as_str()),
        Some(RECORD_DIR)
    );
}

#[test]
fn a_daemon_older_than_the_packages_floor_refuses_it() {
    // Catches: a floor below hostkeeper's own package's (a daemon that predates the hooks and
    // the units the package relies on taking it), one above it (no host of the farm could
    // install the package), or a build of an older version asking for a newer daemon than
    // itself (a workspace build no daemon of that build takes).
    assert!(loaded_at("mdm-package-floor-ok", "0.1.60", 53).is_ok());
    assert!(matches!(
        loaded_at("mdm-package-floor-old", "0.1.60", 52),
        Err(ManifestRefusal::DaemonTooOld { .. })
    ));
    assert!(loaded("mdm-package-floor-own", 5).is_ok());
    assert!(matches!(
        loaded("mdm-package-floor-own-old", 4),
        Err(ManifestRefusal::DaemonTooOld { .. })
    ));
}

#[test]
fn the_package_unpacks_with_the_daemons_unpacker() {
    // Catches: an mdm package the daemon's own unpacker refuses (a mode, a top-level name, a
    // digest), or one whose tools do not land under the version's `bin/`.
    let dir = scratch("mdm-package-unpacker");
    let pins = pins_in(&dir.join("archives"), archive_of);
    let (_, commit) = history();
    let mut r = request(&dir, &commit, "out");
    let running = hostkeeper_unpack::running_hostkeeper().to_string();
    r.version = running.parse().expect("the workspace version");
    build(&r, &pins, 1).expect("package");
    let archive = r
        .out
        .join(Package::Mdm.package_name(&r.version, Platform::LinuxX86_64));
    let packages = dir.join("packages");
    std::fs::create_dir(&packages).expect("the packages directory");
    let unseal = Unseal(packages.clone());
    let meta = std::fs::metadata(&packages).expect("stat");
    let owner = hostkeeper_unpack::Owner {
        uid: std::os::unix::fs::MetadataExt::uid(&meta),
        gid: std::os::unix::fs::MetadataExt::gid(&meta),
    };
    let fs = hostkeeper_unpack::HostFs::open(&packages, owner).expect("open the packages");
    let order = hostkeeper_unpack::UnpackOrder {
        service: ServiceName::new("mdm").expect("a service name"),
        version: ConfigVersion::new(&running).expect("a version"),
        platform: hostkeeper_config::platform::Platform::LinuxX86_64,
        package_digest: hostkeeper_config::daemon::Sha256Hex::parse(
            &file_sha256(&archive).expect("hash"),
        )
        .expect("a digest"),
    };
    let limits = hostkeeper_unpack::Limits {
        max_unpacked_bytes: 1 << 30,
        max_entries: 1000,
    };
    let unpacked = hostkeeper_unpack::unpack(
        &archive,
        &order,
        &limits,
        &fs,
        hostkeeper_unpack::Platform::Linux,
    )
    .expect("unpack");
    assert!(unpacked.fresh);
    assert_eq!(unpacked.manifest.run, RUN);
    let bin = packages.join("mdm").join(&running).join("bin");
    assert_eq!(std::fs::read(bin.join("mdm")).expect("bin/mdm"), BINARY);
    assert_eq!(
        std::fs::read(bin.join("lego")).expect("bin/lego"),
        member_bytes("lego")
    );
    drop(unseal);
}

/// Builds `r` with `pins` and requires it refused with `want`, nothing written.
fn refused(r: &Request, pins: &Pins, want: &Error) {
    assert_eq!(build(r, pins, 1).as_ref(), Err(want));
    assert!(!r.out.exists(), "nothing is written on a refusal");
}

#[test]
fn the_mdm_package_refuses_every_flag_it_does_not_take_and_every_platform_but_linux_x86_64() {
    // Catches: an mdm package built from a unit, a trusted root or a failing build given on the
    // command line, one built for a platform its linux-amd64 tools do not run on, one built
    // without its archives, or `--archives` taken silently by another service's package.
    let dir = scratch("mdm-package-refusals");
    let pins = pins_in(&dir.join("archives"), archive_of);
    let (_, commit) = history();
    let base = || request(&dir, &commit, "out");
    let failed = |why: &str| Error::Failed(why.to_owned());
    let mut r = base();
    r.unit = Some(dir.join("mdm.service"));
    refused(&r, &pins, &failed(MDM_REFUSED));
    let mut r = base();
    r.trusted_root = Some(dir.join("trusted_root.json"));
    refused(&r, &pins, &failed(MDM_REFUSED));
    let mut r = base();
    r.fail_ready = true;
    refused(&r, &pins, &failed(MDM_REFUSED));
    for platform in [Platform::LinuxArm64, Platform::MacosArm64] {
        let mut r = base();
        r.platform = platform;
        refused(&r, &pins, &failed(MDM_PLATFORM));
    }
    let mut r = base();
    r.archives = None;
    refused(&r, &pins, &failed(MDM_NEEDS_ARCHIVES));
    for service in [Service::Hostkeeper, Service::Hello] {
        let mut r = base();
        r.service = service;
        refused(&r, &pins, &failed(ARCHIVES_ONLY_MDM));
    }
}

#[test]
fn a_missing_or_differing_archive_or_one_without_its_member_refuses_the_build_naming_it() {
    // Catches: a package built from an archive that is not its pin's (a download replaced after
    // the fetch), from no archive, or with a member missing: each is refused naming the file and,
    // for a digest, the one it has and the pin's, and no package is written.
    let dir = scratch("mdm-package-bad-archive");
    let archives = dir.join("archives");
    let pins = pins_in(&archives, archive_of);
    let (_, commit) = history();
    let r = request(&dir, &commit, "out");
    let caddy = archives.join("caddy.tar.gz");

    std::fs::write(&caddy, b"another caddy").expect("write");
    let got = hex::encode(Sha256::digest(b"another caddy"));
    let want = pins.get(Tool::Caddy).sha256_hex();
    refused(
        &r,
        &pins,
        &Error::Failed(format!(
            "{}: its SHA-256 is {got}, the pin of caddy is {want}",
            caddy.display()
        )),
    );

    std::fs::remove_file(&caddy).expect("remove");
    let err = build(&r, &pins, 1).expect_err("no archive");
    assert!(
        matches!(&err, Error::Io { what, .. }
            if *what == format!("read {}, the pinned archive of caddy", caddy.display())),
        "{err:?}"
    );
    assert!(!r.out.exists());

    // NanoDEP's archive without its depsyncer, pinned as it is.
    let without = |tool: Tool| match tool {
        Tool::Nanodep => tar_gz(&[(
            format!("nanodep-linux-amd64-v{PINNED}/depserver-linux-amd64"),
            member_bytes("depserver"),
        )]),
        other => archive_of(other),
    };
    let short = dir.join("short");
    let pins = pins_in(&short.join("archives"), without);
    let r = request(&short, &commit, "out");
    let err = build(&r, &pins, 1).expect_err("a member is missing");
    assert!(
        err.to_string().contains(&format!(
            "nanodep-linux-amd64-v{PINNED}/depsyncer-linux-amd64"
        )),
        "{err}"
    );
    assert!(!r.out.exists());
}

#[test]
fn every_unit_of_the_package_starts_a_binary_of_its_bin_through_current() {
    // Catches: an mdm unit check that still wants `bin/mdm` (every service unit starts a tool),
    // or one that lets a unit start a program outside the package, in a directory below `bin/`,
    // or through a version directory instead of `current`.
    for unit in UNITS.iter().filter(|u| u.name.ends_with(".service")) {
        assert_eq!(
            check_unit("mdm", unit.name, unit.text.as_bytes()),
            Ok(()),
            "{}",
            unit.name
        );
    }
    let bin = "/var/lib/hostkeeper/packages/mdm/current/bin/";
    let no_start = Err(Error::Failed(format!(
        "u has no ExecStart line naming a program in {bin}"
    )));
    for program in ["/usr/bin/caddy", &format!("{bin}tools/caddy"), bin] {
        let unit = format!("[Service]\nExecStart={program} run\n");
        assert_eq!(
            check_unit("mdm", "u", unit.as_bytes()),
            no_start,
            "{program}"
        );
    }
    let versioned = format!(
        "[Service]\nExecStart={bin}caddy --config /var/lib/hostkeeper/packages/mdm/1.2.3/x\n"
    );
    assert_eq!(
        check_unit("mdm", "u", versioned.as_bytes()),
        Err(Error::Failed(
            "u names a package file by a path other than /var/lib/hostkeeper/packages/mdm/current/"
                .to_owned()
        ))
    );
    // Another service's unit may not start mdm's tools.
    let other = format!("[Service]\nExecStart={bin}caddy\n");
    assert!(check_unit("hello", "u", other.as_bytes()).is_err());
}

#[test]
fn the_built_in_pins_name_the_archives_the_package_reads() {
    // Catches: the verb building from other pins than hostkeeper-mdm-core's (the archives
    // `release mdm-fetch` left would then be refused, or other ones read).
    let dir = scratch("mdm-package-built-in");
    let (_, commit) = history();
    let r = request(&dir, &commit, "out");
    let err = package_dated(&r, || Ok(1)).expect_err("no archive is there");
    let pins = Pins::built_in().expect("the built-in pins");
    let first: PathBuf = dir.join("archives").join(pins.get(Tool::Nanohub).archive());
    assert!(
        matches!(&err, Error::Io { what, .. } if what.starts_with(&format!("read {}", first.display()))),
        "{err:?}"
    );
}
