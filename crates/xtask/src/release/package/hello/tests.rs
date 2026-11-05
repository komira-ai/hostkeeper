use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest, ManifestRefusal};
use hostkeeper_config::names::{ServiceName, Version as ConfigVersion};

use super::*;
use crate::release::contract::{Commit, Service};
use crate::release::fakes::{scratch, time_of};
use crate::release::file_sha256;
use crate::release::package::tests::{
    Unseal, built, history, host, read_back, shape_problems, unpack,
};
use crate::release::package::{
    FAIL_READY_ONLY_HELLO, MACOS_LATER, Request, UNIT_REFUSED, UNIT_REQUIRED, check_unit, package,
};

/// The packaged binary; nothing reads it as a program.
const BINARY: &[u8] = b"the hello binary";

/// The committed goldens of the package for version [`VERSION`]: what a Linux package and a macOS
/// package hold, byte for byte.
const LINUX_ENTRIES: &str =
    include_str!("../../../../../../examples/hello/package/linux/entries.txt");
const LINUX_MANIFEST: &str =
    include_str!("../../../../../../examples/hello/package/linux/manifest.toml");
const LINUX_UNIT: &str =
    include_str!("../../../../../../examples/hello/package/linux/units/hello@.service");
const MACOS_ENTRIES: &str =
    include_str!("../../../../../../examples/hello/package/macos/entries.txt");
const MACOS_MANIFEST: &str =
    include_str!("../../../../../../examples/hello/package/macos/manifest.toml");
const MACOS_PLIST_A: &str =
    include_str!("../../../../../../examples/hello/package/macos/units/hello.a.plist");
const MACOS_PLIST_B: &str =
    include_str!("../../../../../../examples/hello/package/macos/units/hello.b.plist");

/// The version of the goldens.
const VERSION: &str = "1.2.3";

fn version() -> Version {
    VERSION.parse().expect("a version")
}

/// A request for the hello package of `platform` written under `dir`/`out`, with the binary
/// written beside it.
fn request(
    dir: &Path,
    commit: &Commit,
    platform: Platform,
    fail_ready: bool,
    out: &str,
) -> Request {
    std::fs::write(dir.join("hello"), BINARY).expect("write the binary");
    Request {
        service: Service::Hello,
        platform,
        version: version(),
        commit: commit.clone(),
        binary: dir.join("hello"),
        unit: None,
        fail_ready,
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

/// Builds the hello package of `platform` (with `--fail-ready` or without) and returns its bytes.
fn built_for(name: &str, platform: Platform, fail_ready: bool) -> Vec<u8> {
    let dir = scratch(name);
    let (history, commit) = history();
    let r = request(&dir, &commit, platform, fail_ready, "out");
    package(&history, &r).expect("package");
    built(&r)
}

#[test]
fn the_linux_package_holds_the_committed_goldens_byte_for_byte() {
    // Catches: a change to the entries, their order or modes, the manifest or the systemd unit of
    // the hello package for Linux that the committed goldens (the examples/hello/package/linux
    // files, which the farm's proof and the docs quote) do not move with.
    let bytes = built_for("hello-linux-goldens", Platform::LinuxX86_64, false);
    assert_eq!(listing(&bytes), LINUX_ENTRIES);
    assert_eq!(text_of(&bytes, "manifest.toml"), LINUX_MANIFEST);
    assert_eq!(text_of(&bytes, "units/hello@.service"), LINUX_UNIT);
    assert_eq!(text_of(&bytes, "bin/hello").as_bytes(), BINARY);
    assert_eq!(shape_problems(&bytes, time_of(1)), Vec::<String>::new());
}

#[test]
fn the_macos_package_holds_the_committed_goldens_byte_for_byte() {
    // Catches: a change to the entries, the manifest or either launchd plist of the hello package
    // for macOS that the committed goldens (the examples/hello/package/macos files) do not move
    // with, or a Linux unit in a macOS package (a package holds one kind of unit).
    let bytes = built_for("hello-macos-goldens", Platform::MacosArm64, false);
    assert_eq!(listing(&bytes), MACOS_ENTRIES);
    assert_eq!(text_of(&bytes, "manifest.toml"), MACOS_MANIFEST);
    assert_eq!(text_of(&bytes, "units/hello.a.plist"), MACOS_PLIST_A);
    assert_eq!(text_of(&bytes, "units/hello.b.plist"), MACOS_PLIST_B);
    assert_eq!(text_of(&bytes, "bin/hello").as_bytes(), BINARY);
    assert_eq!(shape_problems(&bytes, time_of(1)), Vec::<String>::new());
}

#[test]
fn the_linux_platforms_differ_by_file_name_only() {
    // Catches: per-platform code in the Linux package: bytes, digest or digest file that depend
    // on the architecture beyond the name of the file.
    let dir = scratch("hello-linux-platforms");
    let (history, commit) = history();
    let x86 = request(&dir, &commit, Platform::LinuxX86_64, false, "x86");
    let arm = request(&dir, &commit, Platform::LinuxArm64, false, "arm");
    let lines_x86 = package(&history, &x86).expect("package x86");
    let lines_arm = package(&history, &arm).expect("package arm");
    assert_eq!(built(&x86), built(&arm));
    let hex = file_sha256(&x86.out.join("hello-1.2.3-linux-x86_64.tar.zst")).expect("hash");
    assert_eq!(
        lines_x86,
        [
            format!("hello-1.2.3-linux-x86_64.tar.zst: sha256 {hex}"),
            format!("sha256={hex}")
        ]
    );
    assert_eq!(
        lines_arm,
        [
            format!("hello-1.2.3-linux-arm64.tar.zst: sha256 {hex}"),
            format!("sha256={hex}")
        ]
    );
    for (r, name) in [
        (&x86, "hello-1.2.3-linux-x86_64.tar.zst"),
        (&arm, "hello-1.2.3-linux-arm64.tar.zst"),
    ] {
        assert_eq!(
            std::fs::read_to_string(r.out.join(format!("{name}.sha256"))).expect("digest"),
            format!("{hex}  {name}\n")
        );
    }
    let macos = built_for("hello-macos-differs", Platform::MacosArm64, false);
    assert_ne!(macos, built(&x86));
}

#[test]
fn the_package_is_deterministic() {
    // Catches: an archive whose bytes depend on the run (a clock, an unsorted listing).
    let a = built_for("hello-deterministic-a", Platform::MacosArm64, false);
    let b = built_for("hello-deterministic-b", Platform::MacosArm64, false);
    assert_eq!(a, b);
}

/// The manifest of the package of `platform` loaded with the manifest loader, as hostkeeper
/// `running`.
fn loaded(name: &str, platform: Platform, running: u64) -> Manifest {
    let dir = scratch(name);
    let (history, commit) = history();
    let r = request(&dir, &commit, platform, false, "out");
    package(&history, &r).expect("package");
    let version_dir = scratch(&format!("{name}-unpacked"));
    unpack(&built(&r), &version_dir);
    let expected = Expected {
        name: ServiceName::new("hello").expect("a service name"),
        version: ConfigVersion::new(VERSION).expect("a version"),
    };
    Manifest::load(&version_dir, &expected, &host(running)).expect("the manifest loads")
}

#[test]
fn the_linux_manifest_loads_and_names_the_two_slot_instances_of_the_template() {
    // Catches: a Linux manifest the daemon's loader refuses (a run unit that is not an instance
    // of the template in units/, a hook without bounds, a health URL that is not loopback), or one
    // that gives the hook another unit kind, port or record directory than the units use.
    let manifest = loaded("hello-linux-loader", Platform::LinuxX86_64, 0);
    assert_eq!(manifest.name.as_str(), "hello");
    assert_eq!(manifest.run, ["hello@a.service", "hello@b.service"]);
    assert_eq!(
        manifest.unit_files,
        BTreeSet::from(["hello@.service".to_owned()])
    );
    assert_eq!(manifest.hook.command, ["bin/hello", "update-hook"]);
    assert_eq!(manifest.hook.verify_deadline_secs, 120);
    assert_eq!(
        manifest.health_url.as_deref(),
        Some("http://127.0.0.1:9310/readyz")
    );
    assert_eq!(
        manifest.params.get("units").and_then(|v| v.as_str()),
        Some("systemd")
    );
    assert_eq!(
        manifest.params.get("port").and_then(|v| v.as_integer()),
        Some(9310)
    );
    assert_eq!(
        manifest.params.get("record_dir").and_then(|v| v.as_str()),
        Some("/var/lib/updater/hooks/hello")
    );
}

#[test]
fn the_macos_manifest_loads_and_names_the_two_slot_plists() {
    // Catches: a macOS manifest the loader refuses (a label that is not the service's, a plist
    // missing from units/), or one whose params still say systemd.
    let manifest = loaded("hello-macos-loader", Platform::MacosArm64, 0);
    assert_eq!(manifest.run, ["hello.a", "hello.b"]);
    assert_eq!(
        manifest.unit_files,
        BTreeSet::from(["hello.a.plist".to_owned(), "hello.b.plist".to_owned()])
    );
    assert_eq!(
        manifest.params.get("units").and_then(|v| v.as_str()),
        Some("launchd")
    );
    assert_eq!(
        manifest.health_url.as_deref(),
        Some("http://127.0.0.1:9310/readyz")
    );
}

#[test]
fn a_daemon_older_than_the_first_release_refuses_the_package() {
    // Catches: a `min_hostkeeper` other than the one the manifest states, or a package that a
    // daemon older than the first release would accept.
    let dir = scratch("hello-min-hostkeeper");
    let (history, commit) = history();
    let r = request(&dir, &commit, Platform::LinuxX86_64, false, "out");
    package(&history, &r).expect("package");
    let version_dir = scratch("hello-min-hostkeeper-unpacked");
    unpack(&built(&r), &version_dir);
    let expected = Expected {
        name: ServiceName::new("hello").expect("a service name"),
        version: ConfigVersion::new(VERSION).expect("a version"),
    };
    let older = HostkeeperVersion::parse("0.0.9").expect("a hostkeeper version");
    assert_eq!(
        Manifest::load(&version_dir, &expected, &older),
        Err(ManifestRefusal::DaemonTooOld {
            needs: host(0),
            running: older
        })
    );
}

/// The macOS adapter of the unpacker, answering that nothing is quarantined and every signature is
/// valid (the packaged binary of a test is not a Mach-O file, so none is asked).
struct Fine;

impl hostkeeper_unpack::MacVerify for Fine {
    fn quarantine(&self, _: &Path) -> Result<bool, hostkeeper_unpack::VerifyIoError> {
        Ok(false)
    }

    fn signature(
        &self,
        _: &Path,
    ) -> Result<hostkeeper_unpack::SignatureVerdict, hostkeeper_unpack::VerifyIoError> {
        Ok(hostkeeper_unpack::SignatureVerdict::Valid)
    }
}

/// Builds the hello package of `platform` at the running daemon's version and unpacks it with the
/// daemon's unpacker; the unit files it unpacked and the binary's bytes.
fn unpacked_by_the_daemon(name: &str, platform: Platform) -> (BTreeSet<String>, Vec<u8>) {
    let dir = scratch(name);
    let (history, commit) = history();
    let running = hostkeeper_unpack::running_hostkeeper().to_string();
    let mut req = request(&dir, &commit, platform, false, "out");
    req.version = running.parse().expect("the workspace version");
    package(&history, &req).expect("package");
    let archive = req
        .out
        .join(req.service.package_name(&req.version, platform));
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
        service: ServiceName::new("hello").expect("a service name"),
        version: ConfigVersion::new(&running).expect("a version"),
        package_digest: hostkeeper_config::daemon::Sha256Hex::parse(
            &file_sha256(&archive).expect("hash the package"),
        )
        .expect("a digest"),
    };
    let limits = hostkeeper_unpack::Limits {
        max_unpacked_bytes: 1 << 30,
        max_entries: 1000,
    };
    let fine = Fine;
    let verifier = match platform {
        Platform::MacosArm64 => hostkeeper_unpack::Platform::MacOs(&fine),
        Platform::LinuxX86_64 | Platform::LinuxArm64 => hostkeeper_unpack::Platform::Linux,
    };
    let unpacked =
        hostkeeper_unpack::unpack(&archive, &order, &limits, &fs, verifier).expect("unpack");
    assert!(unpacked.fresh);
    let binary = std::fs::read(packages.join("hello").join(&running).join("bin/hello"))
        .expect("the unpacked binary");
    drop(unseal);
    assert!(!packages.exists(), "the unpacked tree is removed");
    (unpacked.manifest.unit_files, binary)
}

#[test]
fn each_hello_package_unpacks_with_the_daemons_unpacker() {
    // Catches: a hello package the daemon's unpacker refuses on any platform: an entry outside
    // the four top-level names, a mode bit it does not allow, a digest that is not the archive's,
    // a manifest it cannot load, or a macOS package it would refuse for its quarantine or
    // signature checks.
    let (linux, binary) = unpacked_by_the_daemon("hello-daemon-linux", Platform::LinuxArm64);
    assert_eq!(linux, BTreeSet::from(["hello@.service".to_owned()]));
    assert_eq!(binary, BINARY);
    let (macos, binary) = unpacked_by_the_daemon("hello-daemon-macos", Platform::MacosArm64);
    assert_eq!(
        macos,
        BTreeSet::from(["hello.a.plist".to_owned(), "hello.b.plist".to_owned()])
    );
    assert_eq!(binary, BINARY);
}

/// The lines of `text` that differ from the same line of `other`, as `(line, other's line)`.
fn changed_lines<'a>(text: &'a str, other: &'a str) -> Vec<(&'a str, &'a str)> {
    assert_eq!(text.lines().count(), other.lines().count());
    text.lines()
        .zip(other.lines())
        .filter(|(a, b)| a != b)
        .collect()
}

#[test]
fn fail_ready_changes_exactly_the_exec_start_line_of_the_unit() {
    // Catches: a `--fail-ready` that lands in the manifest, the binary or any unit line but the
    // command line, or that is dropped from the unit.
    let plain = built_for("hello-fail-ready-plain", Platform::LinuxX86_64, false);
    let failing = built_for("hello-fail-ready-fail", Platform::LinuxX86_64, true);
    assert_eq!(listing(&plain), listing(&failing));
    for name in ["manifest.toml", "bin/hello"] {
        assert_eq!(text_of(&plain, name), text_of(&failing, name), "{name}");
    }
    assert_eq!(
        changed_lines(
            &text_of(&failing, "units/hello@.service"),
            &text_of(&plain, "units/hello@.service")
        ),
        [(
            "ExecStart=/var/lib/hostkeeper/packages/hello/current/bin/hello --port 9310 --fail-ready",
            "ExecStart=/var/lib/hostkeeper/packages/hello/current/bin/hello --port 9310"
        )]
    );
}

#[test]
fn fail_ready_changes_exactly_the_program_arguments_of_each_plist() {
    // Catches: a `--fail-ready` that lands in a plist key other than ProgramArguments, in only one
    // slot's plist, or after another argument.
    let plain = built_for("hello-fail-ready-plain-mac", Platform::MacosArm64, false);
    let failing = built_for("hello-fail-ready-fail-mac", Platform::MacosArm64, true);
    assert_eq!(
        text_of(&plain, "manifest.toml"),
        text_of(&failing, "manifest.toml")
    );
    for plist in ["units/hello.a.plist", "units/hello.b.plist"] {
        let with = text_of(&failing, plist);
        let without = text_of(&plain, plist);
        assert_eq!(
            with.replace("\t\t<string>--fail-ready</string>\n", ""),
            without,
            "{plist}"
        );
        assert!(
            with.contains(
                "\t\t<string>9310</string>\n\t\t<string>--fail-ready</string>\n\t</array>\n"
            ),
            "{plist}"
        );
    }
}

#[test]
fn hello_takes_no_unit_and_hostkeeper_takes_no_fail_ready_and_needs_its_unit() {
    // Catches: a hello package built from a unit file given on the command line (its units are
    // rendered), a hostkeeper package given a flag only hello has, one built without its unit, or
    // a refusal that still writes a package.
    let dir = scratch("hello-refusals");
    let (history, commit) = history();
    let refuse = |mut r: Request, why: &str| {
        assert_eq!(package(&history, &r), Err(Error::Failed(why.to_owned())));
        assert!(!r.out.exists(), "nothing is written on a refusal");
        r.out = dir.join("unused");
    };
    let mut hello = request(&dir, &commit, Platform::LinuxX86_64, false, "o1");
    hello.unit = Some(dir.join("hello.service"));
    refuse(hello, UNIT_REFUSED);

    let hostkeeper = |platform: Platform, unit: bool, fail_ready: bool, out: &str| {
        let mut r = request(&dir, &commit, platform, fail_ready, out);
        r.service = Service::Hostkeeper;
        r.unit = unit.then(|| dir.join("hostkeeper.service"));
        r
    };
    refuse(
        hostkeeper(Platform::LinuxX86_64, true, true, "o2"),
        FAIL_READY_ONLY_HELLO,
    );
    refuse(
        hostkeeper(Platform::LinuxX86_64, false, false, "o3"),
        UNIT_REQUIRED,
    );
    // macOS is refused for hostkeeper first, whatever else is wrong with the request.
    refuse(
        hostkeeper(Platform::MacosArm64, false, true, "o4"),
        MACOS_LATER,
    );
}

#[test]
fn the_unit_check_holds_each_service_to_its_own_current_path() {
    // Catches: a unit check that still names hostkeeper's path for every service, so that the
    // hello unit is refused, or that lets a unit of one service start another's binary or name a
    // version directory.
    let hello = LINUX_UNIT;
    assert_eq!(
        check_unit("hello", "hello.service", hello.as_bytes()),
        Ok(())
    );
    let refused = |service: &str, unit: &str| check_unit(service, "u", unit.as_bytes());
    assert_eq!(
        refused("hostkeeper", hello),
        Err(Error::Failed(
            "u has no ExecStart line naming /var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper"
                .to_owned()
        ))
    );
    let hostkeeper_unit = hello.replace("hello", "hostkeeper");
    assert_eq!(
        refused("hello", &hostkeeper_unit),
        Err(Error::Failed(
            "u has no ExecStart line naming /var/lib/hostkeeper/packages/hello/current/bin/hello"
                .to_owned()
        ))
    );
    let versioned = hello.replace("hello/current/", "hello/1.2.3/");
    assert_eq!(
        refused("hello", &versioned),
        Err(Error::Failed(
            "u has no ExecStart line naming /var/lib/hostkeeper/packages/hello/current/bin/hello"
                .to_owned()
        ))
    );
    let foreign =
        format!("{hello}WorkingDirectory=/var/lib/hostkeeper/packages/hostkeeper/current/\n");
    assert_eq!(
        refused("hello", &foreign),
        Err(Error::Failed(
            "u names a package file by a path other than /var/lib/hostkeeper/packages/hello/current/"
                .to_owned()
        ))
    );
}

#[test]
fn text_a_unit_cannot_hold_is_refused_naming_the_file() {
    // Catches: a plist that cannot be written (a label with a space, a control character in a
    // path) reported as success, or an error that does not name the unit file.
    let bad_label = Description {
        name: "he llo",
        ..HELLO
    };
    assert_eq!(
        bad_label.contents(Kind::Launchd, &version(), false).err(),
        Some(Error::Failed(
            "units/he llo.a.plist: launchd label \"he llo.a\" holds the byte 0x20, which a label may not"
                .to_owned()
        ))
    );
    let bad_path = Description {
        record_dir: "/var/lib/updater/hooks/hello\n",
        ..HELLO
    };
    assert_eq!(
        bad_path.contents(Kind::Launchd, &version(), false).err(),
        Some(Error::Failed(
            "units/hello.a.plist: plist text \"/var/lib/updater/hooks/hello\\n/slots/a\" holds a control character"
                .to_owned()
        ))
    );
    // The systemd kind has no plist to refuse.
    assert!(bad_path.contents(Kind::Systemd, &version(), false).is_ok());
}

#[test]
fn the_description_alone_decides_the_text() {
    // Catches: a port, slot, record directory or name written in the manifest and the units
    // separately, so that changing one leaves the other behind.
    let other = Description {
        name: "other",
        port: 8080,
        slots: ["x", "y"],
        record_dir: "/var/lib/updater/hooks/other",
        log_dir: "/var/log/other",
    };
    let systemd = other
        .contents(Kind::Systemd, &version(), false)
        .expect("contents");
    assert!(systemd.manifest.contains("name = \"other\"\n"));
    assert!(
        systemd
            .manifest
            .contains("command = [\"bin/other\", \"update-hook\"]\n")
    );
    assert!(
        systemd
            .manifest
            .contains("run = [\"other@x.service\", \"other@y.service\"]\n")
    );
    assert!(
        systemd
            .manifest
            .contains("url = \"http://127.0.0.1:8080/readyz\"\n")
    );
    assert!(systemd.manifest.contains("port = 8080\n"));
    assert!(
        systemd
            .manifest
            .contains("record_dir = \"/var/lib/updater/hooks/other\"\n")
    );
    let unit = String::from_utf8(systemd.units[0].data.clone()).expect("text");
    assert!(unit.contains("ConditionPathExists=/var/lib/updater/hooks/other/slots/%i\n"));
    assert!(
        unit.contains(
            "ExecStart=/var/lib/hostkeeper/packages/other/current/bin/other --port 8080\n"
        )
    );
    assert_eq!(systemd.units[0].file, "other@.service");
    let launchd = other
        .contents(Kind::Launchd, &version(), false)
        .expect("contents");
    assert!(
        launchd
            .manifest
            .contains("run = [\"other.x\", \"other.y\"]\n")
    );
    let files: Vec<&str> = launchd.units.iter().map(|u| u.file.as_str()).collect();
    assert_eq!(files, ["other.x.plist", "other.y.plist"]);
    let plist = String::from_utf8(launchd.units[1].data.clone()).expect("text");
    assert!(plist.contains("<key>/var/lib/updater/hooks/other/slots/y</key>"));
    assert!(plist.contains("<string>/var/log/other/other.y.err.log</string>"));
    assert!(plist.contains("<string>8080</string>"));
}
