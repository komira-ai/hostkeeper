//! The real `launchctl` adapter, run over this test binary as its child: the test binary stands
//! in for `launchctl`, and the helper tests below are what it is asked to run.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::launchctl::{
    LAUNCHCTL, Launchctl, LaunchctlError, LaunchctlProcess, OUTPUT_MAX,
};
use hostkeeper_testkit::Scratch;

const LONG: Duration = Duration::from_secs(60);

/// The adapter over this test binary, and the arguments that run the helper test `name` in it.
fn adapter(deadline: Duration) -> LaunchctlProcess {
    let exe = std::env::current_exe().expect("the test binary");
    LaunchctlProcess::new(&exe, deadline)
}

fn helper(name: &str) -> Vec<String> {
    [name, "--exact", "--ignored", "--nocapture"]
        .map(str::to_owned)
        .to_vec()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Catches nothing itself: the child of `a_child_is_read_with_its_streams_and_exit_code`.
#[test]
#[ignore = "the child process of a_child_is_read_with_its_streams_and_exit_code; run only by it"]
fn child_prints_to_both_streams_and_exits_three() {
    let _ = std::io::stdout().write_all(b"to-stdout\n");
    let _ = std::io::stderr().write_all(b"to-stderr\n");
    std::process::exit(3);
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

/// Catches nothing itself: the child of `a_child_has_an_empty_environment`.
#[test]
#[ignore = "the child process of a_child_has_an_empty_environment; run only by it"]
#[expect(
    clippy::disallowed_methods,
    reason = "this helper counts the environment its parent gave it"
)]
fn child_prints_its_environment_size() {
    // An instrumented build sets its own `__LLVM_PROFILE_RT_*` variable in the process it starts;
    // that is not something the parent passed.
    let count = std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(b"__LLVM_PROFILE_RT_"))
        .count();
    let _ = std::io::stdout().write_all(format!("env-count={count}\n").as_bytes());
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

/// Catches either stream or the exit code of a child being lost.
#[test]
fn a_child_is_read_with_its_streams_and_exit_code() {
    let out = adapter(LONG)
        .run(&helper("child_prints_to_both_streams_and_exits_three"))
        .expect("the child ran");
    assert_eq!(out.code, Some(3));
    assert!(text(&out.stdout).contains("to-stdout\n"), "{out:?}");
    assert_eq!(text(&out.stderr), "to-stderr\n");
}

/// Catches a child's output being kept past the cap, in either stream.
#[test]
fn a_child_printing_past_the_cap_is_cut_to_it() {
    let out = adapter(LONG)
        .run(&helper("child_prints_twice_the_cap_to_both_streams"))
        .expect("the child ran");
    assert_eq!(out.stdout.len(), OUTPUT_MAX);
    assert_eq!(out.stderr.len(), OUTPUT_MAX);
    assert!(out.stderr.iter().all(|byte| *byte == b'x'));
}

/// Catches the parent's environment reaching the child.
#[test]
fn a_child_has_an_empty_environment() {
    let out = adapter(LONG)
        .run(&helper("child_prints_its_environment_size"))
        .expect("the child ran");
    assert!(text(&out.stdout).contains("env-count=0\n"), "{out:?}");
}

/// Catches a child that never ends holding the adapter: at its deadline (zero, so nothing the
/// child could print has been read) it is killed and the run is an error naming the arguments.
#[test]
fn a_child_that_never_ends_is_killed_at_its_deadline() {
    let args = helper("child_never_ends");
    let result = adapter(Duration::ZERO).run(&args);
    assert_eq!(
        result,
        Err(LaunchctlError::TimedOut {
            args: args.join(" ")
        })
    );
}

/// Catches a program that cannot start being reported as a run.
#[test]
fn a_program_that_does_not_exist_is_a_spawn_error() {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "launchctl-missing")
        .expect("a scratch directory");
    let process = LaunchctlProcess::new(&dir.path().join("no-such-program"), LONG);
    let args = vec!["print".to_owned(), "system/x".to_owned()];
    assert_eq!(
        process.run(&args),
        Err(LaunchctlError::Spawn {
            args: "print system/x".to_owned(),
            why: "No such file or directory (os error 2)".to_owned()
        })
    );
}

/// Catches the system adapter running anything but `/bin/launchctl`.
#[test]
fn the_system_adapter_runs_launchctl() {
    let process = LaunchctlProcess::system(LONG);
    assert_eq!(process.program(), Path::new(LAUNCHCTL));
    assert_eq!(LAUNCHCTL, "/bin/launchctl");
}
