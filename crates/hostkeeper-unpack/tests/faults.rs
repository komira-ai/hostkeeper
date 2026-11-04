//! A full disk, a crash at every call of the port, and what a crash leaves behind.

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::{Layout, Seal};
use hostkeeper_unpack::{UnpackError, remove_orphans, unpack};
use support::{
    Call, Faulty, MANIFEST, Matcher, Rig, SERVICE, Tar, VERSION, limits, order, sha256_hex,
    valid_tar, zstd,
};

const ENOSPC: i32 = 28;
const EDQUOT: i32 = 122;

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
        let err = unpack(&path, &order, &limits(), &fake).unwrap_err();
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
    unpack(&path, &order, &limits(), &counted).unwrap();
    let calls = counted.calls().len();
    assert!(calls > 20, "{calls}");
    let mut published = 0;
    for crash in 0..calls {
        let rig = Rig::new("f-crash");
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let fake = Faulty::new(rig.host()).crash_at(crash);
        let result = unpack(&path, &order, &limits(), &fake);
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
        let unpacked = unpack(&path, &order, &limits(), &rig.host()).unwrap();
        assert_eq!(unpacked.seal.package_digest(), sha256_hex(&archive));
        assert_eq!(
            rig.listing(),
            [VERSION.to_owned(), format!("{VERSION}.seal")],
            "crash at {crash}"
        );
    }
    // Only a crash at the last call, the fsync after the rename, finds the version in place.
    assert_eq!(published, 1);
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
    unpack(&path, &order, &limits(), &fake).unwrap();
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
