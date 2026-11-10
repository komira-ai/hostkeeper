//! A full disk, a crash at every call of the port, and what a crash leaves behind.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::version::{
    FillError, FillOrder, Filled, PackageFs, UnpackError, remove_orphans, stage_filled,
};
use hostkeeper_steps::{Layout, Seal};

use super::support::{self, Call, EACCES, Faulty, Matcher, Rig, SERVICE, VERSION, sha256_hex};

const ENOSPC: i32 = 28;
const EIO: i32 = 5;
// EDQUOT is 122 on Linux and 69 on macOS, so the platform's own value.
const EDQUOT: i32 = rustix::io::Errno::DQUOT.raw_os_error();

/// What an installer writes into a stage: a program, a file of two buffers and more, and a plain
/// file.
fn fill(stage: &Path) -> Result<(), String> {
    let write = |e: std::io::Error| e.to_string();
    fs::create_dir_all(stage.join("tree/usr/bin")).map_err(write)?;
    fs::write(stage.join("tree/usr/bin/x"), "#!x\n").map_err(write)?;
    fs::set_permissions(
        stage.join("tree/usr/bin/x"),
        fs::Permissions::from_mode(0o755),
    )
    .map_err(write)?;
    fs::write(stage.join("tree/usr/bin/big"), vec![9u8; 200 * 1024]).map_err(write)?;
    fs::write(stage.join("target.json"), "{}\n").map_err(write)
}

/// The package digest the tests give `version`.
fn digest(version: &str) -> Sha256Hex {
    Sha256Hex::parse(&sha256_hex(format!("the package of {version}").as_bytes())).unwrap()
}

/// Publishes `version` through `fs`, filling it with [`fill`].
fn publish(fs: &dyn PackageFs, version: &str) -> Result<Filled, UnpackError> {
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new(version).unwrap();
    let package = digest(version.as_str());
    let order = FillOrder {
        service: &service,
        version: &version,
        package_digest: &package,
    };
    stage_filled(&order, fs, &mut |_| Ok(()), &mut |stage| fill(stage)).map_err(|e| match e {
        FillError::Unpack(e) => e,
        FillError::Fill(why) => panic!("the fill failed: {why}"),
    })
}

/// Publishes `version` on the healthy port.
fn install(rig: &Rig, version: &str) {
    publish(&rig.host(), version).unwrap();
}

/// What is in the service directory: each name with what a link points at.
fn state(rig: &Rig) -> Vec<(String, Option<String>)> {
    rig.listing()
        .into_iter()
        .map(|name| {
            let target = fs::read_link(rig.service().join(&name))
                .ok()
                .map(|t| t.to_string_lossy().into_owned());
            (name, target)
        })
        .collect()
}

/// The permission bits of `path`, not following a link.
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn published() -> [String; 2] {
    [VERSION.to_owned(), format!("{VERSION}.seal")]
}

/// Whether `c` is the mode 0555 given to the published version directory itself.
fn version_mode(c: &Call) -> bool {
    matches!(c, Call::SetMode(p, 0o555) if p == &Path::new(SERVICE).join(VERSION))
}

/// Whether `c` is the fsync of the service directory itself.
fn service_sync(c: &Call) -> bool {
    matches!(c, Call::SyncDir(p) if p == Path::new(SERVICE))
}

/// Catches a full disk that leaves a partial version, or that makes room by deleting: ENOSPC at
/// the write of the seal, at its fsync and at a directory fsync, and EDQUOT at the create of the
/// seal, are each `no_space`; no `.stage-*`, no new version and no seal of it stay; the versions
/// `current` and `previous` point at, their seals and both links are as they were.
#[test]
fn enospc_while_publishing_leaves_no_partial_version() {
    let at: Vec<(&str, Matcher, usize, i32)> = vec![
        (
            "the write of the seal",
            Box::new(|c| matches!(c, Call::Write(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            0,
            ENOSPC,
        ),
        (
            "the fsync of the seal",
            Box::new(|c| matches!(c, Call::FileSync(_))),
            0,
            ENOSPC,
        ),
        (
            "a directory fsync",
            Box::new(|c| matches!(c, Call::SyncDir(_))),
            0,
            ENOSPC,
        ),
        (
            "the create of the seal, over quota",
            Box::new(|c| matches!(c, Call::CreateFile(_))),
            0,
            EDQUOT,
        ),
    ];
    for (what, matcher, nth, errno) in at {
        let rig = Rig::new("f-enospc");
        install(&rig, "1.4.1");
        install(&rig, "1.4.2");
        let layout = Layout::package(&rig.service());
        layout.switch("1.4.1").unwrap();
        layout.switch("1.4.2").unwrap();
        let before = state(&rig);
        let fake = Faulty::new(rig.host()).fail(matcher, nth, errno);
        let err = publish(&fake, VERSION).unwrap_err();
        assert!(matches!(err, UnpackError::NoSpace(_)), "{what}: {err:?}");
        assert_eq!(err.reason(), "no_space", "{what}");
        assert!(err.to_string().starts_with("no space left"), "{err}");
        assert_eq!(state(&rig), before, "{what}");
        for version in ["1.4.1", "1.4.2"] {
            layout.check_tree(version).unwrap();
        }
        assert_eq!(
            (layout.current().unwrap(), layout.previous().unwrap()),
            (Some("1.4.2".into()), Some("1.4.1".into()))
        );
    }
}

/// Catches a version directory that can exist half-written, or without its seal: the publish is
/// stopped at each call of the port in turn, as a crash there would stop it, and after each
/// `<version>` is either absent or whole with a seal that matches it and the package. After
/// `remove_orphans`, the next publish gives the sealed version, with nothing else beside it.
#[test]
fn a_crash_during_a_publish_leaves_no_version_directory() {
    let clean = Rig::new("f-crash-count");
    let counted = Faulty::new(clean.host());
    publish(&counted, VERSION).unwrap();
    let calls = counted.calls().len();
    assert!(calls > 10, "{calls}");
    let mut found = 0;
    for crash in 0..calls {
        let rig = Rig::new("f-crash");
        let fake = Faulty::new(rig.host()).crash_at(crash);
        let result = publish(&fake, VERSION);
        assert!(result.is_err(), "a crash at call {crash} was not seen");
        let layout = Layout::package(&rig.service());
        let dir = layout.version_path(VERSION);
        if dir.exists() {
            found += 1;
            layout
                .check_tree(VERSION)
                .unwrap_or_else(|e| panic!("crash at {crash}: {e}"));
            let seal = Seal::parse(&fs::read(layout.seal_path(VERSION)).unwrap()).unwrap();
            assert_eq!(
                seal.package_digest(),
                digest(VERSION).as_str(),
                "crash at {crash}"
            );
        }
        remove_orphans(&ServiceName::new(SERVICE).unwrap(), &rig.host()).unwrap();
        let again = publish(&rig.host(), VERSION).unwrap();
        assert_eq!(again.dir, dir, "crash at {crash}");
        assert_eq!(rig.listing(), published(), "crash at {crash}");
        assert_eq!(mode(&dir), 0o555, "crash at {crash}");
    }
    // Only a crash at the last three calls, after the rename (the fsync of the service
    // directory, the mode of the version directory, its fsync), finds the version in place.
    assert_eq!(found, 3);
}

/// Catches a publish that works only where a directory that is not writable can be renamed: on a
/// port that renames as macOS does for a caller that is not root (a directory its owner cannot
/// write is refused with `EACCES`, even within one parent), a stage is renamed while its owner
/// can still write it, and the published version is whole, sealed and 0555. The port is shown to
/// refuse such a rename first, so the test cannot pass on a port that renames everything.
#[test]
fn the_stage_is_renamed_where_a_read_only_directory_cannot_be_renamed() {
    let rig = Rig::new("f-mac-rename");
    let fake = Faulty::new(rig.host()).mac_rename();
    fs::create_dir_all(rig.service().join("ro")).unwrap();
    fs::set_permissions(rig.service().join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    let service = Path::new(SERVICE);
    let refused = fake
        .rename(&service.join("ro"), &service.join("moved"))
        .unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(EACCES));
    fs::set_permissions(rig.service().join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir(rig.service().join("ro")).unwrap();

    let first = publish(&fake, VERSION).unwrap();
    assert!(first.fresh);
    assert_eq!(rig.listing(), published());
    let layout = Layout::package(&rig.service());
    layout.check_tree(VERSION).unwrap();
    assert_eq!(mode(&layout.version_path(VERSION)), 0o555);
}

/// Catches a version left writable by its owner staying so, or being lost: when the mode 0555
/// of the version directory after its rename fails, or its fsync does, the publish is an
/// `io_error` that leaves the version whole and sealed (0700 when the mode failed); the next
/// publish of the order takes it as it is and makes it 0555; and when that publish's own mode
/// fails too, it is an `io_error` that leaves the version as it was.
#[test]
fn a_version_left_writable_after_its_rename_is_made_read_only_by_the_next_publish() {
    for what in ["the mode of the version", "the fsync of the version"] {
        let rig = Rig::new("f-settle");
        let matcher: Matcher = if what == "the mode of the version" {
            Box::new(version_mode)
        } else {
            Box::new(|c| matches!(c, Call::SyncDir(p) if p == &Path::new(SERVICE).join(VERSION)))
        };
        let fake = Faulty::new(rig.host()).fail(matcher, 0, EIO);
        let err = publish(&fake, VERSION).unwrap_err();
        assert!(
            matches!(&err, UnpackError::Io { what, .. } if *what == "cannot make the version read-only"),
            "{what}: {err:?}"
        );
        assert_eq!(err.reason(), "io_error", "{what}");
        assert_eq!(rig.listing(), published(), "{what}");
        let layout = Layout::package(&rig.service());
        layout.check_tree(VERSION).unwrap();
        let dir = layout.version_path(VERSION);
        let left = if what == "the mode of the version" {
            0o700
        } else {
            0o555
        };
        assert_eq!(mode(&dir), left, "{what}");

        if left == 0o700 {
            let failing = Faulty::new(rig.host()).fail(Box::new(version_mode), 0, EIO);
            let err = publish(&failing, VERSION).unwrap_err();
            assert!(
                matches!(&err, UnpackError::Io { what, .. } if *what == "cannot make the version read-only"),
                "{err:?}"
            );
            assert_eq!(mode(&dir), 0o700);
            layout.check_tree(VERSION).unwrap();
        }
        let again = publish(&rig.host(), VERSION).unwrap();
        assert!(!again.fresh, "{what}");
        assert_eq!(mode(&dir), 0o555, "{what}");
        layout.check_tree(VERSION).unwrap();
    }
}

/// Catches orphans left behind, or more than orphans removed: `remove_orphans` removes every
/// `.stage-*` (read-only contents included), every `.<version>.seal.tmp` and every seal without
/// its version; it keeps a sealed version, its seal, the links, other files and every other
/// service's directory, and a service with no directory has nothing to remove.
#[test]
fn a_crashed_staging_directory_is_removed_at_start() {
    let rig = Rig::new("f-orphans");
    install(&rig, "1.4.2");
    let service = rig.service();
    fs::create_dir_all(service.join(".stage-x/bin")).unwrap();
    fs::write(service.join(".stage-x/bin/half"), "half").unwrap();
    fs::set_permissions(
        service.join(".stage-x/bin"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    fs::set_permissions(service.join(".stage-x"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::create_dir(service.join(".stage-y")).unwrap();
    fs::write(service.join(".1.5.0.seal.tmp"), "{").unwrap();
    fs::write(service.join("9.9.9.seal"), "{}").unwrap();
    fs::write(service.join("notes"), "kept").unwrap();
    symlink("1.4.2", service.join("current")).unwrap();
    let other = rig.packages.join("other");
    fs::create_dir_all(other.join(".stage-z")).unwrap();
    fs::write(other.join("1.0.seal"), "{}").unwrap();

    remove_orphans(&ServiceName::new(SERVICE).unwrap(), &rig.host()).unwrap();
    assert_eq!(rig.listing(), ["1.4.2", "1.4.2.seal", "current", "notes"]);
    Layout::package(&service).check_tree("1.4.2").unwrap();
    assert!(other.join(".stage-z").is_dir() && other.join("1.0.seal").is_file());

    let absent = ServiceName::new("absent").unwrap();
    remove_orphans(&absent, &rig.host()).unwrap();
    assert!(!rig.packages.join("absent").exists());

    fs::write(rig.packages.join("afile"), "x").unwrap();
    let err = remove_orphans(&ServiceName::new("afile").unwrap(), &rig.host()).unwrap_err();
    assert!(matches!(err, UnpackError::Io { .. }), "{err:?}");
    assert_eq!(err.reason(), "io_error");
}

/// Catches a failure at a step of the publish, or a partial write, that leaves a seal without
/// its version, or a changed installed version: a write of the seal that fails half done (ENOSPC
/// and EIO), the fsync of the temporary seal, the rename of the seal, the fsync of the service
/// directory after it, and the rename of the tree each give the reason of their error, leave no
/// new version and no seal of it, and the installed versions and links as they were.
#[test]
fn a_failure_at_each_step_of_the_publish_leaves_the_host_as_it_was() {
    let at: Vec<(&str, Matcher, i32, &str)> = vec![
        (
            "a half write, ENOSPC",
            Box::new(|c| matches!(c, Call::Write(..))),
            ENOSPC,
            "no_space",
        ),
        (
            "a half write, EIO",
            Box::new(|c| matches!(c, Call::Write(..))),
            EIO,
            "io_error",
        ),
        (
            "the fsync of the temporary seal",
            Box::new(|c| matches!(c, Call::FileSync(p) if p.ends_with(".1.5.0.seal.tmp"))),
            ENOSPC,
            "no_space",
        ),
        (
            "the rename of the seal",
            Box::new(|c| matches!(c, Call::Rename(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            EIO,
            "io_error",
        ),
        (
            "the fsync after the seal's rename",
            Box::new(service_sync),
            EIO,
            "io_error",
        ),
        (
            "the rename of the tree",
            Box::new(|c| matches!(c, Call::Rename(_, to) if to.ends_with("1.5.0"))),
            EIO,
            "io_error",
        ),
    ];
    for (what, matcher, errno, reason) in at {
        let rig = Rig::new("f-publish");
        install(&rig, "1.4.1");
        install(&rig, "1.4.2");
        let layout = Layout::package(&rig.service());
        layout.switch("1.4.1").unwrap();
        layout.switch("1.4.2").unwrap();
        let before = state(&rig);
        let fake = Faulty::new(rig.host()).partial().fail(matcher, 0, errno);
        let err = publish(&fake, VERSION).unwrap_err();
        assert_eq!(err.reason(), reason, "{what}: {err:?}");
        assert_eq!(state(&rig), before, "{what}");
        layout.check_tree("1.4.1").unwrap();
        layout.check_tree("1.4.2").unwrap();
    }
}

/// Catches a cleanup that fails being taken for a clean host, or leaving what the next run
/// cannot remove: when the removal of the staging directory fails too, the error of the publish
/// stands, what is left is only orphans, and `remove_orphans` then leaves the host as it was.
#[test]
fn a_cleanup_that_fails_leaves_only_orphans() {
    let rig = Rig::new("f-cleanup");
    install(&rig, "1.4.2");
    let before = state(&rig);
    let fake = Faulty::new(rig.host()).fail_all(
        Box::new(|c| matches!(c, Call::FileSync(_) | Call::RemoveTree(_))),
        ENOSPC,
    );
    let err = publish(&fake, VERSION).unwrap_err();
    assert!(matches!(err, UnpackError::NoSpace(_)), "{err:?}");
    assert!(
        rig.listing().iter().any(|n| n.starts_with(".stage-")),
        "{:?}",
        rig.listing()
    );
    remove_orphans(&ServiceName::new(SERVICE).unwrap(), &rig.host()).unwrap();
    assert_eq!(state(&rig), before);
}

/// Catches a failure of the last fsync, after the tree's rename, losing the version or its
/// seal: the version stays whole and sealed even when looking at it in the cleanup fails too,
/// and the next publish of the order takes it as it is.
#[test]
fn a_version_published_before_a_failed_fsync_stays_and_is_used() {
    let rig = Rig::new("f-last-fsync");
    let syncs = std::rc::Rc::new(std::cell::Cell::new(0));
    let after_rename = std::rc::Rc::new(std::cell::Cell::new(false));
    let (count, after) = (syncs.clone(), after_rename.clone());
    let fake = Faulty::new(rig.host()).fail_all(
        Box::new(move |c| {
            if service_sync(c) {
                count.set(count.get() + 1);
                after.set(count.get() == 2);
                return count.get() == 2;
            }
            matches!(c, Call::Exists(_)) & after.get()
        }),
        ENOSPC,
    );
    let err = publish(&fake, VERSION).unwrap_err();
    assert!(matches!(err, UnpackError::NoSpace(_)), "{err:?}");
    assert!(after_rename.get());
    assert_eq!(rig.listing(), published());
    Layout::package(&rig.service()).check_tree(VERSION).unwrap();
    let again = publish(&rig.host(), VERSION).unwrap();
    assert!(!again.fresh);
}

/// Catches two publishes of one service, or an orphan removal during a publish, touching each
/// other's work: while a publish is between the rename of its seal and of its tree, a second
/// publish of the service and `remove_orphans` are each refused with `io_error` and remove
/// nothing, and the first finishes with its version whole; a lock held by another holder refuses
/// a publish the same way.
#[test]
fn a_publish_holds_its_service_against_other_publishes_and_orphan_removal() {
    let rig = Rig::new("f-lock");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (packages, record) = (rig.packages.clone(), seen.clone());
    let fake = Faulty::new(rig.host()).at(
        Box::new(|c| matches!(c, Call::Rename(_, to) if to.ends_with("1.5.0"))),
        move |_| {
            let other =
                hostkeeper_install::version::HostFs::open(&packages, support::me()).unwrap();
            let published = publish(&other, VERSION);
            let removed = remove_orphans(&ServiceName::new(SERVICE).unwrap(), &other);
            let names: Vec<String> = fs::read_dir(packages.join(SERVICE))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            record.borrow_mut().push((
                published.map(|_| ()).map_err(|e| e.reason()),
                removed.map_err(|e| e.reason()),
                names.iter().any(|n| n.starts_with(".stage-"))
                    && names.contains(&"1.5.0.seal".into()),
            ));
        },
    );
    publish(&fake, VERSION).unwrap();
    assert_eq!(*seen.borrow(), [(Err("io_error"), Err("io_error"), true)]);
    assert_eq!(rig.listing(), published());
    Layout::package(&rig.service()).check_tree(VERSION).unwrap();

    let held = rig.host();
    let _lock = held.lock(Path::new(SERVICE)).unwrap();
    let err = publish(&rig.host(), "1.6.0").unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if what.contains("lock of the service")),
        "{err:?}"
    );
    assert!(!rig.service().join("1.6.0").exists());
}

/// Catches a service directory that cannot be made or looked at being passed over: a packages
/// directory nobody may write refuses a first publish (`io_error`, nothing made), and a failed
/// look at the service directory stops `remove_orphans` before it removes anything.
#[test]
fn a_service_directory_that_cannot_be_made_or_looked_at_stops_the_work() {
    let rig = Rig::new("f-service-dir");
    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o555)).unwrap();
    let err = publish(&rig.host(), VERSION).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot create the service directory"),
        "{err:?}"
    );
    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!rig.service().exists());
    install(&rig, VERSION);
    fs::create_dir(rig.service().join(".stage-left")).unwrap();
    let fake = Faulty::new(rig.host()).fail(Box::new(|c| matches!(c, Call::Exists(_))), 0, EIO);
    let err = remove_orphans(&ServiceName::new(SERVICE).unwrap(), &fake).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot look at the package directory"),
        "{err:?}"
    );
    assert!(rig.service().join(".stage-left").is_dir());
    fs::write(rig.packages.join("afile"), "x").unwrap();
    assert!(rig.host().exists(Path::new("afile/x")).is_err());
    assert!(!rig.host().exists(Path::new("absent/x")).unwrap());
}

/// Catches a service directory whose first making stopped half-way staying so: a directory a
/// crash left between its `mkdir` and its mode (0700) is made 0755 by the next publish, which
/// publishes into it; a call on a ready directory changes nothing; and a link in the place of
/// the service directory is refused, not followed, and its target is left as it was.
#[test]
fn a_service_directory_left_half_made_is_finished_by_the_next_publish() {
    let rig = Rig::new("f-half-made");
    fs::create_dir(rig.service()).unwrap();
    fs::set_permissions(rig.service(), fs::Permissions::from_mode(0o700)).unwrap();
    publish(&rig.host(), VERSION).unwrap();
    assert_eq!(mode(&rig.service()), 0o755);
    rig.host().create_service(Path::new(SERVICE)).unwrap();
    assert_eq!(mode(&rig.service()), 0o755);
    assert_eq!(rig.listing(), published());

    let rig = Rig::new("f-service-link");
    let elsewhere = rig.scratch.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&elsewhere, rig.service()).unwrap();
    let err = publish(&rig.host(), VERSION).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot create the service directory"),
        "{err:?}"
    );
    assert_eq!(mode(&elsewhere), 0o700);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// Catches an orphan that cannot be removed being passed over: when the removal of a leftover
/// staging directory fails, the publish stops with `io_error` before it stages anything.
#[test]
fn an_orphan_that_cannot_be_removed_stops_the_publish() {
    let rig = Rig::new("f-orphan-stuck");
    fs::create_dir_all(rig.service().join(".stage-old")).unwrap();
    let fake = Faulty::new(rig.host()).fail(Box::new(|c| matches!(c, Call::RemoveTree(_))), 0, EIO);
    let err = publish(&fake, VERSION).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot remove an orphan"),
        "{err:?}"
    );
    assert_eq!(err.reason(), "io_error");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| matches!(c, Call::CreateStage(_)))
    );
    assert_eq!(rig.listing(), [".stage-old"]);
}

/// Catches a version bound that refuses a version which fits, or a temporary seal of the longest
/// version that cannot be written: a 245-byte version, whose `.<version>.seal.tmp` is exactly 255
/// bytes, is published through the host port to a sealed version directory, its seal is readable
/// and names the package, no temporary seal stays, and a second publish of the order writes
/// nothing and returns the version.
#[test]
fn the_longest_version_is_published_and_found_again() {
    let rig = Rig::new("f-longest");
    let version = "1".repeat(245);
    assert_eq!(format!(".{version}.seal.tmp").len(), 255);
    let first = publish(&rig.host(), &version).unwrap();
    assert!(first.fresh);
    assert_eq!(first.dir, rig.service().join(&version));
    assert_eq!(rig.listing(), [version.clone(), format!("{version}.seal")]);
    let seal =
        Seal::parse(&fs::read(rig.service().join(format!("{version}.seal"))).unwrap()).unwrap();
    assert_eq!(seal.package_digest(), digest(&version).as_str());
    Layout::package(&rig.service())
        .check_tree(&version)
        .unwrap();
    let fake = Faulty::new(rig.host());
    let again = publish(&fake, &version).unwrap();
    assert!(!again.fresh);
    assert!(
        fake.calls().iter().all(|c| !c.writes()),
        "{:?}",
        fake.calls()
    );
    assert_eq!(rig.listing(), [version.clone(), format!("{version}.seal")]);
}
