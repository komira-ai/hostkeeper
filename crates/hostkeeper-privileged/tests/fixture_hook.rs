//! The fixture program itself, run as the launcher runs a hook but with no systemd and no root:
//! its exit code, last line and log for a step, a refused command line, `print-pid`'s file, and
//! `sleep`, which runs on with its child until a signal ends it. Part of the pull-request set.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use hostkeeper_hook::{Report, Step};
use hostkeeper_privileged::Rig;
use hostkeeper_privileged::fixture::{self, Mode, ok_report};
use hostkeeper_testkit::Scratch;
use rustix::process::{Pid, Signal, kill_process};

const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");

fn bench(name: &str) -> (Scratch, Rig) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = Scratch::under(base, name).expect("create the scratch directory");
    let rig =
        Rig::new(scratch.path(), Path::new(FIXTURE), name, std::process::id()).expect("a rig");
    (scratch, rig)
}

fn step(step: Step, params: &Path) -> Output {
    Command::new(FIXTURE)
        .arg(step.name())
        .arg("--params")
        .arg(params)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("run the fixture")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("text")
}

/// Catches a fixture program whose step does not end as the library decides: `ok` exits `0`
/// with a last line that round-trips through `hostkeeper_hook::Report` and logs to standard
/// error, and `fail-at` exits `1` at its step.
#[test]
fn the_fixture_program_runs_a_step_with_its_exit_code_and_last_line() {
    let (_scratch, rig) = bench("program_ok");
    let ok = rig.params("ok", &Mode::Ok, None).expect("params");
    let out = step(Step::Cutover, &ok);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(Report::read(&text(&out.stdout)), ok_report(Step::Cutover));
    assert_eq!(text(&out.stderr), "fixture: cutover ok\n");
    let fail = rig
        .params("fail", &Mode::FailAt(Step::Verify), None)
        .expect("params");
    let out = step(Step::Verify, &fail);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        Report::read(&text(&out.stdout)),
        fixture::failed_report(Step::Verify)
    );
}

/// Catches a refused command line that the program runs anyway or ends with another code than
/// a failure: an unknown mode, and an argument after `serve`, each exit `1` with the reason.
#[test]
fn the_fixture_program_refuses_a_bad_command_line_with_exit_1() {
    let (scratch, _rig) = bench("program_refuses");
    let params = scratch.path().join("params");
    std::fs::write(&params, "\"fixture.mode\" = \"nap\"\n").expect("write params");
    let out = step(Step::Cutover, &params);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "hostkeeper-fixture-hook: unknown fixture mode \"nap\" (ok, sleep, fail-at=<step>, \
         print-pid, flood=<bytes>)\n"
    );
    assert!(out.stdout.is_empty());
    let out = Command::new(FIXTURE)
        .args(["serve", "now"])
        .output()
        .expect("run the fixture");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "hostkeeper-fixture-hook: serve takes no argument, not \"now\"\n"
    );
}

/// Catches `print-pid` writing another process's pid or the digest of another file than the
/// program that ran.
#[test]
fn print_pid_names_the_program_that_ran() {
    let (_scratch, rig) = bench("program_print_pid");
    let pids = rig.pid_file("print");
    let params = rig
        .params("print", &Mode::PrintPid, Some(pids.as_path()))
        .expect("params");
    let child = Command::new(FIXTURE)
        .args(["verify", "--params"])
        .arg(&params)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("run the fixture");
    let pid = child.id();
    let out = child.wait_with_output().expect("it ends");
    assert_eq!(out.status.code(), Some(0));
    let found = fixture::read_pid_file(&pids).expect("the pid file");
    assert_eq!(found.pids, [pid]);
    let digest = fixture::file_sha256(Path::new(FIXTURE)).expect("hash the fixture");
    assert_eq!(found.exe_sha256, Some(digest));
}

/// Waits until `path` exists, or fails with how `child` ended if it ended first: a fixture that
/// exits instead of blocking fails here rather than hangs.
fn wait_for(path: &Path, child: &mut std::process::Child) {
    while !path.exists() {
        if let Some(status) = child.try_wait().expect("look at the fixture") {
            panic!(
                "the fixture ended ({status}) before it wrote {}",
                path.display()
            );
        }
        std::thread::yield_now();
    }
}

/// Catches a `sleep` step that ends on its own, writes its pid file before its child runs, or
/// starts no child: it writes its pid and its child's, runs on, and ends only by the signal it
/// is sent.
#[test]
fn sleep_runs_with_its_child_until_a_signal_ends_it() {
    let (_scratch, rig) = bench("program_sleep");
    let pids = rig.pid_file("sleep");
    let params = rig
        .params("sleep", &Mode::Sleep, Some(pids.as_path()))
        .expect("params");
    let mut child = Command::new(FIXTURE)
        .args(["cutover", "--params"])
        .arg(&params)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("run the fixture");
    wait_for(&pids, &mut child);
    let found = fixture::read_pid_file(&pids).expect("the pid file");
    assert_eq!(found.pids.len(), 2, "{found:?}");
    assert_eq!(found.pids[0], child.id());
    assert_eq!(child.try_wait().expect("look"), None, "sleep runs on");
    for pid in found.pids.iter().rev() {
        let pid = Pid::from_raw(i32::try_from(*pid).expect("a pid")).expect("not 0");
        kill_process(pid, Signal::TERM).expect("the process is there to signal");
    }
    let status = child.wait().expect("it ends");
    assert_eq!(status.signal(), Some(Signal::TERM.as_raw()), "{status}");
}
