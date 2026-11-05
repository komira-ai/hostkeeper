//! The hook's log: what the host's step launcher kept of a hook's standard error, as lines of the
//! daemon's log under the step's unit.

use std::io::{self, Write};
use std::process::Command;

use hostkeeper_daemon::hook_log::{DROPPED, hook_log_lines, log_hook_output, write_hook_log};
use hostkeeper_steps::{Captured, Ring};

const UNIT: &str = "updater-web-cutover-7";

fn captured(stderr: Ring) -> Captured {
    let mut stdout = Ring::new();
    stdout.push(b"not logged\n{\"phase\":\"done\"}\n");
    Captured {
        code: Some(0),
        stdout,
        stderr,
    }
}

fn ring(bytes: &[u8]) -> Ring {
    let mut ring = Ring::new();
    ring.push(bytes);
    ring
}

fn line(text: &str) -> String {
    format!("hostkeeper daemon: {UNIT}: {text}")
}

/// Catches a log that keeps blank lines, keeps a carriage return, loses a line without a final
/// line feed, refuses bytes that are not UTF-8, or logs standard output: each line of standard
/// error that is not blank is one line of the daemon's log, in order, under the unit.
#[test]
fn every_line_of_the_hooks_standard_error_is_logged_under_its_unit() {
    let stderr = ring(b"stopping web\r\n\n   \t\r\nbad byte \xff here\nno line feed at the end");
    assert_eq!(
        hook_log_lines(UNIT, &captured(stderr)),
        vec![
            line("stopping web"),
            line("bad byte \u{fffd} here"),
            line("no line feed at the end"),
        ]
    );
}

/// A writer that refuses every write.
struct Refuses;

impl Write for Refuses {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("refused"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Catches a writer that drops lines, joins them, or leaves the last one without its line feed:
/// the bytes written are the log lines, each ended by a line feed, in order; nothing for a hook
/// that wrote nothing; a refused write is the writer's error.
#[test]
fn the_hook_log_is_written_one_line_each() {
    let mut out = Vec::new();
    write_hook_log(&mut out, UNIT, &captured(ring(b"one\r\n\ntwo"))).expect("written");
    assert_eq!(
        String::from_utf8(out).expect("utf-8"),
        format!("{}\n{}\n", line("one"), line("two"))
    );

    let mut out = Vec::new();
    write_hook_log(&mut out, UNIT, &captured(Ring::new())).expect("written");
    assert!(out.is_empty());

    let refused = write_hook_log(&mut Refuses, UNIT, &captured(ring(b"x\n")));
    assert_eq!(
        refused.err().map(|e| e.to_string()),
        Some("refused".to_owned())
    );
}

/// The child of `the_hosts_step_log_writes_to_the_daemons_standard_error`: it writes one run's log
/// to this process's standard error. Run only by that test, as a process of its own, so that its
/// standard error can be read.
#[test]
#[ignore = "run only as the child process of the_hosts_step_log_writes_to_the_daemons_standard_error"]
fn child_writes_the_hook_log_to_standard_error() {
    log_hook_output(UNIT, &captured(ring(b"to the journal\n\nand on\n")));
}

/// Catches a step log of the host's launcher that writes nowhere, or somewhere other than the
/// daemon's standard error: this test binary is run again for the child test alone, and its
/// standard error is exactly the two log lines.
#[test]
fn the_hosts_step_log_writes_to_the_daemons_standard_error() {
    let output = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "child_writes_the_hook_log_to_standard_error",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("the child ran");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf-8"),
        format!("{}\n{}\n", line("to the journal"), line("and on"))
    );
}

/// Catches a log that says nothing of the bytes the launcher dropped, or that logs something for
/// a hook that wrote nothing: a ring that dropped its beginning is logged after the line that
/// says so; nothing, or only blank lines, logs nothing, whether bytes were dropped or not.
#[test]
fn a_cut_log_says_so_and_an_empty_one_logs_nothing() {
    let mut cut = Ring::with_capacity(10);
    cut.push(b"dropped part\nkept line\n");
    assert!(cut.cut());
    assert_eq!(
        hook_log_lines(UNIT, &captured(cut)),
        vec![line(DROPPED), line("kept line")]
    );

    assert_eq!(
        hook_log_lines(UNIT, &captured(Ring::new())),
        Vec::<String>::new()
    );
    assert_eq!(
        hook_log_lines(UNIT, &captured(ring(b"\n \n"))),
        Vec::<String>::new()
    );
    let mut blank_and_cut = Ring::with_capacity(3);
    blank_and_cut.push(b"lost\n \n");
    assert!(blank_and_cut.cut());
    assert_eq!(
        hook_log_lines(UNIT, &captured(blank_and_cut)),
        Vec::<String>::new()
    );
}
