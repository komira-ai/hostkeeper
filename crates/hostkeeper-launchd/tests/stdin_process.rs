//! The runner of a program with input, run over this test binary as its child: the test binary
//! stands in for the program, and the helper tests below are what it is asked to run. A helper
//! writes what it saw to its standard error, which the test harness leaves alone (it writes its
//! own lines to standard output), so the parent compares that text whole.

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::launchctl::{LaunchctlOutput, OUTPUT_MAX};
use hostkeeper_launchd::stdin_process::run_ok;
use hostkeeper_launchd::{FakeStdinRun, StdinCall, StdinError, StdinProcess, StdinRun};
use hostkeeper_testkit::Scratch;

const LONG: Duration = Duration::from_secs(60);

/// What the tests give as input: a password with bytes a shell, an argument list or a log line
/// would treat specially.
const INPUT: &[u8] = b"pa ss=word\"$x\\\x01\n";

const STEP: &str = "softwareupdate";

fn runner(deadline: Duration) -> StdinProcess {
    let exe = std::env::current_exe().expect("the test binary");
    StdinProcess::new(&exe, deadline)
}

/// The arguments that run the helper test `name` in this binary.
fn helper(name: &str) -> Vec<String> {
    [name, "--exact", "--ignored", "--nocapture"]
        .map(str::to_owned)
        .to_vec()
}

/// The environment this process has, without what an instrumented build sets by itself in each
/// process it starts (`__LLVM_PROFILE_RT_*`), which no parent passed.
#[expect(
    clippy::disallowed_methods,
    reason = "the helper reports the environment its parent gave it"
)]
fn own_environment() -> Vec<(Vec<u8>, Vec<u8>)> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(b"__LLVM_PROFILE_RT_"))
        .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .collect()
}

/// The record a helper writes of what it saw: one `arg:` line per argument after the program,
/// one `env:` line per variable, then `stdin:` and the bytes of its standard input to its end.
fn record(args: &[Vec<u8>], env: &[(Vec<u8>, Vec<u8>)], stdin: &[u8]) -> Vec<u8> {
    let mut text = Vec::new();
    for arg in args {
        text.extend_from_slice(b"arg:");
        text.extend_from_slice(arg);
        text.push(b'\n');
    }
    for (name, value) in env {
        text.extend_from_slice(b"env:");
        text.extend_from_slice(name);
        text.push(b'=');
        text.extend_from_slice(value);
        text.push(b'\n');
    }
    text.extend_from_slice(b"stdin:");
    text.extend_from_slice(stdin);
    text
}

/// Catches nothing itself: the child of `the_input_reaches_the_child_on_its_standard_input_alone`.
#[test]
#[ignore = "the child process of the_input_reaches_the_child_on_its_standard_input_alone; run only by it"]
fn child_records_its_arguments_environment_and_input() {
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    let _ = std::io::stderr().write_all(&record(&args, &own_environment(), &stdin));
    std::process::exit(0);
}

/// Catches nothing itself: the child of `a_failed_run_is_named_by_its_step_and_exit_code_alone`.
#[test]
#[ignore = "the child process of a_failed_run_is_named_by_its_step_and_exit_code_alone; run only by it"]
fn child_echoes_its_input_to_both_streams_and_exits_three() {
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let _ = std::io::stdout().write_all(&stdin);
    let _ = std::io::stderr().write_all(&stdin);
    std::process::exit(3);
}

/// Catches nothing itself: the child of `a_child_ended_by_a_signal_has_no_exit_code`.
#[test]
#[ignore = "the child process of a_child_ended_by_a_signal_has_no_exit_code; run only by it"]
fn child_aborts() {
    std::process::abort();
}

/// Catches nothing itself: the child of `a_child_printing_past_the_cap_is_cut_to_it`.
#[test]
#[ignore = "the child process of a_child_printing_past_the_cap_is_cut_to_it; run only by it"]
fn child_prints_twice_the_cap_to_both_streams() {
    let block = vec![b'x'; OUTPUT_MAX * 2];
    let _ = std::io::stderr().write_all(&block);
    let _ = std::io::stdout().write_all(&block);
    std::process::exit(0);
}

/// Catches nothing itself: the child of `a_child_that_never_reads_its_input_still_ends_the_run`.
#[test]
#[ignore = "the child process of a_child_that_never_reads_its_input_still_ends_the_run; run only by it"]
fn child_exits_without_reading() {
    let _ = std::io::stderr().write_all(b"left\n");
    std::process::exit(0);
}

/// Catches nothing itself: the child of `a_child_that_never_ends_is_killed_at_its_deadline`.
#[test]
#[ignore = "the child process of a_child_that_never_ends_is_killed_at_its_deadline; run only by it"]
fn child_never_ends() {
    loop {
        std::thread::park();
    }
}

/// Catches the input going anywhere but the child's standard input, the standard input not being
/// closed after it (the child reads to its end), and the parent's environment reaching the
/// child: the child's arguments are exactly those given, its environment is exactly empty, and
/// its standard input holds exactly the input.
#[test]
fn the_input_reaches_the_child_on_its_standard_input_alone() {
    let args = helper("child_records_its_arguments_environment_and_input");
    let out = runner(LONG).run(STEP, &args, INPUT).expect("the child ran");
    let given: Vec<Vec<u8>> = args.iter().map(|arg| arg.as_bytes().to_vec()).collect();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&record(&given, &[], INPUT))
    );
    assert_eq!(out.code, Some(0));
}

/// Catches a failed run's error holding the input, the arguments or what the child wrote: the
/// child echoes its input to both streams, and the error, as text and as a debug print, is the
/// step and the exit code alone. The output itself, which the error drops, did hold the input.
#[test]
fn a_failed_run_is_named_by_its_step_and_exit_code_alone() {
    let args = helper("child_echoes_its_input_to_both_streams_and_exits_three");
    let process = runner(LONG);
    let out = process.run(STEP, &args, INPUT).expect("the child ran");
    assert_eq!(out.stderr, INPUT);
    let error = run_ok(&process, STEP, &args, INPUT).expect_err("exit code 3");
    assert_eq!(
        error,
        StdinError::Exit {
            step: STEP.to_owned(),
            code: 3
        }
    );
    assert_eq!(
        error.to_string(),
        "softwareupdate: the program exited with code 3"
    );
    assert_eq!(
        format!("{error:?}"),
        "Exit { step: \"softwareupdate\", code: 3 }"
    );
}

/// Catches a run ended by a signal being taken for a success or for an exit code.
#[test]
fn a_child_ended_by_a_signal_has_no_exit_code() {
    let error =
        run_ok(&runner(LONG), STEP, &helper("child_aborts"), INPUT).expect_err("ended by a signal");
    assert_eq!(
        error.to_string(),
        "softwareupdate: the program ended without an exit code"
    );
}

/// Catches a child's output being kept past the cap, in either stream.
#[test]
fn a_child_printing_past_the_cap_is_cut_to_it() {
    let out = runner(LONG)
        .run(
            STEP,
            &helper("child_prints_twice_the_cap_to_both_streams"),
            INPUT,
        )
        .expect("the child ran");
    assert_eq!(out.stdout.len(), OUTPUT_MAX);
    assert_eq!(out.stderr, vec![b'x'; OUTPUT_MAX]);
}

/// Catches the runner waiting for a child to take all its input: the child reads none of an input
/// far larger than a pipe holds and exits, and the run ends with that exit.
#[test]
fn a_child_that_never_reads_its_input_still_ends_the_run() {
    let input = vec![b'p'; 4 * 1024 * 1024];
    let out = runner(LONG)
        .run(STEP, &helper("child_exits_without_reading"), &input)
        .expect("the child ran");
    assert_eq!(out.code, Some(0));
    assert_eq!(out.stderr, b"left\n");
}

/// Catches a child that never ends holding the runner: at its deadline (zero, so nothing the
/// child could print has been read) it is killed and the run is an error naming the step alone.
#[test]
fn a_child_that_never_ends_is_killed_at_its_deadline() {
    let error = runner(Duration::ZERO)
        .run(STEP, &helper("child_never_ends"), INPUT)
        .expect_err("killed at the deadline");
    assert_eq!(
        error,
        StdinError::TimedOut {
            step: STEP.to_owned()
        }
    );
    assert_eq!(
        error.to_string(),
        "softwareupdate: the program did not finish within its deadline and was killed"
    );
}

/// Catches a program that cannot start being reported as a run, or its error naming more than
/// the step and the system's reason.
#[test]
fn a_program_that_does_not_exist_is_a_spawn_error() {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "stdin-missing")
        .expect("a scratch directory");
    let process = StdinProcess::new(&dir.path().join("no-such-program"), LONG);
    assert_eq!(process.program(), dir.path().join("no-such-program"));
    let error = process
        .run(STEP, &["--install".to_owned()], INPUT)
        .expect_err("nothing to start");
    assert_eq!(
        error.to_string(),
        "softwareupdate: the program could not be started: No such file or directory (os error 2)"
    );
}

/// Catches `run_ok` refusing a success or changing its output.
#[test]
fn run_ok_gives_a_success_back_whole() {
    let fake = FakeStdinRun::new();
    let args = vec!["--list".to_owned()];
    fake.then(&args, Ok(LaunchctlOutput::success("listed\n")));
    assert_eq!(
        run_ok(&fake, STEP, &args, INPUT),
        Ok(LaunchctlOutput::success("listed\n"))
    );
}

/// Catches `run_ok` passing an error of the run on as anything else.
#[test]
fn run_ok_passes_a_run_error_on() {
    let fake = FakeStdinRun::new();
    let args = vec!["--list".to_owned()];
    let timed_out = StdinError::TimedOut {
        step: STEP.to_owned(),
    };
    fake.then(&args, Err(timed_out.clone()));
    assert_eq!(run_ok(&fake, STEP, &args, INPUT), Err(timed_out));
}

/// Catches the fake giving an answer twice, out of order, or for other arguments, or not keeping
/// what each call was given.
#[test]
fn the_fake_gives_each_answer_once_in_order_and_keeps_each_call() {
    let fake = FakeStdinRun::new();
    let kept = fake.clone();
    let args = vec!["--install".to_owned()];
    fake.then(&args, Ok(LaunchctlOutput::exit(1, "first")));
    fake.then(&args, Ok(LaunchctlOutput::success("second")));
    assert_eq!(
        fake.run(STEP, &args, b"one"),
        Ok(LaunchctlOutput::exit(1, "first"))
    );
    assert_eq!(
        fake.run(STEP, &args, b"two"),
        Ok(LaunchctlOutput::success("second"))
    );
    let none = Err(StdinError::Spawn {
        step: "other".to_owned(),
        why: "the fake has no answer".to_owned(),
    });
    assert_eq!(fake.run("other", &args, b"three"), none);
    assert_eq!(fake.run("other", &[], b""), none);
    let call = |step: &str, args: &[String], input: &[u8]| StdinCall {
        step: step.to_owned(),
        args: args.to_vec(),
        input: input.to_vec(),
    };
    assert_eq!(
        kept.calls(),
        vec![
            call(STEP, &args, b"one"),
            call(STEP, &args, b"two"),
            call("other", &args, b"three"),
            call("other", &[], b""),
        ]
    );
}

/// Catches a debug print of a recorded call, or of the fake that keeps it, holding a byte of the
/// input it was given: the input is the length alone.
#[test]
fn a_debug_print_of_a_call_withholds_its_input() {
    let fake = FakeStdinRun::new();
    let _ = fake.run(STEP, &["--install".to_owned()], b"pw\n");
    let expected =
        "StdinCall { step: \"softwareupdate\", args: [\"--install\"], input: <3 bytes withheld> }";
    assert_eq!(format!("{:?}", fake.calls()[0]), expected);
    assert_eq!(
        format!("{fake:?}"),
        format!(
            "FakeStdinRun(Mutex {{ data: StdinScript {{ queued: {{}}, calls: [{expected}] }}, poisoned: false, .. }})"
        )
    );
}
