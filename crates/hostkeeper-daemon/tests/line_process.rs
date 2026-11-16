//! The real line reader ([`hostkeeper_daemon::line_process::LineProcess`]), run over this test
//! binary as its child: the test binary stands in for `pmset`, and the helper tests below are what
//! it is asked to run.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use hostkeeper_daemon::line_process::{LineProcess, LineTool};
use hostkeeper_testkit::Scratch;

const LONG: Duration = Duration::from_secs(60);

fn adapter(deadline: Duration) -> LineProcess {
    let exe = std::env::current_exe().expect("the test binary");
    LineProcess::new(&exe, deadline)
}

fn helper(name: &str) -> Vec<String> {
    [name, "--exact", "--ignored", "--nocapture"]
        .map(str::to_owned)
        .to_vec()
}

/// The lines `run_lines` hands over, and how the run ended.
fn lines_of(process: &LineProcess, args: &[String]) -> (Vec<String>, Result<Option<i32>, String>) {
    let mut lines = Vec::new();
    let end = process.run_lines(args, &mut |line| lines.push(line.to_owned()));
    (lines, end)
}

/// Catches nothing itself: the child of `each_line_is_handed_over_as_it_was_written`.
#[test]
#[ignore = "the child process of each_line_is_handed_over_as_it_was_written; run only by it"]
fn child_prints_lines_and_exits_four() {
    let _ = std::io::stdout().write_all(b"first line\r\nnot utf-8: \xff\nlast, no end");
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(b"to-stderr\n");
    std::process::exit(4);
}

/// Catches nothing itself: the child of `a_child_that_never_ends_is_killed_at_its_deadline`.
#[test]
#[ignore = "the child process of a_child_that_never_ends_is_killed_at_its_deadline; run only by it"]
fn child_never_ends() {
    loop {
        std::thread::park();
    }
}

/// Catches a line lost, kept with its line end, or dropped for bytes that are not UTF-8, a last
/// line with no end lost, standard error read as output, and the exit code lost.
#[test]
fn each_line_is_handed_over_as_it_was_written() {
    let (lines, end) = lines_of(&adapter(LONG), &helper("child_prints_lines_and_exits_four"));
    assert_eq!(end, Ok(Some(4)));
    let at = lines
        .iter()
        .position(|line| line == "first line")
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(lines[at + 1], "not utf-8: \u{fffd}");
    assert!(lines[at + 2].starts_with("last, no end"), "{lines:?}");
    assert!(
        !lines.iter().any(|line| line.contains("to-stderr")),
        "{lines:?}"
    );
}

/// Catches a child that never ends holding the probe: at its deadline (zero) it is killed, and the
/// run is an error that says so.
#[test]
fn a_child_that_never_ends_is_killed_at_its_deadline() {
    let (_, end) = lines_of(&adapter(Duration::ZERO), &helper("child_never_ends"));
    assert_eq!(end, Err("killed after 0 seconds".to_owned()));
}

/// Catches a program that cannot start being reported as a run.
#[test]
fn a_program_that_does_not_exist_is_an_error_and_hands_over_nothing() {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "line-missing")
        .expect("a scratch directory");
    let process = LineProcess::new(&dir.path().join("no-such-program"), LONG);
    let (lines, end) = lines_of(&process, &["-g".to_owned(), "log".to_owned()]);
    assert_eq!(lines, Vec::<String>::new());
    assert_eq!(
        end,
        Err("No such file or directory (os error 2)".to_owned())
    );
}
