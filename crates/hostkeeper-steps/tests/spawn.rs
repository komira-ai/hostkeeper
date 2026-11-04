//! The real spawner, with the fixture program (`hostkeeper-steps-spawn-fixture`) in place of
//! `systemd-run` and `systemctl`: both streams read at once to their end, a bounded tail of each
//! kept, the last line read within its bound, an empty environment, and the stop.

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Invocation, Outcome, Report, Step};
use hostkeeper_steps::systemd::unit_name;
use hostkeeper_steps::{
    Captured, LaunchError, Program, RING_BYTES, Spawner, StepCall, StepLauncher, SystemdLauncher,
    SystemdRunProcess,
};
use support::{Scratch, secs};

const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-steps-spawn-fixture");

/// What the launcher logged of the last run.
type Logged = Arc<Mutex<Option<Captured>>>;

fn launcher() -> (SystemdLauncher<SystemdRunProcess>, Logged) {
    let fixture = Path::new(FIXTURE);
    let logged: Logged = Arc::default();
    let log = logged.clone();
    let launcher = SystemdLauncher::new(SystemdRunProcess::new(fixture, fixture)).with_log(
        Arc::new(move |_: &str, got: &Captured| *log.lock().unwrap() = Some(got.clone())),
    );
    (launcher, logged)
}

/// Runs a `cutover` whose params file tells the fixture what to do.
fn run_mode(
    scratch: &Scratch,
    mode: &str,
) -> (Result<hostkeeper_steps::StepExit, LaunchError>, Captured) {
    let params: PathBuf = scratch.path().join("params");
    std::fs::write(&params, mode).unwrap();
    let unit = unit_name("svc", Step::Cutover, 3);
    let call = StepCall {
        service: "svc",
        invocation: Invocation {
            step: Step::Cutover,
            artifact: None,
            params: Some(params),
        },
        // The fixture is an executable file, which is all the launcher checks of the hook.
        executable: Path::new(FIXTURE),
        unit: &unit,
        bound: secs(600),
    };
    let (launcher, logged) = launcher();
    let exit = launcher.run(&call);
    let captured = logged.lock().unwrap().take().expect("the run was logged");
    (exit, captured)
}

/// `len` bytes of `a` to `z` over and over from `a`, as the fixture writes them.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| b'a' + (i % 26) as u8).collect()
}

/// Catches output held without a bound, a ring that keeps the first bytes instead of the last,
/// and an oversized line being parsed: a hook that prints 16 MiB of standard output and 4 MiB of
/// standard error with no line feed keeps exactly the last 1 MiB of each, has no last line, so
/// its report is `{}`, and the exit code alone decides the step: `1` a failure, `0` a success.
#[test]
fn a_hook_that_prints_without_limit_is_bounded_and_its_exit_code_stands() {
    let scratch = Scratch::new("spawn-flood");
    let (out_len, err_len) = (16 * RING_BYTES, 4 * RING_BYTES);
    for (code, outcome) in [(1, Outcome::Failure), (0, Outcome::Success)] {
        let (exit, captured) = run_mode(&scratch, &format!("flood {out_len} {err_len} {code}"));
        let exit = exit.unwrap();
        assert_eq!(exit.code, Some(code));
        assert_eq!(exit.last_line, None);
        let report = Report::read(exit.last_line.as_deref().unwrap_or(""));
        assert_eq!(report, Report::default());
        assert_eq!(Outcome::from_exit(exit.code), outcome);
        let (stdout, stderr) = (captured.stdout.bytes(), captured.stderr.bytes());
        assert_eq!(stdout.len(), RING_BYTES);
        assert_eq!(stderr.len(), RING_BYTES);
        assert!(
            stdout == pattern(out_len)[out_len - RING_BYTES..],
            "the last bytes are kept"
        );
        assert!(
            stderr == pattern(err_len)[err_len - RING_BYTES..],
            "the last bytes are kept"
        );
        assert!(captured.stdout.cut() && captured.stderr.cut());
    }
}

/// Catches streams read one after the other: the fixture writes 4 MiB to standard error (far
/// more than a pipe holds) before any standard output, and the run ends with both read to their
/// end. Read in turn, the fixture would block on the full pipe and the test would not end (the
/// harness's own deadline, not a timing assertion, is what fails it).
#[test]
fn stdout_and_stderr_are_drained_together() {
    let scratch = Scratch::new("spawn-drain");
    let (exit, captured) = run_mode(&scratch, &format!("flood 4096 {} 0", 4 * RING_BYTES));
    assert_eq!(exit.unwrap().code, Some(0));
    assert_eq!(captured.stdout.bytes(), pattern(4096));
    assert!(!captured.stdout.cut());
    assert_eq!(captured.stderr.bytes().len(), RING_BYTES);
}

/// Catches a change to how the report of an ordinary hook is read: log lines, then a JSON line,
/// give that line as the report, and the warning on standard error is kept for the log.
#[test]
fn the_last_line_of_a_normal_hook_is_the_report() {
    let scratch = Scratch::new("spawn-report");
    let (exit, captured) = run_mode(&scratch, "report 0");
    let exit = exit.unwrap();
    assert_eq!(exit.code, Some(0));
    assert_eq!(exit.last_line.as_deref(), Some(r#"{"reason":"all_good"}"#));
    let report = Report::read(exit.last_line.as_deref().unwrap_or(""));
    assert_eq!(report.reason.as_deref(), Some("all_good"));
    assert_eq!(captured.stderr.bytes(), b"a warning\n");
}

/// Catches the bound of the last line not holding on output read from a real pipe: 64 KiB is
/// read, one byte more is not.
#[test]
fn a_long_last_line_from_a_pipe_is_read_only_within_its_bound() {
    let scratch = Scratch::new("spawn-line");
    let (exit, _) = run_mode(&scratch, "line 65536 0");
    assert_eq!(exit.unwrap().last_line.map(|l| l.len()), Some(65_536));
    let (exit, _) = run_mode(&scratch, "line 65537 0");
    assert_eq!(exit.unwrap().last_line, None);
}

/// Catches the real spawner giving the program other arguments than the line of the step, or
/// any input: the fixture prints what it received, and its input is empty.
#[test]
fn systemd_run_is_given_the_line_of_the_step_and_no_input() {
    let scratch = Scratch::new("spawn-args");
    let params = scratch.path().join("params");
    let (exit, captured) = run_mode(&scratch, "received");
    assert_eq!(exit.unwrap().code, Some(0));
    let mut want = vec![
        "--system".to_owned(),
        "--quiet".to_owned(),
        "--wait".to_owned(),
        "--collect".to_owned(),
        "--pipe".to_owned(),
        "--unit=updater-svc-cutover-3".to_owned(),
        "--property=RuntimeMaxSec=600".to_owned(),
        "--".to_owned(),
        FIXTURE.to_owned(),
        "cutover".to_owned(),
        "--params".to_owned(),
        params.display().to_string(),
    ];
    want.push("stdin:".to_owned());
    assert_eq!(
        String::from_utf8(captured.stdout.bytes()).unwrap(),
        want.join("\n")
    );
}

/// Catches the updater's environment reaching `systemd-run`: it is run with none.
#[test]
fn systemd_run_is_given_no_environment() {
    let scratch = Scratch::new("spawn-env");
    let (exit, captured) = run_mode(&scratch, "environ");
    assert_eq!(exit.unwrap().code, Some(0));
    assert_eq!(String::from_utf8_lossy(&captured.stdout.bytes()), "");
}

/// Catches the real stop not reaching `systemctl` with the unit, or reading its exit wrongly.
#[test]
fn the_real_stop_runs_systemctl_and_reads_its_exit() {
    let fixture = Path::new(FIXTURE);
    let spawner = SystemdRunProcess::new(fixture, fixture);
    assert_eq!(spawner.program(Program::Systemctl), fixture);
    let said = spawner
        .spawn(
            Program::Systemctl,
            &[
                OsString::from("stop"),
                OsString::from("--system"),
                OsString::from("u-1"),
            ],
        )
        .unwrap();
    assert_eq!(said.stdout.bytes(), b"stop --system u-1");
    let launcher = SystemdLauncher::new(spawner);
    launcher.stop("updater-svc-cutover-3").unwrap();
    launcher.stop("updater-unloaded-cutover-3").unwrap();
    let err = launcher.stop("updater-refused-cutover-3").unwrap_err();
    assert_eq!(
        err.to_string(),
        "the launcher failed: systemctl stop updater-refused-cutover-3 exited with Some(4): \
         Failed to stop updater-refused-cutover-3.service: Access denied"
    );
}

/// Catches a program that cannot be started being anything but a launcher error, which leaves
/// the step `running` (a hook that cannot start is `NoStart`; this is the updater's own side).
#[test]
fn a_systemd_run_that_cannot_start_is_a_launcher_error() {
    let scratch = Scratch::new("spawn-missing");
    let missing = scratch.path().join("no-systemd-run");
    let launcher = SystemdLauncher::new(SystemdRunProcess::new(&missing, &missing));
    let unit = unit_name("svc", Step::Verify, 1);
    let call = StepCall {
        service: "svc",
        invocation: Invocation {
            step: Step::Verify,
            artifact: None,
            params: Some(scratch.path().join("params")),
        },
        executable: Path::new(FIXTURE),
        unit: &unit,
        bound: secs(5),
    };
    let err = launcher.run(&call).unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text) if text.starts_with("SystemdRun could not be run: ")),
        "{err:?}"
    );
    let err = launcher.stop("updater-svc-cutover-3").unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text) if text.starts_with("Systemctl could not be run: ")),
        "{err:?}"
    );
}
