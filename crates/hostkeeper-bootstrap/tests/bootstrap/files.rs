//! A config is written only when it is absent.

use std::fs;
use std::os::unix::fs::symlink;

use hostkeeper_bootstrap::files::{FileError, Written, write_if_absent, write_if_absent_with};

use crate::support::{exists, mode, read, scratch};

/// An absent file is made whole with the given mode, in a directory made for it, and no temporary
/// file is left.
#[test]
fn an_absent_file_is_written_with_its_mode() {
    let dir = scratch("files-new");
    let path = dir.path().join("etc/hostkeeper/hostkeeper.toml");
    assert_eq!(
        write_if_absent(&path, "text\n", 0o600).expect("written"),
        Written::Made
    );
    assert_eq!(read(&path), "text\n");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("dir")), 0o755);
    let left: Vec<_> = fs::read_dir(path.parent().expect("dir"))
        .expect("dir")
        .collect();
    assert_eq!(left.len(), 1, "no temporary file stays");
}

/// A file that is there is kept byte for byte: the trust root never changes on a reinstall.
#[test]
fn an_existing_file_is_kept_unchanged() {
    let dir = scratch("files-kept");
    let path = dir.path().join("hostkeeper.toml");
    fs::write(&path, "the operator's own\n").expect("write");
    assert_eq!(
        write_if_absent(&path, "other\n", 0o600).expect("kept"),
        Written::Kept
    );
    assert_eq!(read(&path), "the operator's own\n");
}

/// A link or a directory at the path is refused and left as it is, never written through.
#[test]
fn a_link_or_a_directory_is_refused_and_left() {
    let dir = scratch("files-link");
    let target = dir.path().join("target");
    fs::write(&target, "elsewhere\n").expect("write");
    let link = dir.path().join("link.toml");
    symlink(&target, &link).expect("link");
    let error = write_if_absent(&link, "x\n", 0o600).expect_err("a link");
    assert!(matches!(error, FileError::NotAFile { .. }), "{error}");
    assert_eq!(read(&target), "elsewhere\n");
    let folder = dir.path().join("folder.toml");
    fs::create_dir(&folder).expect("dir");
    assert!(matches!(
        write_if_absent(&folder, "x\n", 0o600),
        Err(FileError::NotAFile { .. })
    ));
    assert!(exists(&folder));
}

/// A directory that cannot be made, a path that cannot be inspected and a file that cannot be
/// made are errors that name the path.
#[test]
fn io_failures_name_the_path() {
    let dir = scratch("files-io");
    let file = dir.path().join("a-file");
    fs::write(&file, "x").expect("write");
    let under_file = file.join("sub/x.toml");
    let error = write_if_absent(&under_file, "x\n", 0o600).expect_err("not a directory");
    assert!(matches!(error, FileError::Io { .. }), "{error}");
    assert!(error.to_string().contains("a-file"), "{error}");
    // A name so long that the temporary file's name (a few bytes longer) is refused.
    let long = dir.path().join("x".repeat(250));
    let error = write_if_absent(&long, "x\n", 0o600).expect_err("a name too long");
    assert!(matches!(error, FileError::Io { .. }), "{error}");
    assert!(!exists(&long));
}

/// A file that appears between the check and the link is kept, not replaced: the temporary file
/// is made first, so a stand-in for the racing writer is a directory in the temporary file's own
/// name, which fails the make and leaves what is at the path alone.
#[test]
fn a_blocked_temporary_file_is_an_error_and_nothing_is_written() {
    let dir = scratch("files-blocked");
    let path = dir.path().join("hostkeeper.toml");
    let blocker = dir
        .path()
        .join(format!(".hostkeeper.toml.bootstrap-{}", std::process::id()));
    fs::create_dir(&blocker).expect("dir");
    assert!(matches!(
        write_if_absent(&path, "x\n", 0o600),
        Err(FileError::Io { .. })
    ));
    assert!(!exists(&path));
}

/// A file that appears after the check and before the link is kept, not replaced, and the
/// temporary file is removed.
#[test]
fn a_file_that_appears_meanwhile_is_kept() {
    let dir = scratch("files-race");
    let path = dir.path().join("hostkeeper.toml");
    let racing = path.clone();
    let written = write_if_absent_with(&path, "ours\n", 0o600, &|| {
        fs::write(&racing, "theirs\n").expect("write");
    })
    .expect("kept");
    assert_eq!(written, Written::Kept);
    assert_eq!(read(&path), "theirs\n");
    assert_eq!(
        fs::read_dir(dir.path()).expect("dir").count(),
        1,
        "no temporary file stays"
    );
}

/// What appears meanwhile is judged as a file at the path always is: a directory is refused.
#[test]
fn a_directory_that_appears_meanwhile_is_refused() {
    let dir = scratch("files-race-dir");
    let path = dir.path().join("hostkeeper.toml");
    let racing = path.clone();
    let error = write_if_absent_with(&path, "ours\n", 0o600, &|| {
        fs::create_dir(&racing).expect("dir");
    })
    .expect_err("refused");
    assert!(matches!(error, FileError::NotAFile { .. }), "{error}");
}

/// A link that fails for any other reason than a file being there is an error naming the path,
/// and nothing is written (here the temporary file is taken away before the link).
#[test]
fn a_failed_link_is_an_error_and_leaves_nothing() {
    let dir = scratch("files-link-fails");
    let path = dir.path().join("hostkeeper.toml");
    let base = dir.path().to_path_buf();
    let error = write_if_absent_with(&path, "ours\n", 0o600, &|| {
        for entry in fs::read_dir(&base).expect("dir") {
            fs::remove_file(entry.expect("entry").path()).expect("remove");
        }
    })
    .expect_err("no temporary file to link");
    assert!(matches!(error, FileError::Io { .. }), "{error}");
    assert!(!exists(&path));
}

/// A file that is there but writable by its group or others is not trusted, and is left.
#[test]
fn a_kept_file_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("files-writable");
    let path = dir.path().join("hostkeeper.toml");
    fs::write(&path, "loose\n").expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("mode");
    let error = write_if_absent(&path, "x\n", 0o600).expect_err("loose");
    assert!(matches!(error, FileError::Writable { .. }), "{error}");
    assert_eq!(read(&path), "loose\n");
}
