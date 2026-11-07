//! A full disk, a crash at every call of the port, and what a crash leaves behind.

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::{Layout, Seal};
use hostkeeper_unpack::{PackageFs, Platform, UnpackError, remove_orphans, unpack};
use support::{
    Call, Faulty, MANIFEST, Matcher, Rig, SERVICE, Tar, VERSION, limits, order, sha256_hex,
    valid_tar, zstd,
};

const ENOSPC: i32 = 28;
// EDQUOT is 122 on Linux and 69 on macOS, so the platform's own value.
const EDQUOT: i32 = rustix::io::Errno::DQUOT.raw_os_error();

/// The valid package of `version`, with a file `bin/big` of two buffers and more.
fn package(version: &str) -> Vec<u8> {
    let mut tar = Tar::new();
    tar.file(
        "manifest.toml",
        0o644,
        MANIFEST.replace("1.5.0", version).as_bytes(),
    )
    .dir("bin")
    .file("bin/x", 0o755, b"#!x\n")
    .file("bin/big", 0o644, &vec![9u8; 200 * 1024])
    .dir("files")
    .file("files/c.conf", 0o644, b"c\n");
    zstd(&tar.end().bytes())
}

/// Unpacks `version` on the healthy port.
fn install(rig: &Rig, version: &str) {
    let archive = package(version);
    let (path, _) = rig.archive(&format!("{version}.tar.zst"), &archive);
    unpack(
        &path,
        &order(version, &sha256_hex(&archive)),
        &limits(),
        &rig.host(),
        Platform::Linux,
    )
    .unwrap();
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

/// Catches a full disk that leaves a partial tree, or that makes room by deleting: ENOSPC at the
/// first write, at a write in the middle of a file, at a file fsync, at a directory fsync and at
/// the write of the seal, and EDQUOT at a create, are each `Deferred`, environment, `no_space`;
/// no `.stage-*`, no new version and no seal of it stay; the versions `current` and `previous`
/// point at, their seals and both links are as they were.
#[test]
fn enospc_during_staging_defers_with_no_space_and_leaves_no_partial_tree() {
    let at: Vec<(&str, Matcher, usize, i32)> = vec![
        (
            "the first write",
            Box::new(|c| matches!(c, Call::Write(..))),
            0,
            ENOSPC,
        ),
        (
            "a write in the middle of a file",
            Box::new(|c| matches!(c, Call::Write(p, _) if p.ends_with("bin/big"))),
            1,
            ENOSPC,
        ),
        (
            "a file fsync",
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
            "the write of the seal",
            Box::new(|c| matches!(c, Call::Write(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            0,
            ENOSPC,
        ),
        (
            "a create, over quota",
            Box::new(|c| matches!(c, Call::CreateFile(_))),
            2,
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
        let archive = package(VERSION);
        let (path, order) = rig.archive("new.tar.zst", &archive);
        let fake = Faulty::new(rig.host()).fail(matcher, nth, errno);
        let err = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap_err();
        assert!(matches!(err, UnpackError::NoSpace(_)), "{what}: {err:?}");
        assert_eq!(
            err.outcome(),
            (Outcome::Deferred, Class::Environment, "no_space"),
            "{what}"
        );
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

/// Catches a version directory that can exist half-written, or without its seal: the unpack is
/// stopped at each call of the port in turn, as a crash there would stop it, and after each
/// `<version>` is either absent or whole with a seal that matches it and the archive. After
/// `remove_orphans`, the next unpack gives the sealed version, with nothing else beside it.
#[test]
fn a_crash_during_unpack_leaves_no_version_directory() {
    let archive = package(VERSION);
    let clean = Rig::new("f-crash-count");
    let (path, order) = clean.archive("a.tar.zst", &archive);
    let counted = Faulty::new(clean.host());
    unpack(&path, &order, &limits(), &counted, Platform::Linux).unwrap();
    let calls = counted.calls().len();
    assert!(calls > 20, "{calls}");
    let mut published = 0;
    for crash in 0..calls {
        let rig = Rig::new("f-crash");
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let fake = Faulty::new(rig.host()).crash_at(crash);
        let result = unpack(&path, &order, &limits(), &fake, Platform::Linux);
        assert!(result.is_err(), "a crash at call {crash} was not seen");
        let layout = Layout::package(&rig.service());
        let dir = layout.version_path(VERSION);
        if dir.exists() {
            published += 1;
            layout
                .check_tree(VERSION)
                .unwrap_or_else(|e| panic!("crash at {crash}: {e}"));
            let seal = Seal::parse(&fs::read(layout.seal_path(VERSION)).unwrap()).unwrap();
            assert_eq!(
                seal.package_digest(),
                sha256_hex(&archive),
                "crash at {crash}"
            );
        }
        remove_orphans(&order.service, &rig.host()).unwrap();
        let unpacked = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap();
        assert_eq!(unpacked.seal.package_digest(), sha256_hex(&archive));
        assert_eq!(
            rig.listing(),
            [VERSION.to_owned(), format!("{VERSION}.seal")],
            "crash at {crash}"
        );
        assert_eq!(mode(&dir), 0o555, "crash at {crash}");
    }
    // Only a crash at the last three calls, after the rename (the fsync of the service
    // directory, the mode of the version directory, its fsync), finds the version in place.
    assert_eq!(published, 3);
}

/// The permission bits of `path`, not following a link.
fn mode(path: &std::path::Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Whether `c` is the mode 0555 given to the published version directory itself.
fn version_mode(c: &Call) -> bool {
    matches!(c, Call::SetMode(p, 0o555) if p == &std::path::Path::new(SERVICE).join(VERSION))
}

/// Whether `c` is the fsync of the published version directory itself.
fn version_sync(c: &Call) -> bool {
    matches!(c, Call::SyncDir(p) if p == &std::path::Path::new(SERVICE).join(VERSION))
}

/// Catches an unpack that publishes only where a read-only directory can be renamed: on a port
/// that renames as macOS does for a caller that is not root (a directory its owner cannot write
/// is refused with `EACCES`, even within one parent), a package whose directories are all
/// read-only is published whole and sealed, its version directory and every directory in it
/// 0555, its files as normalised, and the next unpack takes it as it is. The port is shown to
/// refuse such a rename first, so the test cannot pass on a port that renames everything.
#[test]
fn a_read_only_tree_is_published_where_a_read_only_directory_cannot_be_renamed() {
    let rig = Rig::new("f-mac-rename");
    let fake = Faulty::new(rig.host()).mac_rename();
    fs::create_dir_all(rig.service().join("ro")).unwrap();
    fs::set_permissions(rig.service().join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    let service = std::path::Path::new(SERVICE);
    let refused = fake
        .rename(&service.join("ro"), &service.join("moved"))
        .unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(support::EACCES));
    fs::set_permissions(rig.service().join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir(rig.service().join("ro")).unwrap();

    let (path, order) = rig.archive("a.tar.zst", &package(VERSION));
    let unpacked = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap();
    assert!(unpacked.fresh);
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );
    let layout = Layout::package(&rig.service());
    layout.check_tree(VERSION).unwrap();
    let dir = layout.version_path(VERSION);
    let modes: Vec<(&str, u32)> = ["", "bin", "bin/big", "bin/x", "files", "files/c.conf"]
        .into_iter()
        .map(|name| (name, mode(&dir.join(name))))
        .collect();
    assert_eq!(
        modes,
        [
            ("", 0o555),
            ("bin", 0o555),
            ("bin/big", 0o444),
            ("bin/x", 0o555),
            ("files", 0o555),
            ("files/c.conf", 0o444)
        ]
    );
    let again = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap();
    assert!(!again.fresh);
    assert_eq!(again.seal, unpacked.seal);
}

/// Catches a version left writable by its owner staying so, or being lost: when the mode 0555
/// of the version directory after its rename fails, or its fsync does, the unpack is an
/// `io_error` that leaves the version whole and sealed (0700 when the mode failed); the next
/// unpack of the
/// order, with the archive gone, takes it as it is and makes it 0555; and when that unpack's
/// own mode fails too, it is an `io_error` that leaves the version as it was.
#[test]
fn a_version_left_writable_after_its_rename_is_made_read_only_by_the_next_unpack() {
    for what in ["the mode of the version", "the fsync of the version"] {
        let rig = Rig::new("f-settle");
        let (path, order) = rig.archive("new.tar.zst", &package(VERSION));
        let matcher: Matcher = if what == "the mode of the version" {
            Box::new(version_mode)
        } else {
            Box::new(version_sync)
        };
        let fake = Faulty::new(rig.host()).fail(matcher, 0, EIO);
        let err = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap_err();
        assert!(
            matches!(&err, UnpackError::Io { what, .. } if *what == "cannot make the version read-only"),
            "{what}: {err:?}"
        );
        assert_eq!(err.outcome().2, "io_error", "{what}");
        assert_eq!(
            rig.listing(),
            [VERSION.to_owned(), format!("{VERSION}.seal")],
            "{what}"
        );
        let layout = Layout::package(&rig.service());
        layout.check_tree(VERSION).unwrap();
        let dir = layout.version_path(VERSION);
        let left = if what == "the mode of the version" {
            0o700
        } else {
            0o555
        };
        assert_eq!(mode(&dir), left, "{what}");
        fs::remove_file(&path).unwrap();

        if left == 0o700 {
            let failing = Faulty::new(rig.host()).fail(Box::new(version_mode), 0, EIO);
            let err = unpack(&path, &order, &limits(), &failing, Platform::Linux).unwrap_err();
            assert!(
                matches!(&err, UnpackError::Io { what, .. } if *what == "cannot make the version read-only"),
                "{err:?}"
            );
            assert_eq!(mode(&dir), 0o700);
            layout.check_tree(VERSION).unwrap();
        }
        let again = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap();
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
    assert_eq!(
        err.outcome(),
        (Outcome::Deferred, Class::Environment, "io_error")
    );
}

/// Catches an orphan that the unpack itself trips on: a staging directory and a temporary seal
/// that a crash left are removed by the unpack before it stages, and the unpack succeeds.
#[test]
fn an_unpack_removes_the_orphans_of_its_service_first() {
    let rig = Rig::new("f-orphans-unpack");
    let service = rig.service();
    fs::create_dir_all(service.join(".stage-old")).unwrap();
    fs::write(service.join(".1.5.0.seal.tmp"), "{").unwrap();
    fs::write(service.join("1.5.0.seal"), "{\"schema\":1}").unwrap();
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    let fake = Faulty::new(rig.host());
    unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap();
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );
    let removed: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::RemoveTree(_)))
        .collect();
    assert_eq!(removed.len(), 3, "{removed:?}");
}

const EIO: i32 = 5;

/// Whether `c` is the fsync of the service directory itself.
fn service_sync(c: &Call) -> bool {
    matches!(c, Call::SyncDir(p) if p == std::path::Path::new(SERVICE))
}

/// Catches a failure at a step of the publish, or a partial write, that leaves a partial tree, a
/// seal without its version, or a changed installed version: a write that fails half done
/// (ENOSPC and EIO), EDQUOT at a write in the middle of a file, the fsync of the temporary seal,
/// the rename of the seal, the fsync of the service directory after it, and the rename of the
/// tree each give the outcome of their error, leave no new version and no seal of it, and the
/// installed versions and links as they were.
#[test]
fn a_failure_at_each_step_of_the_publish_leaves_the_host_as_it_was() {
    let at: Vec<(&str, Matcher, usize, i32, &str)> = vec![
        (
            "a half write, ENOSPC",
            Box::new(|c| matches!(c, Call::Write(..))),
            2,
            ENOSPC,
            "no_space",
        ),
        (
            "a half write, EIO",
            Box::new(|c| matches!(c, Call::Write(..))),
            2,
            EIO,
            "io_error",
        ),
        (
            "EDQUOT in the middle of a file",
            Box::new(|c| matches!(c, Call::Write(p, _) if p.ends_with("bin/big"))),
            2,
            EDQUOT,
            "no_space",
        ),
        (
            "the fsync of the temporary seal",
            Box::new(|c| matches!(c, Call::FileSync(p) if p.ends_with(".1.5.0.seal.tmp"))),
            0,
            ENOSPC,
            "no_space",
        ),
        (
            "the rename of the seal",
            Box::new(|c| matches!(c, Call::Rename(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            0,
            EIO,
            "io_error",
        ),
        (
            "the fsync after the seal's rename",
            Box::new(service_sync),
            0,
            EIO,
            "io_error",
        ),
        (
            "the rename of the tree",
            Box::new(|c| matches!(c, Call::Rename(_, to) if to.ends_with("1.5.0"))),
            0,
            EIO,
            "io_error",
        ),
    ];
    for (what, matcher, nth, errno, reason) in at {
        let rig = Rig::new("f-publish");
        install(&rig, "1.4.1");
        install(&rig, "1.4.2");
        let layout = Layout::package(&rig.service());
        layout.switch("1.4.1").unwrap();
        layout.switch("1.4.2").unwrap();
        let before = state(&rig);
        let (path, order) = rig.archive("new.tar.zst", &package(VERSION));
        let fake = Faulty::new(rig.host()).partial().fail(matcher, nth, errno);
        let err = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap_err();
        assert_eq!(err.outcome().2, reason, "{what}: {err:?}");
        assert_eq!(state(&rig), before, "{what}");
        layout.check_tree("1.4.1").unwrap();
        layout.check_tree("1.4.2").unwrap();
    }
}

/// Catches a cleanup that fails being taken for a clean host, or leaving what the next run
/// cannot remove: when the removal of the staging directory fails too, the error of the unpack
/// stands, what is left is only orphans, and `remove_orphans` then leaves the host as it was.
#[test]
fn a_cleanup_that_fails_leaves_only_orphans() {
    let rig = Rig::new("f-cleanup");
    install(&rig, "1.4.2");
    let before = state(&rig);
    let (path, order) = rig.archive("new.tar.zst", &package(VERSION));
    let fake = Faulty::new(rig.host()).fail_all(
        Box::new(|c| matches!(c, Call::FileSync(_) | Call::RemoveTree(_))),
        ENOSPC,
    );
    let err = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap_err();
    assert!(matches!(err, UnpackError::NoSpace(_)), "{err:?}");
    assert!(
        rig.listing().iter().any(|n| n.starts_with(".stage-")),
        "{:?}",
        rig.listing()
    );
    remove_orphans(&order.service, &rig.host()).unwrap();
    assert_eq!(state(&rig), before);
}

/// Catches a failure of the last fsync, after the tree's rename, losing the version or its
/// seal: the version stays whole and sealed even when looking at it in the cleanup fails too,
/// and with the archive deleted the next unpack of the order takes it as it is.
#[test]
fn a_version_published_before_a_failed_fsync_stays_and_is_used_without_the_archive() {
    let rig = Rig::new("f-last-fsync");
    let (path, order) = rig.archive("new.tar.zst", &package(VERSION));
    let syncs = std::rc::Rc::new(std::cell::Cell::new(0));
    let published = std::rc::Rc::new(std::cell::Cell::new(false));
    let (count, after) = (syncs.clone(), published.clone());
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
    let err = unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap_err();
    assert!(matches!(err, UnpackError::NoSpace(_)), "{err:?}");
    assert!(published.get());
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );
    Layout::package(&rig.service()).check_tree(VERSION).unwrap();
    fs::remove_file(&path).unwrap();
    let again = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap();
    assert!(!again.fresh);
}

/// Catches two unpacks of one service, or an orphan removal during an unpack, touching each
/// other's work: while an unpack is between the rename of its seal and of its tree, a second
/// unpack of the service and `remove_orphans` are each refused with `io_error` and remove
/// nothing, and the first finishes with its version whole; a lock held by another holder refuses
/// an unpack the same way.
#[test]
fn an_unpack_holds_its_service_against_other_unpacks_and_orphan_removal() {
    let rig = Rig::new("f-lock");
    let (path, order) = rig.archive("new.tar.zst", &package(VERSION));
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (packages, record, second, second_order) = (
        rig.packages.clone(),
        seen.clone(),
        path.clone(),
        order.clone(),
    );
    let fake = Faulty::new(rig.host()).at(
        Box::new(|c| matches!(c, Call::Rename(_, to) if to.ends_with("1.5.0"))),
        move |_| {
            let other = hostkeeper_unpack::HostFs::open(&packages, support::me()).unwrap();
            let unpacked = unpack(&second, &second_order, &limits(), &other, Platform::Linux);
            let removed = remove_orphans(&second_order.service, &other);
            let names: Vec<String> = fs::read_dir(packages.join(SERVICE))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            record.borrow_mut().push((
                unpacked.map(|_| ()).map_err(|e| e.outcome().2),
                removed.map_err(|e| e.outcome().2),
                names.iter().any(|n| n.starts_with(".stage-"))
                    && names.contains(&"1.5.0.seal".into()),
            ));
        },
    );
    unpack(&path, &order, &limits(), &fake, Platform::Linux).unwrap();
    assert_eq!(*seen.borrow(), [(Err("io_error"), Err("io_error"), true)]);
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );
    Layout::package(&rig.service()).check_tree(VERSION).unwrap();

    let held = rig.host();
    let _lock = held.lock(std::path::Path::new(SERVICE)).unwrap();
    let other = package("1.6.0");
    let (other_path, _) = rig.archive("other.tar.zst", &other);
    let other_order = support::order("1.6.0", &sha256_hex(&other));
    let err = unpack(
        &other_path,
        &other_order,
        &limits(),
        &rig.host(),
        Platform::Linux,
    )
    .unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if what.contains("unpack lock")),
        "{err:?}"
    );
    assert!(!rig.service().join("1.6.0").exists());
}

/// Catches a service directory that cannot be made or looked at being passed over: a packages
/// directory nobody may write refuses a first unpack (`io_error`, nothing made), and a failed
/// look at the service directory stops `remove_orphans` before it removes anything.
#[test]
fn a_service_directory_that_cannot_be_made_or_looked_at_stops_the_work() {
    let rig = Rig::new("f-service-dir");
    let (path, order) = rig.archive("a.tar.zst", &package(VERSION));
    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o555)).unwrap();
    let err = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot create the service directory"),
        "{err:?}"
    );
    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!rig.service().exists());
    install(&rig, VERSION);
    fs::create_dir(rig.service().join(".stage-left")).unwrap();
    let fake = Faulty::new(rig.host()).fail(Box::new(|c| matches!(c, Call::Exists(_))), 0, EIO);
    let err = remove_orphans(&order.service, &fake).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot look at the package directory"),
        "{err:?}"
    );
    assert!(rig.service().join(".stage-left").is_dir());
    fs::write(rig.packages.join("afile"), "x").unwrap();
    assert!(rig.host().exists(std::path::Path::new("afile/x")).is_err());
    assert!(!rig.host().exists(std::path::Path::new("absent/x")).unwrap());
}

/// Catches a service directory whose first making stopped half-way staying so: a directory a
/// crash left between its `mkdir` and its mode (0700, here also given another mode by hand) is
/// made 0755 by the next unpack, which publishes into it; a call on a ready directory changes
/// nothing; and a link in the place of the service directory is refused, not followed, and
/// its target is left as it was.
#[test]
fn a_service_directory_left_half_made_is_finished_by_the_next_unpack() {
    let rig = Rig::new("f-half-made");
    fs::create_dir(rig.service()).unwrap();
    fs::set_permissions(rig.service(), fs::Permissions::from_mode(0o700)).unwrap();
    let (path, order) = rig.archive("a.tar.zst", &package(VERSION));
    unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap();
    let mode = |p: &std::path::Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&rig.service()), 0o755);
    rig.host()
        .create_service(std::path::Path::new(SERVICE))
        .unwrap();
    assert_eq!(mode(&rig.service()), 0o755);
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );

    let rig = Rig::new("f-service-link");
    let elsewhere = rig.scratch.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&elsewhere, rig.service()).unwrap();
    let (path, order) = rig.archive("a.tar.zst", &package(VERSION));
    let err = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux).unwrap_err();
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot create the service directory"),
        "{err:?}"
    );
    assert_eq!(mode(&elsewhere), 0o700);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}
