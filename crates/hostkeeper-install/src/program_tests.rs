//! Tests of the hook program's resolution in a package's tree.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_testkit::Scratch;

use super::{MAX_LINKS, ProgramError, check, resolve};

/// A scratch tree under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn tree() -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), "program").expect("a scratch tree")
}

fn file(root: &Path, rel: &str, mode: u32) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the parents");
    fs::write(&path, "#!/bin/sh\n").expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
}

fn link(root: &Path, rel: &str, target: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the parents");
    symlink(target, path).expect("the link");
}

#[test]
fn a_program_with_no_link_on_its_way_is_its_own_path() {
    let dir = tree();
    file(dir.path(), "usr/bin/tierstore", 0o755);
    assert_eq!(
        check(dir.path(), "/usr/bin/tierstore"),
        Ok(PathBuf::from("usr/bin/tierstore"))
    );
}

#[test]
fn links_are_followed_inside_the_tree_absolute_and_relative() {
    let dir = tree();
    file(dir.path(), "opt/tierstore/bin/tierstore", 0o755);
    // An absolute link starts at the tree's root, never the host's.
    link(
        dir.path(),
        "usr/bin/tierstore",
        "/opt/tierstore/bin/tierstore",
    );
    // A relative link is read from its own directory.
    link(dir.path(), "usr/sbin/ts", "../bin/tierstore");
    // A link on a directory of the way, with the rest of the path walked after it.
    link(dir.path(), "usr/lib/tierstore", "/opt/tierstore");
    let real = Ok(PathBuf::from("opt/tierstore/bin/tierstore"));
    assert_eq!(check(dir.path(), "/usr/bin/tierstore"), real);
    assert_eq!(check(dir.path(), "/usr/sbin/ts"), real);
    assert_eq!(check(dir.path(), "/usr/lib/tierstore/bin/tierstore"), real);
}

#[test]
fn dot_dot_never_climbs_above_the_trees_root() {
    let dir = tree();
    file(dir.path(), "usr/bin/x", 0o755);
    link(dir.path(), "usr/lib/p/x", "../../../../../../usr/bin/x");
    assert_eq!(
        check(dir.path(), "/usr/lib/p/x"),
        Ok(PathBuf::from("usr/bin/x"))
    );
    assert_eq!(
        check(dir.path(), "/../../usr/bin/x"),
        Ok(PathBuf::from("usr/bin/x"))
    );
}

#[test]
fn each_program_that_is_not_an_executable_file_of_the_package_is_refused_by_name() {
    let dir = tree();
    let root = dir.path();
    file(root, "usr/bin/plain", 0o644);
    fs::create_dir_all(root.join("usr/lib/dir")).expect("a directory");
    file(root, "usr/bin/file", 0o755);
    link(root, "usr/bin/dangling", "/nowhere/at/all");
    link(root, "usr/bin/a", "b");
    link(root, "usr/bin/b", "a");
    let cases = [
        (
            "usr/bin/file",
            ProgramError::NotAbsolute("usr/bin/file".to_owned()),
        ),
        (
            "/usr/bin/absent",
            ProgramError::Missing("/usr/bin/absent".to_owned()),
        ),
        (
            "/usr/bin/file/x",
            ProgramError::Missing("/usr/bin/file/x".to_owned()),
        ),
        (
            "/usr/bin/dangling",
            ProgramError::Missing("/usr/bin/dangling".to_owned()),
        ),
        ("/usr/bin/a", ProgramError::Loop("/usr/bin/a".to_owned())),
        (
            "/usr/lib/dir",
            ProgramError::NotFile("/usr/lib/dir".to_owned()),
        ),
        (
            "/usr/bin/plain",
            ProgramError::NotExecutable("/usr/bin/plain".to_owned()),
        ),
    ];
    for (program, error) in cases {
        assert_eq!(check(root, program), Err(error), "{program}");
    }
    assert_eq!(
        ProgramError::Loop("/usr/bin/a".to_owned()).to_string(),
        format!("/usr/bin/a passes through more than {MAX_LINKS} links in the package")
    );
}

#[test]
fn resolve_alone_answers_a_directory_and_the_root() {
    let dir = tree();
    fs::create_dir_all(dir.path().join("usr/lib/dir")).expect("a directory");
    assert_eq!(
        resolve(dir.path(), "/usr/lib/dir"),
        Ok(PathBuf::from("usr/lib/dir"))
    );
    assert_eq!(resolve(dir.path(), "/"), Ok(PathBuf::new()));
}
