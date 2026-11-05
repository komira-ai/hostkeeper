use std::collections::BTreeSet;
use std::io::Read;
use std::num::NonZeroU64;

use hostkeeper_config::manifest::{
    Expected, HostkeeperVersion, Manifest, ManifestRefusal, Timeouts,
};
use hostkeeper_config::names::{ServiceName, Version as ConfigVersion};

use super::*;
use crate::release::fakes::{FakeHistory, nth_commit, scratch, time_of};
use crate::release::file_sha256;
use crate::release::ports::PortError;

/// The packaged binary; nothing reads it as a program.
pub(super) const BINARY: &[u8] = b"the hostkeeper binary";
const UNIT: &str = include_str!("../../../tests/release/hostkeeper.service");
const NAMES: [&str; 5] = [
    "bin/",
    "bin/hostkeeper",
    "manifest.toml",
    "units/",
    "units/hostkeeper.service",
];

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A history of one commit, and that commit.
pub(super) fn history() -> (FakeHistory, Commit) {
    let mut history = FakeHistory::default();
    let commit = history.commit(None, &[("VERSION", Some("0.1\n"))]);
    (history, commit)
}

/// A request over a binary and the given unit text written under `dir`, writing to `out`. The
/// unit is passed for a Linux platform, and not for macos-arm64, which takes none.
fn request(dir: &Path, unit: &[u8], commit: &Commit, platform: Platform, out: &str) -> Request {
    std::fs::write(dir.join("hostkeeper"), BINARY).expect("write the binary");
    std::fs::write(dir.join("hostkeeper.service"), unit).expect("write the unit");
    Request {
        service: Service::Hostkeeper,
        platform,
        version: version(),
        commit: commit.clone(),
        binary: dir.join("hostkeeper"),
        unit: (platform != Platform::MacosArm64).then(|| dir.join("hostkeeper.service")),
        fail_ready: false,
        out: dir.join(out),
    }
}

/// One entry of an archive as read back.
pub(super) struct Seen {
    pub(super) name: String,
    pub(super) kind: tar::EntryType,
    pub(super) mode: u32,
    uid: u64,
    gid: u64,
    mtime: u64,
    user: String,
    group: String,
    ustar: bool,
    pub(super) data: Vec<u8>,
}

/// Decompresses and reads every entry of `package`.
pub(super) fn read_back(package: &[u8]) -> Vec<Seen> {
    let decoder = ruzstd::decoding::StreamingDecoder::new(package).expect("a zstd frame");
    let mut archive = tar::Archive::new(decoder);
    let mut seen = Vec::new();
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("an entry");
        let header = entry.header().clone();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).expect("entry bytes");
        seen.push(Seen {
            name: entry.path().expect("a path").to_string_lossy().into_owned(),
            kind: header.entry_type(),
            mode: header.mode().expect("a mode"),
            uid: header.uid().expect("a uid"),
            gid: header.gid().expect("a gid"),
            mtime: header.mtime().expect("an mtime"),
            user: header
                .username()
                .expect("utf-8")
                .unwrap_or_default()
                .to_owned(),
            group: header
                .groupname()
                .expect("utf-8")
                .unwrap_or_default()
                .to_owned(),
            ustar: header.as_ustar().is_some(),
            data,
        });
    }
    seen
}

/// The shape rules of design section 5.5, checked on the bytes of a package by this test alone (a
/// quick check beside the daemon's own unpacker, which the last test of this file runs): every entry that breaks one, and the list when it is not sorted and unique.
pub(super) fn shape_problems(package: &[u8], mtime: u64) -> Vec<String> {
    let seen = read_back(package);
    let mut problems = Vec::new();
    for e in &seen {
        let top = e.name.split('/').next().unwrap_or_default();
        let checks = [
            (
                ["manifest.toml", "bin", "units", "files"].contains(&top),
                "top-level name is outside manifest.toml, bin, units and files",
            ),
            (
                matches!(e.kind, tar::EntryType::Regular | tar::EntryType::Directory),
                "is neither a regular file nor a directory",
            ),
            (e.mode & 0o7000 == 0, "has a setuid, setgid or sticky bit"),
            (
                [0o555, 0o444].contains(&e.mode),
                "has a mode other than 555 or 444",
            ),
            (
                (e.uid == 0) & (e.gid == 0),
                "is not owned by uid 0 and gid 0",
            ),
            (e.mtime == mtime, "is not dated the commit time"),
        ];
        problems.extend(
            checks
                .iter()
                .filter(|(ok, _)| !ok)
                .map(|(_, why)| format!("{}: {why}", e.name)),
        );
    }
    let names: Vec<&str> = seen.iter().map(|e| e.name.as_str()).collect();
    let sorted_unique = names.windows(2).all(|w| w[0] < w[1]);
    problems.extend((!sorted_unique).then(|| "the names are not sorted and unique".to_owned()));
    problems
}

/// Unpacks `package` into `dir` as plain files, for the manifest loader (no bounds, no seal: the
/// daemon's own unpacker is run by the last test of this file).
pub(super) fn unpack(package: &[u8], dir: &Path) {
    for e in read_back(package) {
        if e.kind.is_dir() {
            std::fs::create_dir_all(dir.join(&e.name)).expect("create a directory");
        } else {
            std::fs::write(dir.join(&e.name), &e.data).expect("write a file");
        }
    }
}

pub(super) fn built(request: &Request) -> Vec<u8> {
    let name = Package::of(request.service, request.fail_ready)
        .expect("a package of the request")
        .package_name(&request.version, request.platform);
    std::fs::read(request.out.join(name)).expect("the package")
}

fn nonzero(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("nonzero")
}

fn expected() -> Expected {
    Expected {
        name: ServiceName::new("hostkeeper").expect("a service name"),
        version: ConfigVersion::new("0.1.5").expect("a version"),
    }
}

pub(super) fn host(patch: u64) -> HostkeeperVersion {
    HostkeeperVersion::parse(&format!("0.1.{patch}")).expect("a hostkeeper version")
}

/// The package of version 0.1.5 unpacked into a scratch directory of its own.
fn unpacked(name: &str) -> (crate::testdir::TestDir, crate::testdir::TestDir) {
    let dir = scratch(name);
    let (history, commit) = history();
    let r = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "out");
    package(&history, &r).expect("package");
    let version_dir = scratch(&format!("{name}-unpacked"));
    unpack(&built(&r), &version_dir);
    (dir, version_dir)
}

#[test]
fn package_is_deterministic_and_has_exactly_the_four_allowed_top_level_names() {
    // Catches: an archive whose bytes depend on the run (a clock, an unsorted listing, an owner
    // name), one holding a top-level name outside the four, or a check that cannot fail.
    let dir = scratch("package-deterministic");
    let (history, commit) = history();
    let a = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "a");
    let b = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "b");
    let lines_a = package(&history, &a).expect("package a");
    let lines_b = package(&history, &b).expect("package b");
    assert_eq!(lines_a, lines_b);
    let name = Package::Hostkeeper.package_name(&version(), Platform::LinuxX86_64);
    let bytes = built(&a);
    assert_eq!(bytes, built(&b));
    let digest_a = std::fs::read(a.out.join(format!("{name}.sha256"))).expect("digest a");
    assert_eq!(
        digest_a,
        std::fs::read(b.out.join(format!("{name}.sha256"))).expect("digest b")
    );

    let seen = read_back(&bytes);
    let names: Vec<&str> = seen.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, NAMES);
    let tops: BTreeSet<&str> = names
        .iter()
        .map(|n| n.split('/').next().expect("a first component"))
        .collect();
    assert!(tops.is_subset(&BTreeSet::from(["manifest.toml", "bin", "units", "files"])));
    for e in &seen {
        assert_eq!(
            (
                e.uid,
                e.gid,
                e.mtime,
                e.user.as_str(),
                e.group.as_str(),
                e.ustar
            ),
            (0, 0, time_of(1), "", "", true),
            "{}",
            e.name
        );
    }
    let modes: Vec<(bool, u32)> = seen.iter().map(|e| (e.kind.is_dir(), e.mode)).collect();
    assert_eq!(
        modes,
        [
            (true, 0o555),
            (false, 0o555),
            (false, 0o444),
            (true, 0o555),
            (false, 0o444)
        ]
    );
    assert_eq!(shape_problems(&bytes, time_of(1)), Vec::<String>::new());

    // The shape check refuses an archive with an extra top-level file, and a wrong date.
    let extra = compress(
        &archive(&[Entry::file("README", 0o644, b"x".to_vec())], time_of(1)).expect("archive"),
    );
    assert_eq!(
        shape_problems(&extra, time_of(1)),
        [
            "README: top-level name is outside manifest.toml, bin, units and files",
            "README: has a mode other than 555 or 444"
        ]
    );
    assert_eq!(
        shape_problems(&bytes, time_of(2)),
        NAMES
            .iter()
            .map(|n| format!("{n}: is not dated the commit time"))
            .collect::<Vec<_>>()
    );

    // The same check on the entry list before the archive is built.
    let bad = [Entry::file("README", 0o444, Vec::new())];
    assert_eq!(
        check_entries(&bad),
        Err(Error::Refused(vec![
            "README: top-level name is not manifest.toml, bin/, units/ or files/".to_owned()
        ]))
    );
    let escape = [Entry::file("files/../x", 0o444, Vec::new())];
    assert_eq!(
        check_entries(&escape),
        Err(Error::Refused(vec![
            "files/../x: has a .. component".to_owned()
        ]))
    );
}

#[test]
fn package_manifest_loads_with_the_b2_loader_and_names_the_hostkeeper_unit() {
    // Catches: a manifest the daemon's loader refuses (a missing hook table, a wrong name or
    // version, a run unit that is not in units/).
    let (_dir, version_dir) = unpacked("package-b2-loader");
    let manifest = Manifest::load(&version_dir, &expected(), &host(5)).expect("the manifest loads");
    assert_eq!(manifest.name.as_str(), "hostkeeper");
    assert_eq!(manifest.version.as_str(), "0.1.5");
    assert_eq!(manifest.min_hostkeeper, host(5));
    assert_eq!(manifest.run, ["hostkeeper.service"]);
    assert_eq!(
        manifest.unit_files,
        BTreeSet::from(["hostkeeper.service".to_owned()])
    );
}

#[test]
fn package_manifest_loads_with_the_manifest_loader_and_names_the_hostkeeper_unit() {
    // Catches: a package that a daemon one patch older would accept, a hook command or bound
    // other than the one the manifest block sets.
    let (_dir, version_dir) = unpacked("package-manifest-loader");
    assert_eq!(
        Manifest::load(&version_dir, &expected(), &host(4)),
        Err(ManifestRefusal::DaemonTooOld {
            needs: host(5),
            running: host(4)
        })
    );
    let manifest = Manifest::load(&version_dir, &expected(), &host(5)).expect("the manifest loads");
    assert_eq!(manifest.hook.command, ["bin/hostkeeper", "update-hook"]);
    assert_eq!(manifest.hook.verify_deadline_secs, 600);
    assert_eq!(
        manifest.hook.timeout_secs,
        Timeouts {
            prepare: Some(nonzero(300)),
            cutover: nonzero(300),
            verify: nonzero(300),
            rollback: nonzero(300),
            cleanup: nonzero(300),
            uninstall: None,
        }
    );
    assert_eq!(manifest.run, ["hostkeeper.service"]);
}

#[test]
fn the_manifest_text_is_exact() {
    // Catches: a changed key or value of the manifest that no loader check notices, or a hook
    // table that no longer says the daemon does not run it for its own package.
    assert_eq!(
        manifest(&version(), true),
        "schema = 1\nname = \"hostkeeper\"\nversion = \"0.1.5\"\nmin_hostkeeper = \"0.1.5\"\n\n\
         # The daemon does not run this hook for its own package: hostkeeper's self-update is\n\
         # compiled into the daemon. The table is here because every manifest has a bounded hook.\n\
         [hook]\ncommand = [\"bin/hostkeeper\", \"update-hook\"]\nverify_deadline_secs = 600\n\n\
         [hook.timeout_secs]\nprepare = 300\ncutover = 300\nverify = 300\nrollback = 300\n\
         cleanup = 300\n\n[units]\nrun = [\"hostkeeper.service\"]\n"
    );
    // Without units, the same text up to the hook table's end, and no `[units]`.
    let with_units = manifest(&version(), true);
    let without = manifest(&version(), false);
    assert_eq!(
        format!("{without}\n[units]\nrun = [\"hostkeeper.service\"]\n"),
        with_units
    );
    assert!(without.ends_with("cleanup = 300\n") & !without.contains("units"));
}

#[test]
fn the_archive_holds_the_binary_the_manifest_and_the_unit_bytes() {
    // Catches: an entry that holds other bytes than its input.
    let dir = scratch("package-contents");
    let (history, commit) = history();
    let r = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "out");
    package(&history, &r).expect("package");
    let seen = read_back(&built(&r));
    let by_name = |n: &str| {
        seen.iter()
            .find(|e| e.name == n)
            .map(|e| e.data.clone())
            .expect("an entry")
    };
    assert_eq!(by_name("bin/hostkeeper"), BINARY);
    assert_eq!(by_name("units/hostkeeper.service"), UNIT.as_bytes());
    assert_eq!(
        by_name("manifest.toml"),
        manifest(&version(), true).into_bytes()
    );
    assert_eq!(by_name("bin/"), Vec::<u8>::new());
}

#[test]
fn compress_round_trips_through_the_zstd_decoder() {
    // Catches: a compression the decoder of the daemon cannot read back to the same tar.
    let tar = archive(&[Entry::dir("bin/")], 7).expect("archive");
    let compressed = compress(&tar);
    let mut decoder =
        ruzstd::decoding::StreamingDecoder::new(compressed.as_slice()).expect("a zstd frame");
    let mut back = Vec::new();
    decoder.read_to_end(&mut back).expect("decompress");
    assert_eq!(back, tar);
}

#[test]
fn the_digest_file_holds_the_sha256_and_the_name_and_platforms_differ_by_name_only() {
    // Catches: a digest file of another format, or a platform that changes the bytes.
    let dir = scratch("package-digest");
    let (history, commit) = history();
    let x86 = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "x86");
    let arm = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxArm64, "arm");
    let lines = package(&history, &x86).expect("package x86");
    package(&history, &arm).expect("package arm");
    let name = "hostkeeper-0.1.5-linux-x86_64.tar.zst";
    let hex = file_sha256(&x86.out.join(name)).expect("hash");
    assert_eq!(
        lines,
        [format!("{name}: sha256 {hex}"), format!("sha256={hex}")]
    );
    assert_eq!(
        std::fs::read_to_string(x86.out.join(format!("{name}.sha256"))).expect("digest"),
        format!("{hex}  {name}\n")
    );
    let arm_name = "hostkeeper-0.1.5-linux-arm64.tar.zst";
    assert_eq!(
        std::fs::read_to_string(arm.out.join(format!("{arm_name}.sha256"))).expect("digest"),
        format!("{hex}  {arm_name}\n")
    );
    assert_eq!(built(&x86), built(&arm));
}

#[test]
fn a_unit_that_does_not_start_the_binary_through_current_is_refused() {
    // Catches: a unit naming a version directory, another binary, a foreign package path or
    // bytes that are not text, which would escape the atomic switch of current.
    let dir = scratch("package-unit-refused");
    let (history, commit) = history();
    let path = dir.join("hostkeeper.service").display().to_string();
    let binary_path = binary_path("hostkeeper");
    let go = |unit: &[u8], out: &str| {
        let r = request(&dir, unit, &commit, Platform::LinuxX86_64, out);
        let result = package(&history, &r);
        assert!(!r.out.exists(), "nothing is written on a refusal");
        result
    };
    let versioned = UNIT.replace("hostkeeper/current/", "hostkeeper/0.1.5/");
    assert_eq!(
        go(versioned.as_bytes(), "o1"),
        Err(Error::Failed(format!(
            "{path} has no ExecStart line naming {binary_path}"
        )))
    );
    let other = UNIT.replace("current/bin/hostkeeper daemon", "current/bin/other daemon");
    assert_eq!(
        go(other.as_bytes(), "o2"),
        Err(Error::Failed(format!(
            "{path} has no ExecStart line naming {binary_path}"
        )))
    );
    let foreign = format!("{UNIT}WorkingDirectory=/var/lib/hostkeeper/packages/other/current/\n");
    assert_eq!(
        go(foreign.as_bytes(), "o3"),
        Err(Error::Failed(format!(
            "{path} names a package file by a path other than {PACKAGES}hostkeeper/current/"
        )))
    );
    let pinned = format!("{UNIT}WorkingDirectory=/var/lib/hostkeeper/packages/hostkeeper/0.1.5/\n");
    assert_eq!(
        go(pinned.as_bytes(), "o4"),
        Err(Error::Failed(format!(
            "{path} names a package file by a path other than {PACKAGES}hostkeeper/current/"
        )))
    );
    assert_eq!(
        go(b"\xff\xfe", "o5"),
        Err(Error::Failed(format!("{path} is not UTF-8 text")))
    );
}

#[test]
fn a_missing_binary_or_unit_is_an_io_error_naming_the_path() {
    // Catches: a missing input that is silently packaged as empty, or an error that does not say
    // which file.
    let dir = scratch("package-missing-input");
    let (history, commit) = history();
    let mut r = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "out");
    r.binary = dir.join("absent-binary");
    let why = std::fs::read(&r.binary).expect_err("absent").to_string();
    assert_eq!(
        package(&history, &r),
        Err(Error::Io {
            what: format!("read {}", r.binary.display()),
            why
        })
    );
    r.binary = dir.join("hostkeeper");
    let absent = dir.join("absent-unit");
    r.unit = Some(absent.clone());
    let why = std::fs::read(&absent).expect_err("absent").to_string();
    assert_eq!(
        package(&history, &r),
        Err(Error::Io {
            what: format!("read {}", absent.display()),
            why
        })
    );
    assert!(!r.out.exists());
}

#[test]
fn an_unknown_commit_is_a_port_error() {
    // Catches: an archive dated by anything but the commit's committer time.
    let dir = scratch("package-unknown-commit");
    let (history, _) = history();
    let unknown = nth_commit(99);
    let r = request(
        &dir,
        UNIT.as_bytes(),
        &unknown,
        Platform::LinuxX86_64,
        "out",
    );
    assert_eq!(
        package(&history, &r),
        Err(Error::Port(PortError::new(
            format!("git: {unknown}"),
            "unknown commit"
        )))
    );
}

#[test]
fn an_output_that_cannot_be_written_is_an_io_error() {
    // Catches: a write failure that is dropped, leaving a package without its digest file.
    let dir = scratch("package-unwritable");
    let (history, commit) = history();
    let name = "hostkeeper-0.1.5-linux-x86_64.tar.zst";

    let r = request(
        &dir,
        UNIT.as_bytes(),
        &commit,
        Platform::LinuxX86_64,
        "file",
    );
    std::fs::write(&r.out, "in the way").expect("a file where the directory goes");
    let why = std::fs::create_dir_all(&r.out)
        .expect_err("a file")
        .to_string();
    assert_eq!(
        package(&history, &r),
        Err(Error::Io {
            what: format!("create {}", r.out.display()),
            why
        })
    );

    let r = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "pkg");
    std::fs::create_dir_all(r.out.join(name)).expect("a directory where the package goes");
    let target = r.out.join(name);
    let why = std::fs::write(&target, b"")
        .expect_err("a directory")
        .to_string();
    assert_eq!(
        package(&history, &r),
        Err(Error::Io {
            what: format!("write {}", target.display()),
            why
        })
    );

    let r = request(&dir, UNIT.as_bytes(), &commit, Platform::LinuxX86_64, "sum");
    let target = r.out.join(format!("{name}.sha256"));
    std::fs::create_dir_all(&target).expect("a directory where the digest goes");
    let why = std::fs::write(&target, b"")
        .expect_err("a directory")
        .to_string();
    assert_eq!(
        package(&history, &r),
        Err(Error::Io {
            what: format!("write {}", target.display()),
            why
        })
    );
}

#[test]
fn the_entry_list_check_refuses_each_shape_rule_and_accepts_a_good_list() {
    // Catches: a shape rule of the entry check that is not applied: an absolute path, an empty
    // component, a name equal to another after case folding, a mode outside 555 and 444.
    let refused = |entry: Entry| check_entries(&[entry]);
    assert_eq!(
        refused(Entry::file("/etc/x", 0o444, Vec::new())),
        Err(Error::Refused(vec![
            "/etc/x: is an absolute path".to_owned()
        ]))
    );
    assert_eq!(
        refused(Entry::file("bin//x", 0o444, Vec::new())),
        Err(Error::Refused(vec![
            "bin//x: has an empty component".to_owned()
        ]))
    );
    assert_eq!(
        refused(Entry::file("units/x", 0o755, Vec::new())),
        Err(Error::Refused(vec![
            "units/x: mode 755 is not 555 or 444".to_owned()
        ]))
    );
    assert_eq!(
        check_entries(&[
            Entry::file("files/a", 0o444, Vec::new()),
            Entry::file("files/A", 0o444, Vec::new()),
            Entry::dir("files/a/"),
        ]),
        Err(Error::Refused(vec![
            "files/A: the same name as an earlier entry, ignoring ASCII case".to_owned(),
            "files/a/: the same name as an earlier entry, ignoring ASCII case".to_owned(),
        ]))
    );
    let with_unit = Contents {
        manifest: manifest(&version(), true),
        units: vec![Unit {
            file: UNIT_FILE.to_owned(),
            origin: String::new(),
            data: Vec::new(),
        }],
    };
    assert_eq!(
        check_entries(&entries("hostkeeper", Vec::new(), with_unit)),
        Ok(())
    );
    let without_unit = Contents {
        manifest: manifest(&version(), false),
        units: Vec::new(),
    };
    assert_eq!(
        check_entries(&entries("hostkeeper", Vec::new(), without_unit)),
        Ok(())
    );
}

/// On drop, makes every directory under its path writable again and removes the path. The
/// unpacker seals a version read-only, so the scratch directory could not remove it, and the
/// unpacked executable would stay under `target/`, where the coverage report loads every
/// executable it finds.
pub(super) struct Unseal(pub(super) std::path::PathBuf);

impl Drop for Unseal {
    fn drop(&mut self) {
        fn writable(dir: &std::path::Path) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    writable(&entry.path());
                }
            }
        }
        writable(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The packages directory `dir/packages` holding the package of `platform` at the running
/// daemon's own version (so `min_hostkeeper` admits it), built over `binary` and `unit` and
/// unpacked by the daemon's unpacker (design 5.5 steps 3 and 4, hostkeeper-unpack) for
/// `unpack_on`. Returns that directory, the version, the unpacker's answer, and the guard that
/// removes the sealed tree.
fn unpacked_tree(
    dir: &Path,
    platform: Platform,
    binary: &[u8],
    unit: &str,
    unpack_on: hostkeeper_unpack::Platform<'_>,
) -> (
    std::path::PathBuf,
    String,
    hostkeeper_unpack::Unpacked,
    Unseal,
) {
    let (history, commit) = history();
    let running = hostkeeper_unpack::running_hostkeeper().to_string();
    let mut req = request(dir, unit.as_bytes(), &commit, platform, "out");
    std::fs::write(&req.binary, binary).expect("write the binary");
    req.version = running.parse().expect("the workspace version");
    package(&history, &req).expect("package");
    let archive = req
        .out
        .join(Package::Hostkeeper.package_name(&req.version, platform));
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
        service: ServiceName::new("hostkeeper").expect("a service name"),
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
    let unpacked =
        hostkeeper_unpack::unpack(&archive, &order, &limits, &fs, unpack_on).expect("unpack");
    (packages, running, unpacked, unseal)
}

/// Builds the package of `platform` at the running daemon's own version with `binary` as its
/// binary, and unpacks it with the daemon's unpacker for `unpack_on` ([`unpacked_tree`]); the
/// manifest's run units and the unpacked binary's bytes.
fn unpack_as_the_daemon(
    name: &str,
    platform: Platform,
    binary: &[u8],
    unpack_on: hostkeeper_unpack::Platform<'_>,
) -> (Vec<String>, Vec<u8>) {
    let dir = scratch(name);
    let (packages, running, unpacked, unseal) =
        unpacked_tree(&dir, platform, binary, UNIT, unpack_on);
    assert!(unpacked.fresh);
    let bytes = std::fs::read(
        packages
            .join("hostkeeper")
            .join(&running)
            .join("bin/hostkeeper"),
    )
    .expect("the unpacked binary");
    drop(unseal);
    assert!(!packages.exists(), "the unpacked tree is removed");
    (unpacked.manifest.run, bytes)
}

#[test]
fn package_unpacks_with_the_daemons_unpacker_under_its_bounds_and_shape_rules() {
    // Catches: a package the daemon's unpacker (design 5.5 steps 3 and 4, hostkeeper-unpack)
    // refuses: an entry outside the four top-level names, a link or other kind, a mode bit it
    // does not allow, a digest that is not the archive's, or a manifest it cannot load.
    let (run, bytes) = unpack_as_the_daemon(
        "package-daemon-unpack",
        Platform::LinuxX86_64,
        BINARY,
        hostkeeper_unpack::Platform::Linux,
    );
    assert_eq!(run, ["hostkeeper.service"]);
    assert_eq!(bytes, BINARY);
}

/// The Mach-O magic of a 64-bit binary, so the macOS unpack sends the file to the signature
/// check.
const MACHO: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];

/// The macOS checks as the daemon makes them, answering "no quarantine" and "valid
/// signature", and recording what each was asked of (the last path components).
#[derive(Default)]
struct MacChecks {
    quarantine: std::cell::RefCell<Vec<String>>,
    signature: std::cell::RefCell<Vec<String>>,
}

/// The last two components of `path`.
fn tail(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

impl hostkeeper_unpack::MacVerify for MacChecks {
    fn quarantine(&self, path: &Path) -> Result<bool, hostkeeper_unpack::VerifyIoError> {
        self.quarantine.borrow_mut().push(tail(path));
        Ok(false)
    }

    fn signature(
        &self,
        binary: &Path,
    ) -> Result<hostkeeper_unpack::SignatureVerdict, hostkeeper_unpack::VerifyIoError> {
        self.signature.borrow_mut().push(tail(binary));
        Ok(hostkeeper_unpack::SignatureVerdict::Valid)
    }
}

#[test]
fn a_macos_package_holds_the_binary_and_a_manifest_without_units() {
    // Catches: a macos-arm64 package that carries the systemd unit (which a Mac cannot run), a
    // units/ directory or a [units] table, or a package that is not the deterministic archive of
    // the Linux ones with the units left out.
    let dir = scratch("package-macos");
    let (history, commit) = history();
    let r = request(&dir, UNIT.as_bytes(), &commit, Platform::MacosArm64, "out");
    assert_eq!(r.unit, None);
    let lines = package(&history, &r).expect("package");
    let bytes = built(&r);
    assert_eq!(shape_problems(&bytes, time_of(1)), Vec::<String>::new());
    let seen = read_back(&bytes);
    let names: Vec<&str> = seen.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["bin/", "bin/hostkeeper", "manifest.toml"]);
    assert_eq!(seen[1].data, BINARY);
    assert_eq!(seen[2].data, manifest(&version(), false).into_bytes());
    let name = "hostkeeper-0.1.5-macos-arm64.tar.zst";
    let hex = hex::encode(Sha256::digest(&bytes));
    assert_eq!(
        lines,
        [format!("{name}: sha256 {hex}"), format!("sha256={hex}")]
    );
    assert_eq!(
        std::fs::read_to_string(r.out.join(format!("{name}.sha256"))).expect("the digest file"),
        format!("{hex}  {name}\n")
    );
    // The same bytes again, and the macOS bytes are the Linux package's without its units.
    package(&history, &r).expect("package again");
    assert_eq!(built(&r), bytes);
    let version_dir = scratch("package-macos-unpacked");
    unpack(&bytes, &version_dir);
    let manifest = Manifest::load(&version_dir, &expected(), &host(5)).expect("the manifest loads");
    assert_eq!(manifest.run, Vec::<String>::new());
    assert_eq!(manifest.unit_files, BTreeSet::new());
    assert_eq!(manifest.hook.command, ["bin/hostkeeper", "update-hook"]);
}

#[test]
fn a_linux_package_needs_a_unit_and_a_macos_package_takes_none() {
    // Catches: a Linux package built without its unit, or a systemd unit put into the macOS
    // package; neither writes anything.
    let dir = scratch("package-unit-flag");
    let (history, commit) = history();
    for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
        let mut r = request(&dir, UNIT.as_bytes(), &commit, platform, "linux");
        r.unit = None;
        assert_eq!(
            package(&history, &r),
            Err(Error::Failed(LINUX_NEEDS_UNIT.to_owned())),
            "{}",
            platform.name()
        );
        assert!(!r.out.exists());
    }
    let mut r = request(&dir, UNIT.as_bytes(), &commit, Platform::MacosArm64, "mac");
    r.unit = Some(dir.join("hostkeeper.service"));
    assert_eq!(
        package(&history, &r),
        Err(Error::Failed(MACOS_TAKES_NO_UNIT.to_owned()))
    );
    assert!(!r.out.exists());
}

#[test]
fn a_macos_package_unpacks_with_the_daemons_macos_unpacker() {
    // Catches: a macOS package the daemon's unpacker refuses on a Mac (its shape, its manifest
    // without units), or one whose binary is not sent to the code signature check.
    let checks = MacChecks::default();
    let binary = [MACHO.as_slice(), b" a Mach-O hostkeeper"].concat();
    let (run, bytes) = unpack_as_the_daemon(
        "package-daemon-unpack-macos",
        Platform::MacosArm64,
        &binary,
        hostkeeper_unpack::Platform::MacOs(&checks),
    );
    assert_eq!(run, Vec::<String>::new());
    assert_eq!(bytes, binary);
    assert_eq!(*checks.signature.borrow(), ["bin/hostkeeper"]);
    let mut asked = checks.quarantine.borrow().clone();
    asked.sort();
    assert!(
        ["bin/hostkeeper", "manifest.toml"]
            .iter()
            .all(|n| asked.iter().any(|a| a.ends_with(n))),
        "{asked:?}"
    );
}

/// The unit hostkeeper's own package carries, as committed (the release workflow passes it to
/// `release package --unit`).
const PACKAGE_UNIT: &str =
    include_str!("../../../../../packaging/linux/package/hostkeeper.service");

/// The unit the deb and the rpm install, as committed.
const DEB_UNIT: &str = include_str!("../../../../../packaging/linux/hostkeeper.service");

/// What a launcher starts for the `ExecStart=` of `unit` on a host whose packages directory is
/// `packages`: the program, with the unit's own `--packages-dir` rebased onto `packages` and every
/// link on its path followed, and the arguments. Fails naming what the unit lacks, or when the
/// program is not under its packages directory or does not resolve to a file.
fn launched(unit: &str, packages: &Path) -> Result<(std::path::PathBuf, Vec<String>), String> {
    let line = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .ok_or("the unit has no ExecStart=")?;
    let mut words = line.split_whitespace().map(str::to_owned);
    let program = words.next().ok_or("ExecStart= names no program")?;
    let args: Vec<String> = words.collect();
    let dir = args
        .windows(2)
        .find(|w| w[0] == "--packages-dir")
        .map(|w| format!("{}/", w[1]))
        .ok_or("ExecStart= has no --packages-dir")?;
    let inside = program
        .strip_prefix(&dir)
        .ok_or_else(|| format!("{program} is not under the --packages-dir {dir}"))?;
    let path = std::fs::canonicalize(packages.join(inside))
        .map_err(|e| format!("{program} does not resolve: {e}"))?;
    Ok((path, args))
}

#[test]
fn the_committed_package_unit_launches_the_packaged_binary_through_current() {
    // Catches: a package unit whose ExecStart does not reach the binary the package carries once
    // the daemon has unpacked it and `current` names its version (the deb's /usr/bin path, a
    // path outside --packages-dir, a broken link), or whose arguments the daemon refuses.
    use clap::Parser;
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("package-unit-launch");
    let (packages, running, unpacked, unseal) = unpacked_tree(
        &dir,
        Platform::LinuxX86_64,
        BINARY,
        PACKAGE_UNIT,
        hostkeeper_unpack::Platform::Linux,
    );
    assert_eq!(unpacked.manifest.run, ["hostkeeper.service"]);
    let service = packages.join("hostkeeper");
    let unit = std::fs::read_to_string(service.join(&running).join("units/hostkeeper.service"))
        .expect("the unpacked unit");
    assert_eq!(unit, PACKAGE_UNIT);

    // Before `current` names a version, the unit starts nothing.
    let current = service.join(hostkeeper_daemon::installed::CURRENT);
    let refused = launched(&unit, &packages).expect_err("no current yet");
    assert!(refused.contains("does not resolve"), "{refused}");

    std::os::unix::fs::symlink(&running, &current).expect("link current");
    let (program, args) = launched(&unit, &packages).expect("a launch");
    let binary = service.join(&running).join("bin/hostkeeper");
    assert_eq!(program, std::fs::canonicalize(&binary).expect("the binary"));
    assert_eq!(std::fs::read(&program).expect("the launched file"), BINARY);
    let mode = std::fs::metadata(&program)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o111,
        0o111,
        "the launched file is executable: {mode:o}"
    );
    let cli = hostkeeper_daemon::cli::Cli::try_parse_from(
        std::iter::once("hostkeeper".to_owned()).chain(args),
    )
    .expect("the daemon's flags");
    assert!(matches!(
        cli.command,
        hostkeeper_daemon::cli::Command::Daemon(_)
    ));

    // The deb's unit, and units that lack what the launch needs, are refused by name.
    let deb = launched(DEB_UNIT, &packages).expect_err("the deb's unit");
    assert!(
        deb.starts_with("/usr/bin/hostkeeper is not under the --packages-dir"),
        "{deb}"
    );
    for (text, why) in [
        ("[Service]\nType=notify\n", "the unit has no ExecStart="),
        ("[Service]\nExecStart=\n", "ExecStart= names no program"),
        (
            "[Service]\nExecStart=/opt/hostkeeper daemon\n",
            "ExecStart= has no --packages-dir",
        ),
    ] {
        assert_eq!(launched(text, &packages), Err(why.to_owned()));
    }
    drop(unseal);
    assert!(!packages.exists(), "the unpacked tree is removed");
}
