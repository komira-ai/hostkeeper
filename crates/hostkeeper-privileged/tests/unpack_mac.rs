#![cfg(all(target_os = "macos", feature = "privileged"))]
//! The unpack on a real Mac, as the runner's own user and as root: privileged.yml's job
//! `macos-device` runs this file twice, once without `sudo` (as the release check of a package
//! runs `hostkeeper-unpack` on a hosted runner) and once as root (as the daemon runs it on a
//! host). Each test asserts what it must find for the user it runs as, so both runs pass only
//! when the unpack is right for both.
//!
//! - The rule itself, with nothing of hostkeeper: macOS refuses to rename a directory that its
//!   caller cannot write, even to a new name in the same parent (it rewrites the directory's
//!   `..` entry), and lets root do it; a read-only file and a writable directory are renamed for
//!   either. (Linux refuses such a rename only when the parent changes.)
//! - A package whose directories are read-only, unpacked through the production port
//!   (`HostFs`) and the host's package check (`HostMacVerify`, codesign and the quarantine
//!   attribute), is published whole and sealed, its directories 0555, for either user.
//!
//! Each test works in a scratch directory of its own under the target directory, named for the
//! user it runs as, and touches nothing else of the host. Like the other device tests these are
//! not counted by the coverage gate; the unpack's own tests cover the same publish on Linux,
//! against a port that renames as macOS does.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform as PackagePlatform;
use hostkeeper_daemon::mac::HostMacVerify;
use hostkeeper_steps::Layout;
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::{HostFs, Limits, Owner, PackageFs, Platform, UnpackOrder, unpack};
use sha2::{Digest, Sha256};

/// The errno of a refused permission.
const EACCES: i32 = 13;
const SERVICE: &str = "hkci-unpack";
const VERSION: &str = "1.0.0";

/// Whether the test runs as root (privileged.yml's run with the cargo runner `sudo`).
fn root() -> bool {
    rustix::process::geteuid().is_root()
}

/// A scratch directory of the test `name`, named for the user it runs as, so that the run as
/// the runner's user and the run as root never share one.
fn scratch(name: &str) -> Scratch {
    let user = if root() { "root" } else { "user" };
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    Scratch::under(base, &format!("unpack-mac-{name}-{user}")).expect("scratch directory")
}

/// The permission bits of `path`, not following a link.
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Catches a fix built on a wrong reading of the rule: in one writable parent, a directory
/// mode 0555 is renamed for root and refused with `EACCES` for any other user, while a file
/// mode 0444 and a directory mode 0700 are renamed for either.
#[test]
fn macos_renames_a_directory_its_caller_cannot_write_only_for_root() {
    let scratch = scratch("rule");
    let parent = scratch.path();
    fs::create_dir(parent.join("dir-0555")).expect("mkdir");
    set_mode(&parent.join("dir-0555"), 0o555);
    fs::write(parent.join("file-0444"), "x").expect("write");
    set_mode(&parent.join("file-0444"), 0o444);
    fs::create_dir(parent.join("dir-0700")).expect("mkdir");
    set_mode(&parent.join("dir-0700"), 0o700);
    let rename = |from: &str| {
        fs::rename(parent.join(from), parent.join(format!("{from}-renamed")))
            .map_err(|e| e.raw_os_error())
    };
    let found = [
        ("dir-0555", rename("dir-0555")),
        ("file-0444", rename("file-0444")),
        ("dir-0700", rename("dir-0700")),
    ];
    println!("root: {}, renames: {found:?}", root());
    let read_only_dir = if root() { Ok(()) } else { Err(Some(EACCES)) };
    assert_eq!(
        found,
        [
            ("dir-0555", read_only_dir),
            ("file-0444", Ok(())),
            ("dir-0700", Ok(()))
        ]
    );
}

/// The archive of a package whose directories are all read-only: `bin/` and `bin/lib/` (0555
/// in the archive), the hook `bin/x`, a data file `bin/lib/data` and the manifest.
fn package() -> Vec<u8> {
    let manifest = format!(
        "schema = 1
name = \"{SERVICE}\"
version = \"{VERSION}\"
platform = \"macos-arm64\"
min_hostkeeper = \"0.1.0\"
[hook]
command = [\"bin/x\", \"update-hook\"]
verify_deadline_secs = 2
[hook.timeout_secs]
prepare = 5
cutover = 1
verify = 1
rollback = 1
cleanup = 1
"
    );
    let mut builder = tar::Builder::new(Vec::new());
    let entries: [(&str, u32, bool, &[u8]); 5] = [
        ("bin/", 0o555, true, b""),
        ("bin/x", 0o555, false, b"#!/bin/sh\nexit 0\n"),
        ("bin/lib/", 0o555, true, b""),
        ("bin/lib/data", 0o444, false, b"data\n"),
        ("manifest.toml", 0o444, false, manifest.as_bytes()),
    ];
    for (name, mode, dir, data) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(if dir {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, name, data)
            .unwrap_or_else(|e| panic!("cannot add {name}: {e}"));
    }
    let tar = builder.into_inner().expect("the archive in memory");
    ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest)
}

/// Catches an unpack that publishes only as root on a Mac (the release check of a package runs
/// it as the runner's user, which macOS refuses the rename of a read-only directory): a package
/// whose directories are read-only, unpacked through `HostFs` with the host's package check,
/// gives a fresh sealed version for root and for the runner's user alike, its version
/// directory and every directory in it 0555, its files as normalised, its tree as its seal
/// says, nothing left beside it; and the next unpack of the order takes it as it is.
#[test]
fn a_package_with_read_only_directories_unpacks_for_root_and_for_the_runners_user() {
    let scratch = scratch("package");
    let packages = scratch.path().join("packages");
    fs::create_dir(&packages).expect("mkdir");
    set_mode(&packages, 0o755);
    let archive = scratch.path().join("package.tar.zst");
    let bytes = package();
    fs::write(&archive, &bytes).expect("write the archive");
    let order = UnpackOrder {
        service: ServiceName::new(SERVICE).expect("a service name"),
        version: Version::new(VERSION).expect("a version"),
        platform: PackagePlatform::MacosArm64,
        package_digest: Sha256Hex::parse(&hex::encode(Sha256::digest(&bytes))).expect("a digest"),
    };
    let limits = Limits {
        max_unpacked_bytes: 1 << 20,
        max_entries: 100,
    };
    let owner = Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    };
    let host = HostFs::open(&packages, owner).expect("open the packages directory");
    let verify = HostMacVerify::system();
    let result = unpack(&archive, &order, &limits, &host, Platform::MacOs(&verify));
    println!(
        "root: {}, unpack: {:?}",
        root(),
        result.as_ref().map(|u| u.fresh)
    );
    let unpacked = result.expect("the package unpacks");
    assert!(unpacked.fresh);

    let service = packages.join(SERVICE);
    let mut names: Vec<String> = fs::read_dir(&service)
        .expect("list the service directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, [VERSION.to_owned(), format!("{VERSION}.seal")]);
    let layout = Layout::package(&service);
    layout
        .check_tree(VERSION)
        .expect("the version is as sealed");
    let dir = layout.version_path(VERSION);
    let modes: Vec<(&str, u32)> = [
        "",
        "bin",
        "bin/lib",
        "bin/lib/data",
        "bin/x",
        "manifest.toml",
    ]
    .into_iter()
    .map(|name| (name, mode(&dir.join(name))))
    .collect();
    assert_eq!(
        modes,
        [
            ("", 0o555),
            ("bin", 0o555),
            ("bin/lib", 0o555),
            ("bin/lib/data", 0o444),
            ("bin/x", 0o555),
            ("manifest.toml", 0o444)
        ]
    );
    let again = unpack(&archive, &order, &limits, &host, Platform::MacOs(&verify))
        .expect("the version is taken as it is");
    assert!(!again.fresh);
    assert_eq!(again.seal, unpacked.seal);
    // The read-only tree, removed through the port that opens each directory first, so the
    // scratch directory can go.
    host.remove_tree(Path::new(SERVICE))
        .expect("remove the service directory");
}
