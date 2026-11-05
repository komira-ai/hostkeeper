//! The public formats (the tree digest and the seal, AGENTS.md B8), the outcome of every error,
//! and the production port's own refusals.

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_config::manifest::{HostkeeperVersion, ManifestRefusal};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::tree_digest;
use hostkeeper_unpack::{
    HostFs, Owner, PackageFs, Rule, SEAL_SCHEMA, Seal, SealError, UnpackError, unpack,
};
use support::{Rig, VERSION, limits, me, valid_tar, zstd};

/// The tree digest of the valid package of the tests (`valid_tar`): the manifest, `bin/x`,
/// `units/tierstore.service`, `files/c.conf` and their three directories, unpacked.
const GOLDEN_TREE_DIGEST: &str = "18479648f09d5bd43998825db7c2339a2922600de221d2386b627110c516083c";

fn unpack_valid(name: &str) -> (Rig, hostkeeper_unpack::Unpacked) {
    let rig = Rig::new(name);
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    let unpacked = unpack(&path, &order, &limits(), &rig.host()).unwrap();
    (rig, unpacked)
}

/// Catches any change of the tree digest's format (the record framing, the kind letters, the
/// mode's spelling, the order of entries, what is hashed): a fixed tree has a committed digest.
#[test]
fn the_tree_digest_has_a_fixed_value() {
    let (rig, unpacked) = unpack_valid("fmt-golden");
    assert_eq!(unpacked.seal.tree_digest(), GOLDEN_TREE_DIGEST);
    assert_eq!(
        tree_digest(&rig.service().join(VERSION)).unwrap(),
        GOLDEN_TREE_DIGEST
    );
}

/// Catches a format change that an older or newer release could not read: the tree digest of
/// the fixed tree is the committed one, a seal with a field this release does not know is
/// read, and a version sealed so is taken as it is by a second unpack.
#[test]
fn the_seal_and_tree_digest_formats_are_additive_only() {
    let (rig, unpacked) = unpack_valid("fmt-additive");
    assert_eq!(unpacked.seal.tree_digest(), GOLDEN_TREE_DIGEST);
    let newer = format!(
        "{{\"schema\":1,\"tree_digest\":\"{}\",\"package_digest\":\"{}\",\"signed\":{{\"by\":\"ci\"}}}}",
        unpacked.seal.tree_digest(),
        unpacked.seal.package_digest()
    );
    assert_eq!(Seal::parse(newer.as_bytes()).unwrap(), unpacked.seal);
    let seal = rig.service().join("1.5.0.seal");
    fs::set_permissions(&seal, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&seal, &newer).unwrap();
    let archive = rig.scratch.path().join("a.tar.zst");
    let order = support::order(VERSION, unpacked.seal.package_digest());
    let again = unpack(&archive, &order, &limits(), &rig.host()).unwrap();
    assert!(!again.fresh);
    assert_eq!(again.seal, unpacked.seal);
}

/// Catches a seal reader that is not additive or that halts on a seal it cannot read: an extra
/// field reads; schema 2 is its own error; a truncated seal and one that is not UTF-8 are
/// unreadable, which an unpack reports as a deferral to try again, never as a bad package.
#[test]
fn the_seal_is_read_additively() {
    let (rig, unpacked) = unpack_valid("fmt-seal");
    assert_eq!(SEAL_SCHEMA, 1);
    let json = unpacked.seal.to_json();
    let extra = json.replacen('}', ",\"note\":\"x\"}", 1);
    assert_eq!(Seal::parse(extra.as_bytes()).unwrap(), unpacked.seal);
    let two = json.replacen("\"schema\":1", "\"schema\":2", 1);
    assert_eq!(Seal::parse(two.as_bytes()), Err(SealError::Schema(2)));
    let seal = rig.service().join("1.5.0.seal");
    fs::set_permissions(&seal, fs::Permissions::from_mode(0o644)).unwrap();
    let archive = rig.scratch.path().join("a.tar.zst");
    let order = support::order(VERSION, unpacked.seal.package_digest());
    for bad in [&json.as_bytes()[..json.len() - 2], &[0xc3, 0x28, 0xff][..]] {
        assert!(matches!(Seal::parse(bad), Err(SealError::Unreadable(_))));
        fs::write(&seal, bad).unwrap();
        let err = unpack(&archive, &order, &limits(), &rig.host()).unwrap_err();
        assert!(matches!(err, UnpackError::SealUnreadable { .. }), "{err:?}");
        assert_eq!(
            err.outcome(),
            (Outcome::Deferred, Class::Environment, "io_error")
        );
        assert!(err.to_string().contains("cannot be read"), "{err}");
    }
}

/// The outcome each variant must have. A match with no wildcard: a new variant does not compile
/// until it has a row here.
fn row(error: &UnpackError) -> (Outcome, Class, &'static str) {
    let failed = |reason| (Outcome::Failure, Class::Release, reason);
    let deferred = |reason| (Outcome::Deferred, Class::Environment, reason);
    match error {
        UnpackError::BadArchive { .. } => failed("bad_archive"),
        UnpackError::DigestMismatch { .. } => failed("digest_mismatch"),
        UnpackError::Manifest(ManifestRefusal::DaemonTooOld { .. }) => deferred("daemon_too_old"),
        UnpackError::Manifest(_) => failed("manifest_mismatch"),
        UnpackError::NoSpace(_) => deferred("no_space"),
        UnpackError::ArtifactChanged { .. } => failed("artifact_changed"),
        UnpackError::SealUnreadable { .. } => deferred("io_error"),
        UnpackError::SealSchema { .. } => deferred("daemon_too_old"),
        UnpackError::ReservedVersion { .. } => failed("reserved_version"),
        UnpackError::VersionTooLong { .. } => failed("version_too_long"),
        UnpackError::Io { .. } => deferred("io_error"),
    }
}

/// Catches an error reported with the wrong class or token, and a variant added without its row:
/// one error of each variant has the outcome, class and reason of its row, the reason is one of
/// the published constants, and its text says what happened.
#[test]
fn every_unpack_error_maps_to_one_class_and_reason() {
    let version = HostkeeperVersion {
        major: 1,
        minor: 0,
        patch: 0,
    };
    let errors = [
        (
            UnpackError::BadArchive {
                entry: "bin/\\xff".into(),
                rule: Rule::NotUtf8,
                at: 512,
            },
            "bad archive: bin/\\xff: a name that is not UTF-8 (after 512 decompressed bytes)",
        ),
        (
            UnpackError::DigestMismatch {
                expected: "a".into(),
                found: "b".into(),
            },
            "the archive's SHA-256 is b, not a",
        ),
        (
            UnpackError::Manifest(ManifestRefusal::TooLarge),
            "the manifest is larger than 65536 bytes",
        ),
        (
            UnpackError::Manifest(ManifestRefusal::DaemonTooOld {
                needs: version,
                running: version,
            }),
            "the package needs hostkeeper 1.0.0; this daemon is 1.0.0",
        ),
        (
            UnpackError::NoSpace(io::Error::from_raw_os_error(28)),
            "no space left while unpacking: No space left on device (os error 28)",
        ),
        (
            UnpackError::ArtifactChanged {
                version: "1.5.0".into(),
                why: "has no seal".into(),
            },
            "version 1.5.0 is on the host but has no seal",
        ),
        (
            UnpackError::SealUnreadable {
                version: "1.5.0".into(),
                why: "x".into(),
            },
            "the seal of version 1.5.0 cannot be read: x",
        ),
        (
            UnpackError::SealSchema {
                version: "1.5.0".into(),
                found: 2,
            },
            "the seal of version 1.5.0 has schema 2, which this daemon does not read",
        ),
        (
            UnpackError::ReservedVersion {
                version: "current".into(),
            },
            "the version \"current\" is a name the package directory holds for itself",
        ),
        (
            UnpackError::VersionTooLong {
                version: "1.0".into(),
            },
            "the version \"1.0\" is too long for the names the package directory derives from it",
        ),
        (
            UnpackError::Io {
                what: "cannot open the archive",
                source: io::Error::other("gone"),
            },
            "cannot open the archive: gone",
        ),
    ];
    let tokens = [
        hostkeeper_unpack::REASON_BAD_ARCHIVE,
        hostkeeper_unpack::REASON_DIGEST_MISMATCH,
        hostkeeper_unpack::REASON_MANIFEST_MISMATCH,
        hostkeeper_unpack::REASON_DAEMON_TOO_OLD,
        hostkeeper_unpack::REASON_NO_SPACE,
        hostkeeper_unpack::REASON_ARTIFACT_CHANGED,
        hostkeeper_unpack::REASON_IO_ERROR,
        hostkeeper_unpack::REASON_RESERVED_VERSION,
        hostkeeper_unpack::REASON_VERSION_TOO_LONG,
    ];
    for (error, text) in &errors {
        assert_eq!(error.outcome(), row(error), "{error:?}");
        assert!(tokens.contains(&error.outcome().2), "{error:?}");
        assert!(hostkeeper_hook::is_reason_token(error.outcome().2));
        assert_eq!(error.to_string(), *text);
    }
}

/// Catches a rule whose text does not say which rule it is: every rule names itself.
#[test]
fn every_rule_says_what_it_refuses() {
    let rules = [
        (Rule::TooLarge, "max_unpacked_bytes"),
        (Rule::TooManyEntries, "max_entries"),
        (Rule::NameTooLong, "over 255 bytes"),
        (Rule::TooDeep, "deeper than 16"),
        (Rule::ManifestTooLarge, "manifest.toml over 65536"),
        (Rule::TopLevel, "top-level"),
        (Rule::Kind(b'2'), "tar type '2'"),
        (Rule::BadPath, "absolute"),
        (Rule::NotUtf8, "UTF-8"),
        (Rule::ParentMissing, "parent directory"),
        (Rule::Duplicate, "came before"),
        (Rule::CaseFold, "case folding"),
        (Rule::ExtendedAttribute, "extended attribute"),
        (Rule::PaxRecord, "pax record"),
        (Rule::SpecialBits, "setuid"),
        (Rule::Header, "tar header"),
        (Rule::Trailing, "after the end"),
        (Rule::SecondFrame, "after the zstd frame"),
        (Rule::Truncated, "ends before"),
        (Rule::Window { requested: 9 }, "window of 9 bytes"),
        (Rule::Decoder("bad".into()), "invalid: bad"),
        (Rule::NotAsWritten, "reads back"),
    ];
    for (rule, words) in rules {
        assert!(rule.to_string().contains(words), "{rule:?}: {rule}");
    }
}

/// Catches the production port following a link or taking a path that is not plain: the
/// packages directory itself may not be a link, a path with `..`, an absolute one and an empty
/// one are refused, a component that is a link is not followed, an absent tree is removed
/// without error, and a packages directory that cannot be written refuses a new service.
#[test]
fn the_host_port_takes_plain_paths_and_follows_no_link() {
    let rig = Rig::new("fmt-host");
    let link = rig.scratch.path().join("link");
    symlink(&rig.packages, &link).unwrap();
    assert!(HostFs::open(&link, me()).is_err());
    assert!(HostFs::open(&rig.scratch.path().join("absent"), me()).is_err());
    let host = rig.host();
    for bad in ["../x", "/abs", ""] {
        let err = host.create_dir(Path::new(bad)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    fs::create_dir(rig.packages.join("real")).unwrap();
    symlink("real", rig.packages.join("via")).unwrap();
    assert!(host.create_dir(Path::new("via/x")).is_err());
    assert!(host.create_file(Path::new("via/y")).is_err());
    assert!(!rig.packages.join("real/x").exists() && !rig.packages.join("real/y").exists());
    host.remove_tree(Path::new("nothing/here")).unwrap_err();
    host.remove_tree(Path::new("nothing")).unwrap();
    host.remove_tree(Path::new("via")).unwrap();
    assert!(rig.packages.join("real").is_dir());
    assert_eq!(host.real_path(Path::new("a/b")), rig.packages.join("a/b"));

    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o555)).unwrap();
    let err = host.create_service(Path::new("new-service")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    fs::set_permissions(&rig.packages, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(rig.packages.join("afile"), "x").unwrap();
    assert!(host.create_stage(Path::new("afile")).is_err());
}

/// Catches the production owner being anything but root, and a port that gives what it makes to
/// no one: what [`Owner::ROOT`] creates is root's, so for a user who is not root the create is
/// refused rather than left owned by that user.
#[test]
fn what_the_host_port_creates_is_given_to_its_owner() {
    assert_eq!(Owner::ROOT, Owner { uid: 0, gid: 0 });
    let rig = Rig::new("fmt-owner");
    let host = rig.host();
    host.create_dir(Path::new("mine")).unwrap();
    assert_eq!(
        support::owner_of(&rig.packages.join("mine")),
        (me().uid, me().gid)
    );
    if me().uid == 0 {
        // Root may give a file to root; the refusal below is what a user who is not root sees.
        eprintln!("skipped the refusal: these tests run as root");
        return;
    }
    let root = HostFs::open(&rig.packages, Owner::ROOT).unwrap();
    let err = root.create_dir(Path::new("roots")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
}

/// The tree digest of a tree in which full-name order and directory-by-directory order differ:
/// `bin/a-` sorts before `bin/a/x` (`-` is below `/`), though `bin/a` sorts before `bin/a-`.
const GOLDEN_PREFIX_TREE_DIGEST: &str =
    "d478cc9a32cd63940b9680413e71c4a685b40e94f2c73186311cc5906e04b473";

/// The prefix tree's entries, in the order of `order` (indexes into the list below), with each
/// directory before what is in it.
fn prefix_tree(order: &[usize]) -> Vec<u8> {
    let manifest = support::MANIFEST.as_bytes();
    let mut tar = support::Tar::new();
    for &i in order {
        match i {
            0 => tar.dir("bin"),
            1 => tar.dir("bin/a"),
            2 => tar.file("bin/a/x", 0o755, b"x\n"),
            3 => tar.file("bin/a-", 0o644, b"a-\n"),
            4 => tar.dir("files"),
            5 => tar.file("files/c.conf", 0o644, b"c\n"),
            _ => tar.file("manifest.toml", 0o644, manifest),
        };
    }
    zstd(&tar.end().bytes())
}

/// Catches the tree digest walking directory by directory instead of in the byte order of full
/// names, and a digest that depends on the order of the archive: the prefix tree has its
/// committed digest, and the same tree in three other archive orders has the same one.
#[test]
fn the_tree_digest_orders_full_names_and_not_the_archive() {
    for (i, order) in [
        [0, 1, 2, 3, 4, 5, 6],
        [6, 4, 5, 0, 3, 1, 2],
        [0, 3, 1, 2, 6, 4, 5],
        [4, 0, 1, 5, 2, 6, 3],
    ]
    .iter()
    .enumerate()
    {
        let rig = Rig::new("fmt-prefix");
        let (path, order_of) = rig.archive("p.tar.zst", &prefix_tree(order));
        let unpacked = unpack(&path, &order_of, &limits(), &rig.host()).unwrap();
        assert_eq!(
            unpacked.seal.tree_digest(),
            GOLDEN_PREFIX_TREE_DIGEST,
            "order {i}"
        );
    }
}
