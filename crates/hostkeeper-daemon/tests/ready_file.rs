//! The readiness notice as a file (`--ready-file`, `hostkeeper_daemon::notify::ReadyFile`): what
//! the init script of the Alpine package waits for after an OpenRC start, having removed it
//! before. The daemon writes it, with its pid, once its boot work is over, and removes nothing.

mod support;

use std::ffi::OsString;
use std::fs;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::{self, Cli, Command, DaemonFlags};
use hostkeeper_daemon::start;
use hostkeeper_daemon::{ReadyFile, ReadyNotifier};
use support::{OWN, Rig};

fn daemon_flags(args: &[&str]) -> Result<DaemonFlags, clap::Error> {
    let all = ["hostkeeper", "daemon"].iter().chain(args);
    let Command::Daemon(flags) = Cli::try_parse_from(all)?.command else {
        panic!("`daemon` parses as the daemon verb");
    };
    Ok(flags)
}

/// Catches a ready file written half, or with what is not this process: the notice writes the
/// pid of this process and a line feed, over a file that was there, with no temporary file left
/// beside it; making the notifier changes nothing on disk.
#[test]
fn the_ready_file_is_written_with_the_pid_when_ready() {
    let rig = Rig::new("ready-file-write");
    let path = rig.scratch.path().join("ready");
    fs::write(&path, "1\n").expect("a file of an earlier run");
    let notice = ReadyFile::new(&path);
    assert_eq!(notice.path(), path);
    assert_eq!(fs::read_to_string(&path).expect("the file"), "1\n");
    notice.ready().expect("the notice");
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("{}\n", std::process::id())
    );
    assert!(!rig.scratch.path().join("ready.tmp").exists());
}

/// Catches a notice that cannot be written read as done: a file in a directory that is gone, and
/// one whose place is a directory, are each an error.
#[test]
fn a_ready_file_that_cannot_be_written_is_an_error() {
    let rig = Rig::new("ready-file-errors");
    let gone = rig.scratch.path().join("gone").join("ready");
    assert!(ReadyFile::new(&gone).ready().is_err());
    let dir = rig.scratch.path().join("a-dir");
    fs::create_dir(&dir).expect("a directory");
    assert!(ReadyFile::new(&dir).ready().is_err());
    assert!(dir.is_dir(), "the directory is left as it is");
}

/// Catches a start that is told ready before its boot work (or never): the start of the daemon
/// with the file's notifier writes the file; the notifier made from the flags writes nothing
/// before.
#[test]
fn the_start_writes_the_ready_file_once_the_boot_work_is_over() {
    let rig = Rig::new("ready-file-start");
    let path = rig.scratch.path().join("ready");
    let flags = daemon_flags(&["--ready-file", path.to_str().expect("UTF-8")]).expect("parsed");
    let notifier = cli::notifier(&flags, Some(OsString::from("/nowhere.sock"))).expect("made");
    assert!(!path.exists(), "nothing is written before the start");
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, notifier.as_ref())
        .expect("started");
    drop(started);
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("{}\n", std::process::id())
    );
}

/// Catches the flags read wrong: `--ready-file` is the file, no flag is none, and the two notices
/// together are refused as a conflict of arguments.
#[test]
fn the_ready_file_flag_excludes_the_systemd_notice() {
    let flags = daemon_flags(&["--ready-file", "/run/r"]).expect("parsed");
    assert_eq!(
        flags.ready_file.as_deref(),
        Some(std::path::Path::new("/run/r"))
    );
    assert_eq!(daemon_flags(&[]).expect("parsed").ready_file, None);
    let both = daemon_flags(&["--ready-file", "/run/r", "--notify-systemd"])
        .expect_err("the two notices exclude each other");
    assert_eq!(both.kind(), ErrorKind::ArgumentConflict);
}
