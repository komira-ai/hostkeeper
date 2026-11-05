//! The hook's log: what the host's step launcher kept of a hook's standard error, as lines of the
//! daemon's log under the step's unit.

use hostkeeper_daemon::hook_log::{DROPPED, hook_log_lines, log_hook_output};
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
/// error that is not blank is one line of the daemon's log, in order, under the unit. The writer
/// of those lines is run once; what it writes to standard error is not read back.
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
    log_hook_output(
        UNIT,
        &captured(ring(b"written to the daemon's standard error\n")),
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
