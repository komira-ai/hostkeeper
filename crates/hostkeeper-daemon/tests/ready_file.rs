//! The readiness notice as a file (`--ready-file`, `hostkeeper_daemon::notify::ReadyFile`): what
//! the init script of the Alpine package waits for after an OpenRC start. The file is removed when
//! the daemon starts and written, with the daemon's pid, once its boot work is over.

mod support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use clap::Parser;
use hostkeeper_daemon::cli::{self, Cli, Command, DaemonFlags};
use hostkeeper_daemon::start::{self, StartError};
use hostkeeper_daemon::{ReadyFile, ReadyNotifier};
use support::{OWN, Rig};

fn daemon_flags(args: &[&str]) -> Result<DaemonFlags, clap::Error> {
    let all = ["hostkeeper", "daemon"].iter().chain(args);
    match Cli::try_parse_from(all)?.command {
        Command::Daemon(flags) => Ok(flags),
        _ => panic!("`hostkeeper daemon` parses as the daemon command"),
    }
}

/// Catches a ready file that says an earlier process was ready (a start that returns before
/// this daemon's boot pass), or one written half: the file of an earlier run is removed when the
/// notifier is made, and the notice writes the pid of this process and a line feed, with no
/// temporary file left beside it.
#[test]
fn the_ready_file_is_removed_at_the_start_and_written_with_the_pid_when_ready() {
    let rig = Rig::new("ready-file-write");
    let path = rig.scratch.path().join("ready");
    fs::write(&path, "1\n").expect("the file of an earlier run");
    let notice = ReadyFile::new(&path).expect("the notifier");
    assert_eq!(notice.path(), path);
    assert!(!path.exists(), "the earlier file is gone");
    notice.ready().expect("the notice");
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("{}\n", std::process::id())
    );
    assert!(!rig.scratch.path().join("ready.tmp").exists());
    assert!(ReadyFile::new(&path).is_ok(), "removed again");
    assert!(ReadyFile::new(&path).is_ok(), "a missing file is fine");
}

/// Catches a notice that cannot be written or a stale file that cannot be removed read as done:
/// a file in a directory that is gone is not written, and a directory in the file's place is not
/// removed, each an error.
#[test]
fn a_ready_file_that_cannot_be_written_or_removed_is_an_error() {
    let rig = Rig::new("ready-file-errors");
    let gone = rig.scratch.path().join("gone").join("ready");
    let notice = ReadyFile::new(&gone).expect("nothing to remove");
    assert!(notice.ready().is_err());

    let dir = rig.scratch.path().join("a-dir");
    fs::create_dir(&dir).expect("a directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    assert!(ReadyFile::new(&dir).is_err());
}

/// Catches a start that is told ready before its boot work (or never): the start of the daemon
/// with the file's notifier writes the file, and the file is not there before it.
#[test]
fn the_start_writes_the_ready_file_once_the_boot_work_is_over() {
    let rig = Rig::new("ready-file-start");
    let path = rig.scratch.path().join("ready");
    fs::write(&path, "1\n").expect("the file of an earlier run");
    let flags = daemon_flags(&["--ready-file", path.to_str().expect("UTF-8")]).expect("parsed");
    let notifier = cli::notifier(&flags, None).expect("the notifier");
    assert!(!path.exists(), "removed when the notifier is made");
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, notifier.as_ref())
        .expect("started");
    drop(started);
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("{}\n", std::process::id())
    );
}

/// Catches the flags read wrong: `--ready-file` gives the file's notifier, whatever the notify
/// socket is; a stale file that cannot be removed refuses the start naming it; and the two
/// notices together are a usage error.
#[test]
fn the_ready_file_flag_picks_its_notifier_and_excludes_the_systemd_notice() {
    let rig = Rig::new("ready-file-flags");
    let dir = rig.scratch.path().join("in-the-way");
    fs::create_dir(&dir).expect("a directory where the file goes");
    let flags = daemon_flags(&["--ready-file", dir.to_str().expect("UTF-8")]).expect("parsed");
    assert_eq!(flags.ready_file.as_deref(), Some(dir.as_path()));
    match cli::notifier(&flags, Some(OsString::from("/nowhere.sock"))) {
        Err(StartError::Io { path, .. }) => assert_eq!(path, dir),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a file that cannot be removed is refused"),
    }
    assert_eq!(daemon_flags(&[]).expect("parsed").ready_file, None);
    let both = daemon_flags(&["--ready-file", "/run/r", "--notify-systemd"]);
    assert!(both.is_err(), "the two notices exclude each other");
}
