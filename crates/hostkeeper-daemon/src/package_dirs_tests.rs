//! The tests of `package_dirs` that reach what the start cannot: a directory that cannot be
//! listed once its mode is set (the daemon gives every directory it walks a mode its owner can
//! list, so only a directory gone, or another user's, fails there).

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// Catches a directory that cannot be listed passing unsaid, or stopping the walk: its listing
/// fails, it is kept as a failure with why, and it has no entries; a directory that can be listed
/// gives its entries in name order.
#[test]
fn a_directory_that_cannot_be_listed_is_a_failure_with_no_entries() {
    let scratch = scratch("package-dirs-list");
    let gone = scratch.path().join("gone");
    let mut done = Set::default();

    assert_eq!(subdirs(&gone, &mut done), Vec::<PathBuf>::new());
    let failed: Vec<(&Path, io::ErrorKind)> = done
        .failed
        .iter()
        .map(|(path, e)| (path.as_path(), e.kind()))
        .collect();
    assert_eq!(failed, [(gone.as_path(), io::ErrorKind::NotFound)]);

    for name in ["b", "a"] {
        fs::create_dir(scratch.path().join(name)).expect("a directory");
    }
    let mut done = Set::default();
    assert_eq!(
        subdirs(scratch.path(), &mut done),
        [scratch.path().join("a"), scratch.path().join("b")]
    );
    assert!(done.failed.is_empty());
}

/// Catches an entry that changed between the listing and its look passing as a failure, one that
/// cannot be looked at passing unsaid, or a file in a service directory left readable by other
/// users: an entry gone since its service was listed and a link are left as they are with nothing
/// said, a file its group or others can read (a seal, 0644) is closed to its owner (0600) and one
/// already closed is left, and a version path below a file is a failure with why; a directory
/// that is not there is not given a mode and is no failure.
#[test]
fn an_entry_gone_or_below_a_file_is_left_alone_or_a_failure() {
    let scratch = scratch("package-dirs-entries");
    let service = scratch.path().join("svc");
    fs::create_dir(&service).expect("a service");
    let file = service.join("1.0.0.seal");
    fs::write(&file, b"{}").expect("a seal");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("its mode");
    std::os::unix::fs::symlink("1.0.0", service.join("current")).expect("current");

    let mut done = Set::default();
    for entry in ["gone", "current", "1.0.0.seal"] {
        set_version(&service, &service.join(entry), &mut done);
    }
    assert_eq!(
        done.set,
        [(file.clone(), 0o600)],
        "the seal written 0644 is closed"
    );
    assert!(done.failed.is_empty(), "{done:?}");
    let mut done = Set::default();
    set_version(&service, &file, &mut done);
    assert!(
        done.set.is_empty(),
        "a closed file is left as it is: {done:?}"
    );

    set_version(&service, &file.join("below"), &mut done);
    let failed: Vec<(&Path, io::ErrorKind)> = done
        .failed
        .iter()
        .map(|(path, e)| (path.as_path(), e.kind()))
        .collect();
    assert_eq!(
        failed,
        [(file.join("below").as_path(), io::ErrorKind::NotADirectory)]
    );

    let mut done = Set::default();
    assert!(!give(&service.join("gone"), SERVICE_MODE, &mut done));
    assert!(!close(&service.join("gone"), &mut done));
    assert!(done.set.is_empty() && done.failed.is_empty(), "{done:?}");
}
