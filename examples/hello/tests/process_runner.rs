//! The real `Runner`, run over this test binary as its child: the test binary stands in for the
//! program (`systemctl`, `launchctl`), and the helper tests below are what it is asked to run.
//! No system utility is ever run.

mod support;

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

use hello::ports::{ProcessRunner, Runner};
use support::scratch;

/// The command line that runs the helper test `name` in this test binary.
fn helper(name: &str) -> Vec<String> {
    let exe = std::env::current_exe().expect("the test binary");
    [
        exe.to_str().expect("a path of text"),
        name,
        "--exact",
        "--ignored",
        "--nocapture",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn run(argv: &[String]) -> Result<i32, String> {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    ProcessRunner.run(&words)
}

/// Catches nothing itself: the child of `the_exit_code_of_the_program_is_the_result`.
#[test]
#[ignore = "the child process of the_exit_code_of_the_program_is_the_result; run only by it"]
fn child_exits_with_three() {
    std::process::exit(3);
}

/// Catches nothing itself: the child of `the_program_has_an_empty_environment`.
#[test]
#[ignore = "the child process of the_program_has_an_empty_environment; run only by it"]
#[expect(
    clippy::disallowed_methods,
    reason = "this helper counts the environment its parent gave it"
)]
fn child_exits_zero_if_its_environment_is_empty() {
    // An instrumented build sets its own `__LLVM_PROFILE_RT_*` variable in the process it starts;
    // that is not something the parent passed.
    let count = std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(b"__LLVM_PROFILE_RT_"))
        .count();
    std::process::exit(if count == 0 { 0 } else { 7 });
}

/// Catches nothing itself: the child of `the_program_output_goes_to_standard_error`.
#[test]
#[ignore = "the child process of the_program_output_goes_to_standard_error; run only by it"]
fn child_prints_on_both_streams_and_reads_its_input() {
    let _ = std::io::stdout().write_all(b"MARK-OUT\n");
    let _ = std::io::stderr().write_all(b"MARK-ERR\n");
    // With no standard input the read ends at once with nothing; with the parent's it would find
    // what the parent wrote.
    let mut input = [0u8; 1];
    let read = std::io::stdin().read(&mut input).unwrap_or(1);
    std::process::exit(if read == 0 { 0 } else { 8 });
}

/// Catches nothing itself: the parent of the child above, which runs it through `ProcessRunner`
/// and tells what came of it on its standard output.
#[test]
#[ignore = "the parent process of the_program_output_goes_to_standard_error; run only by it"]
fn runner_runs_the_noisy_child() {
    let result = run(&helper("child_prints_on_both_streams_and_reads_its_input"));
    let _ = std::io::stdout().write_all(format!("RESULT {result:?}\n").as_bytes());
}

/// Catches the exit code of the program being lost or replaced, for a success and a failure.
#[test]
fn the_exit_code_of_the_program_is_the_result() {
    assert_eq!(run(&helper("child_exits_with_three")), Ok(3));
}

/// Catches the parent's environment reaching the program: the child exits 7 if it has any.
#[test]
fn the_program_has_an_empty_environment() {
    assert_eq!(
        run(&helper("child_exits_zero_if_its_environment_is_empty")),
        Ok(0)
    );
}

/// Catches a program that is not there, or no program at all, being run or being a success.
#[test]
fn a_program_that_cannot_run_is_an_error_naming_it() {
    let dir = scratch("no-program");
    let missing = dir.path().join("not-a-program");
    let missing = missing.to_str().expect("a path of text").to_owned();
    let why = run(std::slice::from_ref(&missing)).expect_err("nothing to run");
    assert!(why.starts_with(&format!("cannot run {missing}: ")), "{why}");
    assert_eq!(ProcessRunner.run(&[]), Err("no program to run".to_owned()));
}

/// Catches the program's standard output reaching the hook's standard output (where the report
/// line is the only thing the updater may find), and the program reading the hook's input: the
/// parent process here holds an input it never closes and writes to it.
#[test]
fn the_program_output_goes_to_standard_error() {
    let exe = std::env::current_exe().expect("the test binary");
    let mut parent = Command::new(exe)
        .args([
            "runner_runs_the_noisy_child",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the helper");
    let mut input = parent.stdin.take().expect("its input");
    input.write_all(b"x").expect("write to it");
    let (mut out, mut err) = (String::new(), String::new());
    parent
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read");
    parent
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .expect("read");
    drop(input);
    parent.wait().expect("wait");

    assert!(
        out.contains("RESULT Ok(0)\n"),
        "the child found no input: {out:?}"
    );
    assert!(
        !out.contains("MARK-"),
        "nothing of the program on standard output: {out:?}"
    );
    assert!(
        err.contains("MARK-OUT\n"),
        "its output is logged on standard error: {err:?}"
    );
    assert!(err.contains("MARK-ERR\n"), "{err:?}");
}
