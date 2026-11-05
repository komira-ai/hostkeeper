//! The platform of a package at unpack: the order's platform reaches the manifest check, a sealed
//! version is not taken for another platform, and the instruction-set variants inside a package
//! are files like any other (design section 5.5).

mod support;

use std::fs;
use std::path::Path;

use hostkeeper_config::manifest::ManifestRefusal;
use hostkeeper_config::platform::Platform;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::tree_digest;
// The checks of the host the unpack runs on: these tests run the Linux path.
use hostkeeper_unpack::Platform::Linux;
use hostkeeper_unpack::{UnpackError, unpack};
use support::{Faulty, MANIFEST, Rig, Tar, VERSION, limits, valid_tar, zstd};

/// A valid package whose manifest names `platform`.
fn package_for(platform: Platform) -> Vec<u8> {
    let manifest = MANIFEST.replace(
        "platform = \"linux-x86_64\"",
        &format!("platform = \"{platform}\""),
    );
    let mut tar = Tar::new();
    tar.file("manifest.toml", 0o644, manifest.as_bytes())
        .dir("bin")
        .file("bin/x", 0o755, b"#!x\n")
        .dir("files")
        .file("files/c.conf", 0o644, b"c\n");
    zstd(&tar.end().bytes())
}

/// Every regular file below `dir`, as its path relative to `dir` and its bytes, sorted.
fn files_below(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                found.push((name, fs::read(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

/// Catches an order's platform that does not reach the manifest check: a package built for each
/// platform unpacks for an order naming that platform, and its manifest reads as that platform.
#[test]
fn a_package_unpacks_for_the_platform_its_order_names() {
    for platform in Platform::ALL {
        let rig = Rig::new("p-each");
        let (path, mut order) = rig.archive("a.tar.zst", &package_for(platform));
        order.platform = platform;
        let unpacked = unpack(&path, &order, &limits(), &rig.host(), Linux).unwrap();
        assert!(unpacked.fresh, "{platform}");
        assert_eq!(unpacked.manifest.platform, platform);
        assert_eq!(
            rig.listing(),
            [VERSION.to_owned(), format!("{VERSION}.seal")]
        );
    }
}

/// Catches a sealed version taken for another platform's: a version unpacked for linux-x86_64,
/// asked for again by an order for linux-arm64 with the same archive digest, is refused as the
/// loader's `PlatformMismatch`, release, `manifest_mismatch`, and the tree and its seal are left
/// as they were, with nothing written.
#[test]
fn a_sealed_version_is_refused_for_another_platform_and_left_alone() {
    let rig = Rig::new("p-sealed");
    let (path, order) = rig.archive("a.tar.zst", &zstd(&valid_tar().end().bytes()));
    unpack(&path, &order, &limits(), &rig.host(), Linux).unwrap();
    let dir = rig.service().join(VERSION);
    let seal_path = rig.service().join(format!("{VERSION}.seal"));
    let (seal, tree, files) = (
        fs::read(&seal_path).unwrap(),
        tree_digest(&dir).unwrap(),
        files_below(&dir),
    );
    let mut arm = order.clone();
    arm.platform = Platform::LinuxArm64;
    let fake = Faulty::new(rig.host());
    let err = unpack(&path, &arm, &limits(), &fake, Linux).unwrap_err();
    assert!(
        matches!(
            &err,
            UnpackError::Manifest(ManifestRefusal::PlatformMismatch {
                expected: Platform::LinuxArm64,
                found: Platform::LinuxX86_64,
            })
        ),
        "{err:?}"
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "manifest_mismatch")
    );
    assert!(
        fake.calls().iter().all(|c| !c.writes()),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fs::read(&seal_path).unwrap(), seal);
    assert_eq!(tree_digest(&dir).unwrap(), tree);
    assert_eq!(files_below(&dir), files);
    assert!(unpack(&path, &order, &limits(), &rig.host(), Linux).is_ok());
}

/// Catches hostkeeper choosing among instruction-set variants: a package carrying a launcher,
/// two builds of one binary for different instruction-set levels and glibc-hwcaps directories
/// of one library unpacks whole. Every file is there with its bytes, nothing else is, and the
/// seal's tree digest covers all of them.
#[test]
fn instruction_set_variants_are_unpacked_as_plain_files() {
    let variants: [(&str, u64, &[u8]); 8] = [
        ("manifest.toml", 0o644, MANIFEST.as_bytes()),
        ("bin/x", 0o755, b"#!launcher: picks a build at start\n"),
        ("bin/x-x86-64-v2", 0o755, b"\x7fELF v2 build"),
        ("bin/x-x86-64-v3", 0o755, b"\x7fELF v3 build"),
        (
            "bin/glibc-hwcaps/x86-64-v3/libk.so",
            0o644,
            b"\x7fELF libk v3",
        ),
        (
            "bin/glibc-hwcaps/x86-64-v4/libk.so",
            0o644,
            b"\x7fELF libk v4",
        ),
        ("bin/libk.so", 0o644, b"\x7fELF libk baseline"),
        ("files/c.conf", 0o644, b"c\n"),
    ];
    let mut tar = Tar::new();
    tar.dir("bin")
        .dir("bin/glibc-hwcaps")
        .dir("bin/glibc-hwcaps/x86-64-v3")
        .dir("bin/glibc-hwcaps/x86-64-v4")
        .dir("files");
    for (name, mode, bytes) in variants {
        tar.file(name, mode, bytes);
    }
    let rig = Rig::new("p-isa");
    let (path, order) = rig.archive("a.tar.zst", &zstd(&tar.end().bytes()));
    let unpacked = unpack(&path, &order, &limits(), &rig.host(), Linux).unwrap();
    let mut expected: Vec<(String, Vec<u8>)> = variants
        .iter()
        .map(|(name, _, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect();
    expected.sort();
    assert_eq!(files_below(&unpacked.dir), expected);
    assert_eq!(
        unpacked.seal.tree_digest(),
        tree_digest(&unpacked.dir).unwrap()
    );
}
