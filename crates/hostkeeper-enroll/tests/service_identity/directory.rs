//! The identity directory on disk: ownership and modes, links that are not followed, the size
//! bound, and removal.

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_enroll::{
    FilesError, IdentityDir, IdentityFile, IdentityFiles, MAX_FILE_BYTES, Owner,
};

use crate::support::{SERVICE, World, me, scratch, wanted};

fn dir_at(root: &Path) -> IdentityDir {
    IdentityDir::owned_by(root, me())
}

fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

/// Catches a root made with another mode than `0755`, its missing parents not made, and a
/// service directory not repaired to `0750`.
#[test]
fn the_root_and_a_service_directory_are_made_and_repaired() {
    let scratch = scratch("dir-made");
    let root = scratch.path().join("var").join("lib").join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    assert_eq!(
        std::fs::metadata(&root).expect("stat").mode() & 0o7777,
        0o755
    );
    let service = root.join(SERVICE);
    assert_eq!(
        std::fs::metadata(&service).expect("stat").mode() & 0o7777,
        0o750
    );
    std::fs::set_permissions(&service, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    dir.prepare(SERVICE, me()).expect("prepare again");
    assert_eq!(
        std::fs::metadata(&service).expect("stat").mode() & 0o7777,
        0o750
    );
}

/// Catches an installed file followed through a link: a link where `cert.pem` should be is
/// refused by name, and what it points at is not read.
#[test]
fn a_file_that_is_a_symbolic_link_is_refused_and_not_followed() {
    let scratch = scratch("dir-symlink");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    let target = scratch.path().join("elsewhere");
    std::fs::write(&target, b"secret").expect("write");
    let link = root.join(SERVICE).join("cert.pem");
    symlink(&target, &link).expect("symlink");
    let error = dir
        .read(SERVICE, IdentityFile::Cert, me())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is a symbolic link, which is not followed",
            link.display()
        )
    );
    assert_eq!(std::fs::read(&target).expect("read"), b"secret");
}

/// Catches a file owned by another user being read as the account's, or being an error that
/// stops the service: the owner the reader expects is not the file's, so the file is absent, and
/// a link in the same place is still refused.
#[test]
fn a_file_owned_by_another_user_is_absent() {
    let scratch = scratch("dir-owner");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.write(SERVICE, IdentityFile::Key, me(), b"key")
        .expect("write");
    let other = Owner {
        uid: me().uid + 1,
        gid: me().gid,
    };
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Key, other).expect("read"),
        None
    );
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Key, me()).expect("read"),
        Some(b"key".to_vec())
    );
}

/// Catches the services of the root being listed wrongly: each entry with a service name is
/// listed, in order, whatever it is (a directory, a file, a link); an entry whose name is no
/// service name is not, and an absent root has none.
#[test]
fn the_services_of_the_root_are_the_entries_with_a_service_name() {
    let scratch = scratch("dir-services");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    assert_eq!(dir.services().expect("list"), Vec::<String>::new());
    dir.prepare("beta", me()).expect("prepare");
    dir.prepare("alpha", me()).expect("prepare");
    std::fs::write(root.join("a-file"), b"x").expect("write");
    symlink(scratch.path(), root.join("a-link")).expect("symlink");
    std::fs::write(root.join("Not A Service"), b"x").expect("write");
    assert_eq!(
        dir.services().expect("list"),
        ["a-file", "a-link", "alpha", "beta"]
    );
}

/// Catches the size bound: a file of exactly the bound is read, one byte more is refused.
#[test]
fn a_file_larger_than_the_bound_is_refused() {
    let scratch = scratch("dir-size");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    let size = usize::try_from(MAX_FILE_BYTES).expect("a size");
    dir.write(SERVICE, IdentityFile::Cert, me(), &vec![b'a'; size])
        .expect("write");
    let read = dir.read(SERVICE, IdentityFile::Cert, me()).expect("read");
    assert_eq!(read.map(|bytes| bytes.len()), Some(size));
    dir.write(SERVICE, IdentityFile::Cert, me(), &vec![b'a'; size + 1])
        .expect("write");
    let error = dir
        .read(SERVICE, IdentityFile::Cert, me())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            root.join(SERVICE).join("cert.pem").display()
        )
    );
}

/// Catches a directory (or anything but a regular file) read as a file.
#[test]
fn a_file_that_is_a_directory_is_refused() {
    let scratch = scratch("dir-not-regular");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    let path = root.join(SERVICE).join("ca.pem");
    std::fs::create_dir(&path).expect("mkdir");
    let error = dir
        .read(SERVICE, IdentityFile::Ca, me())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("{} is not a regular file", path.display())
    );
}

/// Catches a keeper that follows a link where a service's directory should be, or the root:
/// each is refused as not a directory, read or write, and what the link points to is not
/// touched.
#[test]
fn a_directory_that_is_a_symbolic_link_is_refused() {
    let scratch = scratch("dir-link-dir");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare("other", me()).expect("prepare");
    symlink(root.join("other"), root.join(SERVICE)).expect("symlink");
    let expected = format!(
        "{} is not a directory (a link or a file is not followed)",
        root.join(SERVICE).display()
    );
    let read = dir
        .read(SERVICE, IdentityFile::Key, me())
        .expect_err("refused");
    assert_eq!(read.to_string(), expected);
    let write = dir
        .write(SERVICE, IdentityFile::Key, me(), b"k")
        .expect_err("refused");
    assert_eq!(write.to_string(), expected);
    assert_eq!(names(&root.join("other")), Vec::<String>::new());

    let moved = scratch.path().join("moved");
    std::fs::rename(&root, &moved).expect("rename");
    symlink(&moved, &root).expect("symlink");
    let error = dir
        .read(SERVICE, IdentityFile::Key, me())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is not a directory (a link or a file is not followed)",
            root.display()
        )
    );
}

/// Catches a root that others can write, or that another user owns, being used.
#[test]
fn a_root_that_others_can_write_or_another_user_owns_is_refused() {
    let scratch = scratch("dir-root-checks");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o757)).expect("chmod");
    let error = dir
        .read(SERVICE, IdentityFile::Key, me())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("{} is writable by group or others", root.display())
    );
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let stranger = IdentityDir::owned_by(
        &root,
        Owner {
            uid: me().uid + 1,
            gid: me().gid,
        },
    );
    let error = stranger.prepare(SERVICE, me()).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is owned by user {}, not by {}",
            root.display(),
            me().uid,
            me().uid + 1
        )
    );
}

/// Catches a name that is not a service name reaching the filesystem: no operation takes it.
#[test]
fn a_name_that_is_not_a_service_name_is_refused_by_every_operation() {
    let scratch = scratch("dir-names");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    let bad = "../escape";
    let expected = "\"../escape\" is not a service name";
    assert_eq!(
        dir.prepare(bad, me()).expect_err("refused").to_string(),
        expected
    );
    assert_eq!(
        dir.read(bad, IdentityFile::Key, me())
            .expect_err("refused")
            .to_string(),
        expected
    );
    assert_eq!(
        dir.write(bad, IdentityFile::Key, me(), b"k")
            .expect_err("refused")
            .to_string(),
        expected
    );
    assert_eq!(dir.remove(bad).expect_err("refused").to_string(), expected);
    assert!(!root.exists());
}

/// Catches a write that leaves a temporary file, or fails on one a crash left, and one that
/// replaces a file in place.
#[test]
fn a_write_replaces_the_file_and_leaves_no_temporary_one() {
    let scratch = scratch("dir-write");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    let service = root.join(SERVICE);
    std::fs::write(service.join(".cert.pem.tmp"), b"left by a crash").expect("write");
    dir.write(SERVICE, IdentityFile::Cert, me(), b"one")
        .expect("write");
    dir.write(SERVICE, IdentityFile::Cert, me(), b"two")
        .expect("write");
    assert_eq!(names(&service), ["cert.pem"]);
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Cert, me()).expect("read"),
        Some(b"two".to_vec())
    );
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Key, me()).expect("read"),
        None
    );
}

/// Catches reading from a directory or a service that is not there being an error.
#[test]
fn reading_what_is_absent_finds_nothing() {
    let scratch = scratch("dir-absent");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Key, me()).expect("read"),
        None
    );
    dir.prepare("other", me()).expect("prepare");
    assert_eq!(
        dir.read(SERVICE, IdentityFile::Key, me()).expect("read"),
        None
    );
}

/// Catches an uninstall leaving the identity behind, or failing for want of it: the service's
/// directory goes with its files and what a crash left, an absent one is fine twice over, and a
/// link in its place is removed without touching what it points at.
#[test]
fn removing_a_service_removes_its_directory_and_follows_no_link() {
    let scratch = scratch("dir-remove");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.remove(SERVICE).expect("an absent root is fine");
    dir.write(SERVICE, IdentityFile::Key, me(), b"k")
        .expect("write");
    dir.write(SERVICE, IdentityFile::Cert, me(), b"c")
        .expect("write");
    dir.write("keep", IdentityFile::Key, me(), b"k")
        .expect("write");
    std::fs::write(root.join(SERVICE).join(".cert.pem.tmp"), b"t").expect("write");
    dir.remove(SERVICE).expect("remove");
    assert_eq!(names(&root), ["keep"]);
    dir.remove(SERVICE).expect("an absent service is fine");

    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).expect("mkdir");
    std::fs::write(outside.join("file"), b"x").expect("write");
    symlink(&outside, root.join("linked")).expect("symlink");
    dir.remove("linked").expect("remove a link");
    assert_eq!(names(&root), ["keep"]);
    assert_eq!(names(&outside), ["file"]);
}

/// Catches a directory inside a service's directory being removed by following or recursing: it is
/// an error, and what is there stays.
#[test]
fn a_directory_inside_a_service_directory_stops_its_removal() {
    let scratch = scratch("dir-remove-nested");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    let nested = root.join(SERVICE).join("nested");
    std::fs::create_dir(&nested).expect("mkdir");
    let error = dir.remove(SERVICE).expect_err("refused");
    assert!(matches!(error, FilesError::Io { .. }), "{error}");
    assert!(nested.is_dir());
}

/// Catches a service's directory that is a file being removed as a directory would be.
#[test]
fn removing_a_service_whose_directory_is_a_file_removes_the_file() {
    let scratch = scratch("dir-remove-file");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare("other", me()).expect("prepare");
    std::fs::write(root.join(SERVICE), b"a file").expect("write");
    dir.remove(SERVICE).expect("remove");
    assert_eq!(names(&root), ["other"]);
}

/// Catches the keeper using the directory as the account's: the files of a service are the
/// account's, the directory root's, as the keeper builds them (the same check as the
/// installation test, here from a second pass that repairs a changed mode).
#[test]
fn a_pass_repairs_a_directory_whose_mode_was_changed() {
    let mut world = World::new("dir-repair");
    world.keeper.step(&[wanted(SERVICE)]);
    let service = world.root().join(SERVICE);
    std::fs::set_permissions(&service, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        std::fs::metadata(&service).expect("stat").mode() & 0o7777,
        0o750
    );
}

/// Catches a leftover temporary name that is a directory, or a root under a file, being written
/// through: each is an error naming the path, and nothing is installed.
#[test]
fn a_write_that_cannot_make_its_place_is_an_error() {
    let scratch = scratch("dir-write-fails");
    let root = scratch.path().join("identity");
    let dir = dir_at(&root);
    dir.prepare(SERVICE, me()).expect("prepare");
    std::fs::create_dir(root.join(SERVICE).join(".cert.pem.tmp")).expect("mkdir");
    let error = dir
        .write(SERVICE, IdentityFile::Cert, me(), b"c")
        .expect_err("refused");
    assert!(matches!(error, FilesError::Io { .. }), "{error}");

    let file = scratch.path().join("a-file");
    std::fs::write(&file, b"x").expect("write");
    let under = dir_at(&file.join("identity"));
    let error = under.prepare(SERVICE, me()).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is not a directory (a link or a file is not followed)",
            file.join("identity").display()
        )
    );
}
