use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version as ConfigVersion};

use super::*;
use crate::release::contract::{Commit, Package, Service};
use crate::release::fakes::{scratch, time_of};
use crate::release::file_sha256;
use crate::release::package::tests::{
    Unseal, built, history, host, read_back, shape_problems, unpack,
};
use crate::release::package::{
    FAIL_READY_ONLY_HELLO, PXE_LINUX_ONLY, PXE_TRUSTED_ROOT_REFUSED, PXE_UNIT_REFUSED, Request,
    check_unit, package,
};
use crate::release::{Error, contract};

/// The packaged binary; nothing reads it as a program.
const BINARY: &[u8] = b"the pxe binary";

/// The committed goldens of the package for version [`VERSION`] on linux-x86_64: what a Linux
/// package holds, byte for byte.
const ENTRIES: &str = include_str!("../../../../../../packaging/pxe/linux/entries.txt");
const MANIFEST: &str = include_str!("../../../../../../packaging/pxe/linux/manifest.toml");
const UNIT: &str = include_str!("../../../../../../packaging/pxe/linux/units/pxe.service");

/// The version of the goldens.
const VERSION: &str = "1.2.3";

fn version() -> contract::Version {
    VERSION.parse().expect("a version")
}

/// A request for the pxe package of `platform` written under `dir`/`out`, with the binary
/// written beside it.
fn request(dir: &Path, commit: &Commit, platform: Platform, out: &str) -> Request {
    std::fs::write(dir.join("pxe"), BINARY).expect("write the binary");
    Request {
        service: Service::Pxe,
        platform,
        version: version(),
        commit: commit.clone(),
        binary: dir.join("pxe"),
        unit: None,
        fail_ready: false,
        trusted_root: None,
        out: dir.join(out),
    }
}

/// The entries of an archive as the goldens list them: one `<mode> <path>` line each.
fn listing(package: &[u8]) -> String {
    read_back(package)
        .iter()
        .map(|e| format!("{:04o} {}\n", e.mode, e.name))
        .collect()
}

/// The data of the entry `name` of an archive, as text.
fn text_of(package: &[u8], name: &str) -> String {
    let data = read_back(package)
        .into_iter()
        .find(|e| e.name == name)
        .map(|e| e.data)
        .expect("an entry");
    String::from_utf8(data).expect("UTF-8 text")
}

/// Builds the pxe package of `platform` and returns its bytes.
fn built_for(name: &str, platform: Platform) -> Vec<u8> {
    let dir = scratch(name);
    let (history, commit) = history();
    let r = request(&dir, &commit, platform, "out");
    package(&history, &r).expect("package");
    built(&r)
}

#[test]
fn the_linux_package_holds_the_committed_goldens_byte_for_byte() {
    // Catches: a change to the entries, their order or modes, the manifest or the systemd unit
    // of the pxe package that the committed goldens (the packaging/pxe/linux files, which the
    // privileged proof and the runbook quote) do not move with.
    let bytes = built_for("pxe-goldens", Platform::LinuxX86_64);
    assert_eq!(listing(&bytes), ENTRIES);
    assert_eq!(text_of(&bytes, "manifest.toml"), MANIFEST);
    assert_eq!(text_of(&bytes, "units/pxe.service"), UNIT);
    assert_eq!(text_of(&bytes, "bin/pxe").as_bytes(), BINARY);
    assert_eq!(shape_problems(&bytes, time_of(1)), Vec::<String>::new());
}

#[test]
fn the_linux_platforms_differ_by_file_name_and_manifest_platform_only() {
    // Catches: per-platform code in the pxe package: bytes, digest or digest file that depend on
    // the architecture beyond the name of the file and the manifest's own platform line.
    let dir = scratch("pxe-platforms");
    let (history, commit) = history();
    let x86 = request(&dir, &commit, Platform::LinuxX86_64, "x86");
    let arm = request(&dir, &commit, Platform::LinuxArm64, "arm");
    let lines_x86 = package(&history, &x86).expect("package x86");
    let lines_arm = package(&history, &arm).expect("package arm");
    for (r, name, lines) in [
        (&x86, "pxe-1.2.3-linux-x86_64.tar.zst", &lines_x86),
        (&arm, "pxe-1.2.3-linux-arm64.tar.zst", &lines_arm),
    ] {
        let hex = file_sha256(&r.out.join(name)).expect("hash");
        assert_eq!(
            lines,
            &[format!("{name}: sha256 {hex}"), format!("sha256={hex}")]
        );
        assert_eq!(
            std::fs::read_to_string(r.out.join(format!("{name}.sha256"))).expect("digest"),
            format!("{hex}  {name}\n")
        );
        let mut written: Vec<String> = std::fs::read_dir(&r.out)
            .expect("the output directory")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        written.sort();
        assert_eq!(written, [name.to_owned(), format!("{name}.sha256")]);
    }
    let (x86_bytes, arm_bytes) = (built(&x86), built(&arm));
    assert_eq!(listing(&x86_bytes), listing(&arm_bytes));
    assert_eq!(text_of(&x86_bytes, "manifest.toml"), MANIFEST);
    assert_eq!(
        text_of(&arm_bytes, "manifest.toml"),
        MANIFEST.replace("platform = \"linux-x86_64\"", "platform = \"linux-arm64\"")
    );
    for (x, a) in read_back(&x86_bytes).iter().zip(&read_back(&arm_bytes)) {
        if x.name != "manifest.toml" {
            assert_eq!(x.data, a.data, "{}", x.name);
        }
    }
}

#[test]
fn the_package_is_deterministic() {
    // Catches: an archive whose bytes depend on the run (a clock, an unsorted listing).
    let a = built_for("pxe-deterministic-a", Platform::LinuxArm64);
    let b = built_for("pxe-deterministic-b", Platform::LinuxArm64);
    assert_eq!(a, b);
}

/// The manifest of the package of `platform` loaded with the manifest loader.
fn loaded(name: &str, platform: Platform) -> Manifest {
    let bytes = built_for(name, platform);
    let version_dir = scratch(&format!("{name}-unpacked"));
    unpack(&bytes, &version_dir);
    let expected = Expected {
        name: ServiceName::new("pxe").expect("a service name"),
        version: ConfigVersion::new(VERSION).expect("a version"),
        platform: platform.name().parse().expect("a platform"),
    };
    Manifest::load(&version_dir, &expected, &host(0)).expect("the manifest loads")
}

#[test]
fn the_manifest_loads_and_runs_the_one_unit_with_the_hook_params() {
    // Catches: a manifest the daemon's loader refuses (a run unit missing from units/, a hook
    // without bounds, a command that is not the package's own binary), or params that do not name
    // the release, settings file, boot directory and unit the hook reads.
    for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
        let manifest = loaded(&format!("pxe-loader-{}", platform.name()), platform);
        assert_eq!(manifest.name.as_str(), "pxe");
        assert_eq!(manifest.run, ["pxe.service"]);
        assert_eq!(
            manifest.unit_files,
            BTreeSet::from(["pxe.service".to_owned()])
        );
        assert_eq!(manifest.hook.command, ["bin/pxe"]);
        assert_eq!(manifest.hook.verify_deadline_secs, 300);
        assert_eq!(manifest.health_url, None);
        let param = |key: &str| manifest.params.get(key).and_then(|v| v.as_str());
        assert_eq!(
            param("release"),
            Some("https://github.com/komira-ai/hostkeeper/releases/download/v1.2.3")
        );
        assert_eq!(param("settings"), Some("/etc/pxe/pxe.toml"));
        assert_eq!(param("boot_dir"), Some("/var/lib/pxe"));
        assert_eq!(param("unit"), Some("pxe.service"));
        assert_eq!(manifest.params.len(), 4);
    }
}

#[test]
fn the_package_unpacks_with_the_daemons_unpacker() {
    // Catches: a pxe package the daemon's unpacker refuses: an entry outside the four top-level
    // names, a mode bit it does not allow, a digest that is not the archive's or a manifest it
    // cannot load.
    for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
        let dir = scratch(&format!("pxe-daemon-{}", platform.name()));
        let (history, commit) = history();
        let running = hostkeeper_unpack::running_hostkeeper().to_string();
        let mut req = request(&dir, &commit, platform, "out");
        req.version = running.parse().expect("the workspace version");
        package(&history, &req).expect("package");
        let archive = req
            .out
            .join(Package::Pxe.package_name(&req.version, platform));
        let packages = dir.join("packages");
        std::fs::create_dir(&packages).expect("the packages directory");
        let unseal = Unseal(packages.clone());
        let meta = std::fs::metadata(&packages).expect("stat the packages directory");
        let owner = hostkeeper_unpack::Owner {
            uid: std::os::unix::fs::MetadataExt::uid(&meta),
            gid: std::os::unix::fs::MetadataExt::gid(&meta),
        };
        let fs = hostkeeper_unpack::HostFs::open(&packages, owner).expect("open the packages");
        let order = hostkeeper_unpack::UnpackOrder {
            service: ServiceName::new("pxe").expect("a service name"),
            version: ConfigVersion::new(&running).expect("a version"),
            platform: platform.name().parse().expect("a platform"),
            package_digest: hostkeeper_config::daemon::Sha256Hex::parse(
                &file_sha256(&archive).expect("hash the package"),
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
        assert_eq!(
            unpacked.manifest.unit_files,
            BTreeSet::from(["pxe.service".to_owned()])
        );
        let binary = std::fs::read(packages.join("pxe").join(&running).join("bin/pxe"))
            .expect("the unpacked binary");
        assert_eq!(binary, BINARY);
        drop(unseal);
        assert!(!packages.exists(), "the unpacked tree is removed");
    }
}

#[test]
fn the_unit_starts_serve_through_current_and_passes_the_unit_check() {
    // Catches: a unit that starts a version directory or another binary, or names a package file
    // by any path but pxe's own `current` (the check of design section 4.7), or a golden unit the
    // check would refuse.
    assert_eq!(
        check_unit("pxe", "units/pxe.service", UNIT.as_bytes()),
        Ok(())
    );
    let versioned = UNIT.replace("pxe/current/", "pxe/1.2.3/");
    assert_eq!(
        check_unit("pxe", "u", versioned.as_bytes()),
        Err(Error::Failed(
            "u has no ExecStart line naming /var/lib/hostkeeper/packages/pxe/current/bin/pxe"
                .to_owned()
        ))
    );
    let foreign = format!("{UNIT}WorkingDirectory=/var/lib/hostkeeper/packages/hello/current/\n");
    assert_eq!(
        check_unit("pxe", "u", foreign.as_bytes()),
        Err(Error::Failed(
            "u names a package file by a path other than /var/lib/hostkeeper/packages/pxe/current/"
                .to_owned()
        ))
    );
}

#[test]
fn the_description_alone_decides_the_text() {
    // Catches: a name, unit, settings file or boot directory written in the manifest and the unit
    // separately, so that changing one leaves the other behind, or a release URL that is not the
    // repository's download URL of the version's tag.
    let other = Description {
        name: "other",
        unit: "other.service",
        settings: "/etc/other/other.toml",
        boot_dir: "/var/lib/other",
    };
    let contents = other.contents(Platform::LinuxArm64, &version());
    assert!(contents.manifest.contains("name = \"other\"\n"));
    assert!(contents.manifest.contains("command = [\"bin/other\"]\n"));
    assert!(contents.manifest.contains("run = [\"other.service\"]\n"));
    assert!(contents.manifest.contains("platform = \"linux-arm64\"\n"));
    assert!(contents.manifest.ends_with(
        "[params]\n\
                 release = \"https://github.com/komira-ai/hostkeeper/releases/download/v1.2.3\"\n\
                 settings = \"/etc/other/other.toml\"\n\
                 boot_dir = \"/var/lib/other\"\n\
                 unit = \"other.service\"\n"
    ));
    assert_eq!(contents.units.len(), 1);
    assert_eq!(contents.units[0].file, "other.service");
    assert_eq!(contents.units[0].origin, "units/other.service");
    assert!(contents.trusted_root.is_none());
    let unit = String::from_utf8(contents.units[0].data.clone()).expect("text");
    assert!(unit.contains(
        "ExecStart=/var/lib/hostkeeper/packages/other/current/bin/other serve --notify-systemd \
         --settings /etc/other/other.toml --dir /var/lib/other/current\n"
    ));
    assert!(unit.contains("Description=other, the boot server of network install\n"));
}

#[test]
fn the_release_url_is_the_repositorys_download_base_and_the_versions_tag() {
    // Catches: a release URL built from another repository, a missing `v`, or a version other
    // than the package's own.
    assert_eq!(
        release_url(&version()),
        "https://github.com/komira-ai/hostkeeper/releases/download/v1.2.3"
    );
    assert_eq!(
        release_url(&"0.1.70".parse().expect("a version")),
        "https://github.com/komira-ai/hostkeeper/releases/download/v0.1.70"
    );
}

#[test]
fn pxe_takes_no_unit_no_fail_ready_no_trusted_root_and_no_macos() {
    // Catches: a pxe package built from a unit file or trusted root given on the command line
    // (its unit is rendered and it carries none), from a flag only hello has, or for macOS, which
    // has no pxe package; and a refusal that still writes a package.
    let dir = scratch("pxe-refusals");
    let (history, commit) = history();
    let refuse = |r: Request, why: &str| {
        assert_eq!(package(&history, &r), Err(Error::Failed(why.to_owned())));
        assert!(!r.out.exists(), "nothing is written on a refusal");
    };
    let mut with_unit = request(&dir, &commit, Platform::LinuxX86_64, "o1");
    with_unit.unit = Some(dir.join("pxe.service"));
    refuse(with_unit, PXE_UNIT_REFUSED);
    let mut failing = request(&dir, &commit, Platform::LinuxX86_64, "o2");
    failing.fail_ready = true;
    refuse(failing, FAIL_READY_ONLY_HELLO);
    let mut rooted = request(&dir, &commit, Platform::LinuxArm64, "o3");
    rooted.trusted_root = Some(dir.join("trusted_root.json"));
    refuse(rooted, PXE_TRUSTED_ROOT_REFUSED);
    refuse(
        request(&dir, &commit, Platform::MacosArm64, "o4"),
        PXE_LINUX_ONLY,
    );
    let mut mac_with_unit = request(&dir, &commit, Platform::MacosArm64, "o5");
    mac_with_unit.unit = Some(dir.join("pxe.service"));
    refuse(mac_with_unit, PXE_LINUX_ONLY);
}

#[test]
fn the_refusal_texts_are_exact() {
    // Catches: a refusal that does not say which flag or platform the pxe package turns away.
    assert_eq!(
        PXE_UNIT_REFUSED,
        "the pxe package takes no --unit: its unit is rendered, the same for both platforms"
    );
    assert_eq!(
        PXE_TRUSTED_ROOT_REFUSED,
        "the pxe package takes no --trusted-root: only hostkeeper's package carries the trusted root"
    );
    assert_eq!(
        PXE_LINUX_ONLY,
        "the pxe package is for the Linux platforms only: it boots machines from a Linux host"
    );
    assert_eq!(
        FAIL_READY_ONLY_HELLO,
        "--fail-ready is an option of the hello package; hostkeeper and pxe have no such flag"
    );
}
