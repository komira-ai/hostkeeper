//! The state directories the daemon makes at start, so that a Mac (no tmpfiles) boots: the
//! packages directory (0755) and the hooks directory (0700).

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_daemon::layout::ensure_state_dirs;
use hostkeeper_daemon::start::{Settings, StartError};
use support::Scratch;

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).expect("metadata").mode() & 0o7777
}

fn settings(root: &Path) -> Settings {
    Settings {
        config: root.join("hostkeeper.toml"),
        state_dir: root.join("var/lib/updater/state"),
        packages_dir: root.join("var/lib/hostkeeper/packages"),
        facts_file: root.join("var/lib/hostkeeper/facts.json"),
        run_dir: root.join("run"),
        trusted_uid: 0,
        package_lock_wait_secs: hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    }
}

/// Catches a daemon that reads `--packages-dir` before making it: on an empty root, each
/// directory tmpfiles makes is there after the call, with its mode.
#[test]
fn an_empty_root_gets_the_packages_and_hooks_directories_with_their_modes() {
    let scratch = Scratch::new("state-dirs-empty");
    let settings = settings(scratch.path());
    ensure_state_dirs(&settings).expect("make them");
    assert_eq!(mode(&settings.packages_dir), 0o755);
    let hooks = scratch.path().join("var/lib/updater/hooks");
    assert_eq!(mode(&hooks), 0o700);
    assert!(hooks.is_dir());
}

/// Catches a start that goes on past a packages path that is a file, or a link (to a directory
/// even): it stops, naming the path.
#[test]
fn a_file_or_a_link_where_a_directory_belongs_stops_the_start_naming_the_path() {
    let scratch = Scratch::new("state-dirs-refused");
    let settings = settings(scratch.path());
    let parent = settings.packages_dir.parent().expect("parent");
    fs::create_dir_all(parent).expect("parent");

    fs::write(&settings.packages_dir, b"x").expect("a file");
    let error = ensure_state_dirs(&settings).expect_err("a file");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == settings.packages_dir),
        "{error}"
    );
    assert!(error.to_string().contains("is not a directory"), "{error}");

    fs::remove_file(&settings.packages_dir).expect("remove");
    let target = scratch.path().join("elsewhere");
    fs::create_dir(&target).expect("target");
    symlink(&target, &settings.packages_dir).expect("a link");
    let error = ensure_state_dirs(&settings).expect_err("a link");
    assert!(error.to_string().contains("is not a directory"), "{error}");
}

/// Catches a start that re-modes or empties what is there: existing directories keep their
/// modes and contents.
#[test]
fn existing_directories_keep_their_modes_and_contents() {
    let scratch = Scratch::new("state-dirs-existing");
    let settings = settings(scratch.path());
    let hooks = scratch.path().join("var/lib/updater/hooks");
    for (dir, mode) in [(&settings.packages_dir, 0o750), (&hooks, 0o711)] {
        fs::create_dir_all(dir).expect("dir");
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("mode");
        fs::write(dir.join("keep"), b"kept").expect("content");
    }
    ensure_state_dirs(&settings).expect("fine");
    assert_eq!(mode(&settings.packages_dir), 0o750);
    assert_eq!(mode(&hooks), 0o711);
    for dir in [&settings.packages_dir, &hooks] {
        assert_eq!(fs::read(dir.join("keep")).expect("kept"), b"kept");
    }
}

/// Catches a hooks path that is not checked as the packages path is: a file, then a link, there
/// stops the start naming the hooks path, and the packages directory before it is still made.
#[test]
fn a_file_or_a_link_at_the_hooks_path_stops_the_start_naming_it() {
    let scratch = Scratch::new("state-dirs-hooks-refused");
    let settings = settings(scratch.path());
    let hooks = scratch.path().join("var/lib/updater/hooks");
    fs::create_dir_all(hooks.parent().expect("parent")).expect("parent");

    fs::write(&hooks, b"x").expect("a file");
    let error = ensure_state_dirs(&settings).expect_err("a file");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == hooks),
        "{error}"
    );
    assert!(settings.packages_dir.is_dir());

    fs::remove_file(&hooks).expect("remove");
    symlink(scratch.path(), &hooks).expect("a link");
    let error = ensure_state_dirs(&settings).expect_err("a link");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == hooks),
        "{error}"
    );
}

/// Catches an unmakeable path going unreported: a file where a parent directory belongs makes
/// the creation fail, naming the packages path.
#[test]
fn a_file_where_a_parent_directory_belongs_stops_the_start() {
    let scratch = Scratch::new("state-dirs-parent-file");
    let settings = settings(scratch.path());
    fs::create_dir_all(scratch.path().join("var/lib")).expect("parent");
    fs::write(scratch.path().join("var/lib/hostkeeper"), b"x").expect("a file");
    let error = ensure_state_dirs(&settings).expect_err("a file parent");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == settings.packages_dir),
        "{error}"
    );
}

/// Catches a start that leaves the daemon's run directory (`/var/run/hostkeeper` on a Mac, where
/// the step jobs live) to the umask once no admin socket makes it: an empty root gets it mode
/// 0700, one that is there with mode 0755 is set 0700 and keeps its contents, and a file there
/// stops the start naming the path.
#[test]
fn the_run_directory_is_made_or_set_0700_and_a_file_there_stops_the_start() {
    use hostkeeper_daemon::layout::RUN_SUBDIR;
    let scratch = Scratch::new("state-dirs-run");
    let settings = settings(scratch.path());
    let run = settings.run_dir.join(RUN_SUBDIR);
    ensure_state_dirs(&settings).expect("make them");
    assert_eq!(mode(&run), 0o700);

    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("open it");
    fs::create_dir(run.join("steps")).expect("a job directory");
    ensure_state_dirs(&settings).expect("again");
    assert_eq!(mode(&run), 0o700);
    assert!(run.join("steps").is_dir());

    fs::remove_dir(run.join("steps")).expect("remove");
    fs::remove_dir(&run).expect("remove");
    fs::write(&run, b"x").expect("a file");
    let error = ensure_state_dirs(&settings).expect_err("a file");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == run),
        "{error}"
    );
    assert!(error.to_string().contains("is not a directory"), "{error}");
}
