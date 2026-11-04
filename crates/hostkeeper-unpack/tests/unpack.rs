//! The unpack of a valid package, the manifest checks after the pass, and a version that is on
//! the host already.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::manifest::ManifestRefusal;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::{Layout, tree_digest};
use hostkeeper_unpack::{Rule, Seal, UnpackError, unpack};
use support::{
    Call, Faulty, MANIFEST, Rig, SERVICE, Tar, VERSION, limits, me, order, owner_of, sha256_hex,
    valid_tar, zstd,
};

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Catches an unpack that leaves a version that is not whole: after it, `<version>/` holds the
/// archive's four files with their bytes, `<version>.seal` names the digest of that tree and the
/// archive's SHA-256, no `.stage-*` is left, and the manifest came from the tree.
#[test]
fn a_valid_package_unpacks_to_a_sealed_version_directory() {
    let rig = Rig::new("u-valid");
    let archive = zstd(&valid_tar().end().bytes());
    let (path, order) = rig.archive("a.tar.zst", &archive);
    let unpacked = unpack(&path, &order, &limits(), &rig.host()).unwrap();
    let dir = rig.service().join(VERSION);
    assert_eq!(unpacked.dir, dir);
    assert!(unpacked.fresh);
    for (name, bytes) in [
        ("manifest.toml", MANIFEST.as_bytes()),
        ("bin/x", b"#!x\n"),
        ("units/tierstore.service", b"[Service]\n"),
        ("files/c.conf", b"c\n"),
    ] {
        assert_eq!(fs::read(dir.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(
        rig.listing(),
        [VERSION.to_owned(), format!("{VERSION}.seal")]
    );
    let seal = Seal::parse(&fs::read(rig.service().join("1.5.0.seal")).unwrap()).unwrap();
    assert_eq!(seal, unpacked.seal);
    assert_eq!(seal.package_digest(), sha256_hex(&archive));
    assert_eq!(seal.tree_digest(), tree_digest(&dir).unwrap());
    assert_eq!(mode(&rig.service().join("1.5.0.seal")), 0o444);
    assert_eq!(unpacked.manifest.name.as_str(), SERVICE);
    assert_eq!(unpacked.manifest.hook.command, ["bin/x", "update-hook"]);
    Layout::package(&rig.service()).check_tree(VERSION).unwrap();
}

/// Catches a mode, owner or time taken from the archive: tar modes 0777, 0755, 0644, 0600 and
/// 0640 with odd owners and times become 0555, 0555, 0444, 0444 and 0444, every directory
/// 0555, every entry owned by the port's owner; and two archives that differ only in owner,
/// group, time and mode bits other than execute have one tree digest.
#[test]
fn modes_are_normalised_and_owner_and_mtime_ignored() {
    let modes = [0o777, 0o755, 0o644, 0o600, 0o640];
    let build = |odd: bool| {
        let mut tar = Tar::new();
        tar.file("manifest.toml", 0o644, MANIFEST.as_bytes())
            .dir("bin")
            .dir("files");
        tar.file("files/c.conf", 0o644, b"c\n");
        for (i, m) in modes.iter().enumerate() {
            let tar_mode = if odd { *m } else { (m & 0o111) | 0o444 };
            let mut head = support::Head::new(format!("bin/f{i}").as_bytes(), b'0', tar_mode, 1);
            if odd {
                (head.uid, head.gid, head.mtime) = (4242 + i as u64, 77, 12345 + i as u64);
            }
            tar.entry(&head, b"z");
        }
        zstd(&tar.end().bytes())
    };
    let rig = Rig::new("u-modes");
    let (path, order) = rig.archive("odd.tar.zst", &build(true));
    let fake = Faulty::new(rig.host());
    let odd = unpack(&path, &order, &limits(), &fake).unwrap();
    let dir = rig.service().join(VERSION);
    let want = [0o555, 0o555, 0o444, 0o444, 0o444];
    for (i, w) in want.iter().enumerate() {
        let file = dir.join(format!("bin/f{i}"));
        assert_eq!(mode(&file), *w, "f{i}");
        assert_eq!(owner_of(&file), (me().uid, me().gid), "f{i}");
        let set = |c: &Call| matches!(c, Call::FileMode(p, m) if p.ends_with(format!("bin/f{i}")) && m == w);
        assert!(fake.calls().iter().any(set), "f{i}: {:?}", fake.calls());
    }
    for d in ["", "bin", "files"] {
        assert_eq!(mode(&dir.join(d)), 0o555, "{d:?}");
        assert_eq!(owner_of(&dir.join(d)), (me().uid, me().gid), "{d:?}");
    }
    let plain = Rig::new("u-modes-plain");
    let (path, order) = plain.archive("plain.tar.zst", &build(false));
    let same = unpack(&path, &order, &limits(), &plain.host()).unwrap();
    assert_eq!(odd.seal.tree_digest(), same.seal.tree_digest());
    assert_ne!(odd.seal.package_digest(), same.seal.package_digest());
}

/// Catches an archive file that is not the one the order names being sealed: one byte of
/// padding more than the bytes the order's digest is of is `digest_mismatch`, and nothing stays.
#[test]
fn an_archive_with_another_digest_is_digest_mismatch() {
    let rig = Rig::new("u-digest");
    let archive = zstd(&valid_tar().end().bytes());
    let (path, _) = rig.archive("a.tar.zst", &archive);
    let order = order(VERSION, &"ab".repeat(32));
    let err = unpack(&path, &order, &limits(), &rig.host()).unwrap_err();
    assert!(
        matches!(&err, UnpackError::DigestMismatch { expected, found }
            if expected == &"ab".repeat(32) && found == &sha256_hex(&archive)),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "digest_mismatch")
    );
    assert!(rig.listing().is_empty(), "{:?}", rig.listing());
}

fn manifest_tar(manifest: &str) -> Vec<u8> {
    let mut tar = Tar::new();
    tar.file("manifest.toml", 0o644, manifest.as_bytes())
        .dir("bin")
        .file("bin/x", 0o755, b"#!x\n")
        .dir("files")
        .file("files/c.conf", 0o644, b"c\n");
    zstd(&tar.end().bytes())
}

/// Runs an unpack of a package with `manifest` and returns its error, after checking that
/// nothing of it stayed.
fn refused_manifest(name: &str, manifest: &str) -> UnpackError {
    let rig = Rig::new(name);
    let (path, order) = rig.archive("m.tar.zst", &manifest_tar(manifest));
    let err = unpack(&path, &order, &limits(), &rig.host()).unwrap_err();
    assert!(rig.listing().is_empty(), "{name}: {:?}", rig.listing());
    err
}

/// Catches a package for another service or version being staged as this one: a manifest that
/// names another version, or another service, is `Failed`, release, `manifest_mismatch`, with
/// the staging directory removed and no version.
#[test]
fn a_manifest_for_another_name_or_version_is_manifest_mismatch() {
    let cases = [
        ("version = \"1.5.0\"", "version = \"1.5.1\""),
        ("name = \"tierstore\"", "name = \"other\""),
    ];
    for (from, to) in cases {
        let err = refused_manifest("u-mismatch", &MANIFEST.replace(from, to));
        assert!(
            matches!(
                &err,
                UnpackError::Manifest(
                    ManifestRefusal::VersionMismatch { .. } | ManifestRefusal::NameMismatch { .. }
                )
            ),
            "{to}: {err:?}"
        );
        assert_eq!(
            err.outcome(),
            (Outcome::Failure, Class::Release, "manifest_mismatch"),
            "{to}"
        );
    }
}

/// Catches a manifest refusal turned into something else: an unknown key and a value nested
/// deeper than a scalar are the loader's own refusals, class release, with the loader's token.
#[test]
fn a_manifest_the_loader_refuses_is_a_release_failure() {
    let unknown = format!("{MANIFEST}user = \"root\"\n");
    let err = refused_manifest("u-unknown", &unknown);
    assert!(
        matches!(&err, UnpackError::Manifest(ManifestRefusal::UnknownKey { key, .. }) if key == "user"),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "manifest_mismatch")
    );
    let deep = format!(
        "{MANIFEST}[params]\nx = {}1{}\n",
        "[".repeat(64),
        "]".repeat(64)
    );
    let err = refused_manifest("u-nested", &deep);
    let UnpackError::Manifest(refusal) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, refusal.reason())
    );
    assert_eq!(refusal.reason(), "manifest_mismatch");
}

/// Catches a package the daemon is too old for being refused as bad, or kept: a `min_hostkeeper`
/// above the running daemon is `Deferred`, environment, `daemon_too_old`, and the staging
/// directory is removed.
#[test]
fn a_package_needing_a_newer_hostkeeper_is_deferred() {
    let manifest = MANIFEST.replace("min_hostkeeper = \"0.1.0\"", "min_hostkeeper = \"999.0.0\"");
    let err = refused_manifest("u-too-old", &manifest);
    assert!(
        matches!(
            &err,
            UnpackError::Manifest(ManifestRefusal::DaemonTooOld { .. })
        ),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Deferred, Class::Environment, "daemon_too_old")
    );
    assert!(hostkeeper_unpack::running_hostkeeper().major < 999);
}

/// Unpacks the valid package once and returns the rig, the archive's path and the order.
fn unpacked(name: &str) -> (Rig, std::path::PathBuf, hostkeeper_unpack::UnpackOrder) {
    let rig = Rig::new(name);
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    unpack(&path, &order, &limits(), &rig.host()).unwrap();
    (rig, path, order)
}

/// Catches a retry that unpacks again, or that takes a changed version for the sealed one: a
/// second unpack of the same order writes nothing and returns the version (`fresh: false`); with
/// a file of the tree changed it is `artifact_changed` and the tree is left as it is.
#[test]
fn an_existing_sealed_version_is_not_unpacked_again() {
    let (rig, path, order) = unpacked("u-again");
    let fake = Faulty::new(rig.host());
    let again = unpack(&path, &order, &limits(), &fake).unwrap();
    assert!(!again.fresh);
    assert!(
        fake.calls().iter().all(|c| !c.writes()),
        "{:?}",
        fake.calls()
    );
    let x = rig.service().join("1.5.0/bin/x");
    fs::set_permissions(
        rig.service().join("1.5.0/bin"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::set_permissions(&x, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&x, "#!y\n").unwrap();
    let fake = Faulty::new(rig.host());
    let err = unpack(&path, &order, &limits(), &fake).unwrap_err();
    assert!(
        matches!(&err, UnpackError::ArtifactChanged { .. }),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "artifact_changed")
    );
    assert!(
        fake.calls().iter().all(|c| !c.writes()),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fs::read(&x).unwrap(), b"#!y\n");
}

/// Catches a version on the host taken for the order's when it is not, or a seal that cannot be
/// read taken for a changed tree: a version sealed for another archive and one with no seal are
/// `artifact_changed`; a seal that cannot be read now and a tree that cannot be read now are
/// tried again (`io_error`); a seal of schema 2 is `daemon_too_old`; each touches nothing.
#[test]
fn a_version_on_the_host_that_is_not_the_orders_is_refused_untouched() {
    let (rig, path, order) = unpacked("u-other-seal");
    let seal = rig.service().join("1.5.0.seal");
    let sealed = fs::read(&seal).unwrap();
    let run = |want: (Outcome, Class, &str)| {
        let fake = Faulty::new(rig.host());
        let err = unpack(&path, &order, &limits(), &fake).unwrap_err();
        assert_eq!(err.outcome(), want, "{err:?}");
        assert!(
            fake.calls().iter().all(|c| !c.writes()),
            "{:?}",
            fake.calls()
        );
        err
    };
    let changed = (Outcome::Failure, Class::Release, "artifact_changed");
    let again = (Outcome::Deferred, Class::Environment, "io_error");

    let other = order_for_other_archive(&order);
    let fake = Faulty::new(rig.host());
    let err = unpack(&path, &other, &limits(), &fake).unwrap_err();
    assert_eq!(err.outcome(), changed);
    assert!(
        err.to_string().contains("was sealed for the archive"),
        "{err}"
    );

    fs::set_permissions(&seal, fs::Permissions::from_mode(0o000)).unwrap();
    let err = run(again);
    assert!(matches!(err, UnpackError::SealUnreadable { .. }), "{err:?}");
    fs::set_permissions(&seal, fs::Permissions::from_mode(0o644)).unwrap();

    fs::write(&seal, "{\"schema\":1,").unwrap();
    assert!(matches!(run(again), UnpackError::SealUnreadable { .. }));
    fs::write(&seal, "{\"schema\":2}").unwrap();
    let err = run((Outcome::Deferred, Class::Environment, "daemon_too_old"));
    assert!(
        matches!(err, UnpackError::SealSchema { found: 2, .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("schema 2"), "{err}");

    fs::write(&seal, sealed).unwrap();
    let bin = rig.service().join("1.5.0/bin");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(matches!(run(again), UnpackError::Io { .. }));
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
    unpack(&path, &order, &limits(), &rig.host()).unwrap();

    fs::remove_file(&seal).unwrap();
    // A version with no seal: remove_orphans keeps the version, the unpack refuses it.
    let err = run(changed);
    assert!(err.to_string().contains("has no seal"), "{err}");
    assert!(rig.service().join(VERSION).is_dir());
}

fn order_for_other_archive(
    order: &hostkeeper_unpack::UnpackOrder,
) -> hostkeeper_unpack::UnpackOrder {
    let mut other = order.clone();
    other.package_digest = hostkeeper_config::daemon::Sha256Hex::parse(&"cd".repeat(32)).unwrap();
    other
}

/// Catches an archive file that cannot be read being reported as a bad package: a missing
/// archive and a directory in its place are `io_error`, tried again, and nothing stays.
#[test]
fn an_archive_that_cannot_be_read_is_tried_again() {
    let rig = Rig::new("u-unreadable");
    let order = order(VERSION, &"ab".repeat(32));
    for path in [
        rig.scratch.path().join("absent"),
        rig.scratch.path().to_path_buf(),
    ] {
        let err = unpack(&path, &order, &limits(), &rig.host()).unwrap_err();
        assert!(matches!(err, UnpackError::Io { .. }), "{err:?}");
        assert_eq!(
            err.outcome(),
            (Outcome::Deferred, Class::Environment, "io_error")
        );
        assert!(rig.listing().is_empty(), "{:?}", rig.listing());
    }
}

/// Catches a staged tree that did not reach the disk as it was written being sealed: a file
/// whose bytes land with one changed is `bad_archive`, the tree not as written; a directory that
/// cannot be read back is `io_error`; neither leaves a version.
#[test]
fn a_staged_tree_that_reads_back_otherwise_is_refused() {
    let rig = Rig::new("u-readback");
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    let fake = Faulty::new(rig.host()).corrupt("bin/x");
    let err = unpack(&path, &order, &limits(), &fake).unwrap_err();
    assert!(
        matches!(&err, UnpackError::BadArchive { rule: Rule::NotAsWritten, entry, .. }
            if entry == "the staged tree"),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "bad_archive")
    );
    assert!(rig.listing().is_empty(), "{:?}", rig.listing());

    let rig = Rig::new("u-readback-io");
    let (path, order) = rig.archive("c.tar.zst", &zstd(&valid_tar().end().bytes()));
    let fake = Faulty::new(rig.host()).dir_mode("/bin", 0o300);
    let err = unpack(&path, &order, &limits(), &fake).unwrap_err();
    assert!(matches!(err, UnpackError::Io { .. }), "{err:?}");
    assert_eq!(
        err.outcome(),
        (Outcome::Deferred, Class::Environment, "io_error")
    );
    assert!(!rig.service().join(VERSION).exists());
    support::open_up(&rig.service());
    hostkeeper_unpack::remove_orphans(&order.service, &rig.host()).unwrap();
    assert!(rig.listing().is_empty(), "{:?}", rig.listing());
}

/// Catches the bounds of the daemon config not reaching the unpack: `Limits` takes
/// `max_unpacked_bytes` and `max_entries` from the config's bounds as they are, and an unpacked
/// version reads back, prints and compares as the value it is.
#[test]
fn the_limits_are_the_daemon_configs_bounds() {
    let bounds = hostkeeper_config::daemon::Bounds {
        max_unpacked_bytes: 123_456,
        max_entries: 789,
        ..Default::default()
    };
    let limits = hostkeeper_unpack::Limits::from(&bounds);
    assert_eq!(
        limits,
        hostkeeper_unpack::Limits {
            max_unpacked_bytes: 123_456,
            max_entries: 789,
        }
    );
    assert!(format!("{limits:?}").contains("123456"));
    let (rig, _, _) = unpacked("u-values");
    let host = rig.host();
    assert!(format!("{host:?}").contains("HostFs"));
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    let again = unpack(&path, &order, &support::limits(), &host).unwrap();
    assert_eq!(again.clone(), again);
    assert!(format!("{again:?}").contains("1.5.0"));
    assert_eq!(order.clone(), order);
    assert!(format!("{order:?}").contains("tierstore"));
}
