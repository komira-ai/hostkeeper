//! The public formats (the tree digest and the seal, AGENTS.md B8), the reason of every error,
//! and the production port's own refusals.

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::version::{
    FillError, FillOrder, Filled, HostFs, Owner, PackageFs, REASON_ARTIFACT_CHANGED,
    REASON_DAEMON_TOO_OLD, REASON_IO_ERROR, REASON_NO_SPACE, REASON_RESERVED_VERSION,
    REASON_VERSION_TOO_LONG, UnpackError, stage_filled,
};
use hostkeeper_steps::{SEAL_SCHEMA, Seal, SealError, tree_digest};

use super::support::{self, MANIFEST, Rig, SERVICE, VERSION, me, sha256_hex};

/// The tree digest of the tree of [`write_tree`] in the order of [`PACKAGE_ORDER`]: the manifest,
/// `bin/x`, `units/tierstore.service`, `files/c.conf` and their three directories.
const GOLDEN_TREE_DIGEST: &str = "2060e19cfc96ecacbcb0122b8c4126c0319a439840f75bd4b458c8ef4e134a45";

/// The tree digest of a tree in which full-name order and directory-by-directory order differ:
/// `bin/a-` sorts before `bin/a/x` (`-` is below `/`), though `bin/a` sorts before `bin/a-`.
const GOLDEN_PREFIX_TREE_DIGEST: &str =
    "85dfbc97472ae3de86d597f5ed577d5c8aaccf6a7156f9a34f2112171970dd6f";

/// One entry of a tree: a directory, or a file with its mode and bytes.
enum Entry {
    Dir(&'static str),
    File(&'static str, u32, &'static [u8]),
}

use Entry::{Dir, File};

/// The package of the golden digest, each directory before what is in it.
const PACKAGE_ORDER: [Entry; 7] = [
    File("manifest.toml", 0o644, MANIFEST.as_bytes()),
    Dir("bin"),
    File("bin/x", 0o755, b"#!x\n"),
    Dir("units"),
    File("units/tierstore.service", 0o644, b"[Service]\n"),
    Dir("files"),
    File("files/c.conf", 0o640, b"c\n"),
];

/// Writes `entries`, in their order, into `stage`, with the modes the tree digest records: a
/// file 0555 when its mode has an execute bit and 0444 otherwise, a directory 0555, the
/// directories made read-only last, the deepest first.
fn write_tree(stage: &Path, entries: &[&Entry]) -> Result<(), String> {
    let write = |e: io::Error| e.to_string();
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Dir(name) => {
                fs::create_dir(stage.join(name)).map_err(write)?;
                dirs.push(*name);
            }
            File(name, mode, bytes) => {
                fs::write(stage.join(name), bytes).map_err(write)?;
                let normal = if mode & 0o111 != 0 { 0o555 } else { 0o444 };
                fs::set_permissions(stage.join(name), fs::Permissions::from_mode(normal))
                    .map_err(write)?;
            }
        }
    }
    dirs.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for name in dirs {
        fs::set_permissions(stage.join(name), fs::Permissions::from_mode(0o555)).map_err(write)?;
    }
    Ok(())
}

fn digest() -> Sha256Hex {
    Sha256Hex::parse(&sha256_hex(b"the package")).unwrap()
}

/// Publishes `entries` as the version of the tests on the healthy port.
fn publish_tree(rig: &Rig, entries: &[&Entry]) -> Filled {
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new(VERSION).unwrap();
    let package = digest();
    let order = FillOrder {
        service: &service,
        version: &version,
        package_digest: &package,
    };
    let host = rig.host();
    stage_filled(&order, &host, &mut |_| Ok::<(), String>(()), &mut |stage| {
        write_tree(stage, entries)
    })
    .unwrap()
}

fn package() -> (Rig, Seal) {
    let rig = Rig::new("fmt-golden");
    let entries: Vec<&Entry> = PACKAGE_ORDER.iter().collect();
    publish_tree(&rig, &entries);
    let layout = hostkeeper_steps::Layout::package(&rig.service());
    let seal =
        Seal::parse(&hostkeeper_steps::read_seal(&layout.seal_path(VERSION)).unwrap()).unwrap();
    (rig, seal)
}

/// Calls the fill-free path of `stage_filled` on a version that is there, and returns whether it
/// was taken as it is (`fresh: false`).
fn taken_as_it_is(rig: &Rig) -> Result<Filled, UnpackError> {
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new(VERSION).unwrap();
    let package = digest();
    let order = FillOrder {
        service: &service,
        version: &version,
        package_digest: &package,
    };
    stage_filled(
        &order,
        &rig.host(),
        &mut |_| Ok::<(), String>(()),
        &mut |_| panic!("filled a version that is there"),
    )
    .map_err(|e| match e {
        FillError::Unpack(e) => e,
        FillError::Fill(why) => panic!("{why}"),
    })
}

/// Catches any change of the tree digest's format (the record framing, the kind letters, the
/// mode's spelling, the order of entries, what is hashed): a fixed tree has a committed digest.
#[test]
fn the_tree_digest_has_a_fixed_value() {
    let (rig, seal) = package();
    assert_eq!(seal.tree_digest(), GOLDEN_TREE_DIGEST);
    assert_eq!(
        tree_digest(&rig.service().join(VERSION)).unwrap(),
        GOLDEN_TREE_DIGEST
    );
}

/// Catches a format change that an older or newer release could not read: the tree digest of
/// the fixed tree is the committed one, a seal with a field this release does not know is
/// read, and a version sealed so is taken as it is by a second publish.
#[test]
fn the_seal_and_tree_digest_formats_are_additive_only() {
    let (rig, seal) = package();
    assert_eq!(seal.tree_digest(), GOLDEN_TREE_DIGEST);
    let newer = format!(
        "{{\"schema\":1,\"tree_digest\":\"{}\",\"package_digest\":\"{}\",\"signed\":{{\"by\":\"ci\"}}}}",
        seal.tree_digest(),
        seal.package_digest()
    );
    assert_eq!(Seal::parse(newer.as_bytes()).unwrap(), seal);
    let file = rig.service().join("1.5.0.seal");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&file, &newer).unwrap();
    let again = taken_as_it_is(&rig).unwrap();
    assert!(!again.fresh);
}

/// Catches a seal reader that is not additive or that halts on a seal it cannot read: an extra
/// field reads; schema 2 is its own error; a truncated seal and one that is not UTF-8 are
/// unreadable, which a publish reports as a deferral to try again, never as a bad package.
#[test]
fn the_seal_is_read_additively() {
    let (rig, seal) = package();
    assert_eq!(SEAL_SCHEMA, 1);
    let json = seal.to_json();
    let extra = json.replacen('}', ",\"note\":\"x\"}", 1);
    assert_eq!(Seal::parse(extra.as_bytes()).unwrap(), seal);
    let two = json.replacen("\"schema\":1", "\"schema\":2", 1);
    assert_eq!(Seal::parse(two.as_bytes()), Err(SealError::Schema(2)));
    let file = rig.service().join("1.5.0.seal");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&file, &two).unwrap();
    let err = taken_as_it_is(&rig).unwrap_err();
    assert!(
        matches!(err, UnpackError::SealSchema { found: 2, .. }),
        "{err:?}"
    );
    assert_eq!(err.reason(), "daemon_too_old");
    for bad in [&json.as_bytes()[..json.len() - 2], &[0xc3, 0x28, 0xff][..]] {
        assert!(matches!(Seal::parse(bad), Err(SealError::Unreadable(_))));
        fs::write(&file, bad).unwrap();
        let err = taken_as_it_is(&rig).unwrap_err();
        assert!(matches!(err, UnpackError::SealUnreadable { .. }), "{err:?}");
        assert_eq!(err.reason(), "io_error");
        assert!(err.to_string().contains("cannot be read"), "{err}");
    }
}

/// Catches an error reported with the wrong reason token, and a variant added without its row:
/// one error of each variant has the reason of its row, the reason is one of the published
/// constants and a token the hook contract accepts, and its text says what happened.
#[test]
fn every_error_has_one_reason_and_says_what_happened() {
    let errors = [
        (
            UnpackError::NoSpace(io::Error::from_raw_os_error(28)),
            REASON_NO_SPACE,
            "no space left while publishing: No space left on device (os error 28)",
        ),
        (
            UnpackError::ArtifactChanged {
                version: "1.5.0".into(),
                why: "has no seal".into(),
            },
            REASON_ARTIFACT_CHANGED,
            "version 1.5.0 is on the host but has no seal",
        ),
        (
            UnpackError::SealUnreadable {
                version: "1.5.0".into(),
                why: "x".into(),
            },
            REASON_IO_ERROR,
            "the seal of version 1.5.0 cannot be read: x",
        ),
        (
            UnpackError::SealSchema {
                version: "1.5.0".into(),
                found: 2,
            },
            REASON_DAEMON_TOO_OLD,
            "the seal of version 1.5.0 has schema 2, which this daemon does not read",
        ),
        (
            UnpackError::ReservedVersion {
                version: "current".into(),
            },
            REASON_RESERVED_VERSION,
            "the version \"current\" is a name the package directory holds for itself",
        ),
        (
            UnpackError::VersionTooLong {
                version: "1.0".into(),
            },
            REASON_VERSION_TOO_LONG,
            "the version \"1.0\" is too long for the names the package directory derives from it",
        ),
        (
            UnpackError::Io {
                what: "cannot open the stage",
                source: io::Error::other("gone"),
            },
            REASON_IO_ERROR,
            "cannot open the stage: gone",
        ),
    ];
    for (error, reason, text) in &errors {
        assert_eq!(error.reason(), *reason, "{error:?}");
        assert_eq!(error.to_string(), *text);
    }
    let tokens = [
        REASON_NO_SPACE,
        REASON_ARTIFACT_CHANGED,
        REASON_IO_ERROR,
        REASON_DAEMON_TOO_OLD,
        REASON_RESERVED_VERSION,
        REASON_VERSION_TOO_LONG,
    ];
    assert_eq!(
        tokens,
        [
            "no_space",
            "artifact_changed",
            "io_error",
            "daemon_too_old",
            "reserved_version",
            "version_too_long"
        ]
    );
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
        let err = host.create_file(Path::new(bad)).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    fs::create_dir(rig.packages.join("real")).unwrap();
    symlink("real", rig.packages.join("via")).unwrap();
    assert!(host.create_stage(Path::new("via")).is_err());
    assert!(host.create_file(Path::new("via/y")).is_err());
    assert!(!rig.packages.join("real/y").exists());
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
    host.create_service(Path::new("mine")).unwrap();
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
    let err = root.create_service(Path::new("roots")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
}

/// Catches the tree digest walking directory by directory instead of in the byte order of full
/// names, and a digest that depends on the order the tree was written in: the prefix tree has its
/// committed digest, and the same tree written in three other orders has the same one.
#[test]
fn the_tree_digest_orders_full_names_and_not_the_order_written() {
    const TREE: [Entry; 7] = [
        Dir("bin"),
        Dir("bin/a"),
        File("bin/a/x", 0o755, b"x\n"),
        File("bin/a-", 0o644, b"a-\n"),
        Dir("files"),
        File("files/c.conf", 0o644, b"c\n"),
        File("manifest.toml", 0o644, MANIFEST.as_bytes()),
    ];
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
        // A directory is made before what is in it, whatever the order of the list.
        let mut entries: Vec<&Entry> = order.iter().map(|&at| &TREE[at]).collect();
        entries.sort_by_key(|entry| match entry {
            Dir(name) => name.len(),
            File(..) => usize::MAX,
        });
        let filled = publish_tree(&rig, &entries);
        assert_eq!(
            tree_digest(&filled.dir).unwrap(),
            GOLDEN_PREFIX_TREE_DIGEST,
            "order {i}"
        );
    }
}
