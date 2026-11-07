//! The probe of the host's launcher against a spawner that records what it is asked to run: the
//! `systemd-run` line of a probe, its refusals, and the exit code and bounded output it gives.

mod support;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_steps::systemd::probe_command;
use hostkeeper_steps::{
    Captured, LaunchError, ProbeCall, ProbeExit, Program, RING_BYTES, Ring, Spawner, StepLauncher,
    SystemdLauncher,
};
use support::Scratch;

/// A spawner that records each program and its arguments, and answers from a script.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(Program, Vec<String>)>>,
    replies: Mutex<VecDeque<io::Result<Captured>>>,
}

impl Recorder {
    fn answering(replies: Vec<io::Result<Captured>>) -> Recorder {
        Recorder {
            calls: Mutex::default(),
            replies: Mutex::new(replies.into()),
        }
    }

    fn calls(&self) -> Vec<(Program, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Spawner for &Recorder {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured> {
        let args = args
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect();
        self.calls.lock().unwrap().push((program, args));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a reply is scripted")
    }
}

fn captured(code: Option<i32>, stdout: &[u8]) -> Captured {
    let mut out = Ring::new();
    out.push(stdout);
    Captured {
        code,
        stdout: out,
        stderr: Ring::new(),
    }
}

/// What the log was given: each unit, with the standard output captured.
type Logged = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

const UNIT: &str = "hostkeeper-self-update-protocol";
const ARGS: &[&str] = &["self-update-protocol"];

fn probe<'a>(executable: &'a Path, timeout_secs: u64, output_bytes: usize) -> ProbeCall<'a> {
    ProbeCall {
        service: "hostkeeper",
        executable,
        args: ARGS,
        unit: UNIT,
        timeout_secs,
        output_bytes,
    }
}

/// A scratch directory with an executable file `hostkeeper` in it.
fn binary(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let file = scratch.path().join("hostkeeper");
    std::fs::write(&file, "#!hostkeeper\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    (scratch, file)
}

fn line(executable: &Path, max: &str) -> Vec<String> {
    [
        "--system",
        "--quiet",
        "--wait",
        "--collect",
        "--pipe",
        "--unit=hostkeeper-self-update-protocol",
        &format!("--property=RuntimeMaxSec={max}"),
        "--",
        executable.to_str().unwrap(),
        "self-update-protocol",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Catches a probe that is not run in its own bounded transient unit, or with other arguments:
/// the line is exactly the documented one, `RuntimeMaxSec=` the probe's bound, and a bound of 0
/// is 1 second, never unbounded.
#[test]
fn the_probe_runs_its_binary_in_a_bounded_transient_unit() {
    let (_scratch, file) = binary("probe-line");
    let recorder = Recorder::answering(vec![
        Ok(captured(Some(0), b"1\n")),
        Ok(captured(Some(0), b"1\n")),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    launcher.run_probe(&probe(&file, 10, 64)).unwrap();
    launcher.run_probe(&probe(&file, 0, 64)).unwrap();
    assert_eq!(
        recorder.calls(),
        [
            (Program::SystemdRun, line(&file, "10")),
            (Program::SystemdRun, line(&file, "1")),
        ]
    );
    let made: Vec<String> = probe_command(&probe(&file, 10, 64))
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    assert_eq!(made, line(&file, "10"));
}

/// Catches a probe started from a path that cannot run: a relative path is a launcher failure,
/// a missing or non-executable file is `NoStart`, each before anything is spawned; a spawn that
/// fails is a launcher failure.
#[test]
fn a_binary_that_cannot_run_is_refused_before_anything_is_spawned() {
    let (scratch, file) = binary("probe-refused");
    let plain = scratch.path().join("plain");
    std::fs::write(&plain, "data").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let missing = scratch.path().join("missing");
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);

    let err = launcher
        .run_probe(&probe(Path::new("bin/hostkeeper"), 10, 64))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text)
            if text == "the probed binary \"bin/hostkeeper\" is not an absolute path"),
        "{err:?}"
    );
    let err = launcher.run_probe(&probe(&missing, 10, 64)).unwrap_err();
    let want = format!(
        "{}: No such file or directory (os error 2)",
        missing.display()
    );
    assert!(
        matches!(&err, LaunchError::NoStart(text) if *text == want),
        "{err:?}"
    );
    let err = launcher.run_probe(&probe(&plain, 10, 64)).unwrap_err();
    let want = format!("{} is not an executable file", plain.display());
    assert!(
        matches!(&err, LaunchError::NoStart(text) if *text == want),
        "{err:?}"
    );
    assert!(recorder.calls().is_empty());

    let recorder = Recorder::answering(vec![Err(io::Error::other("no such program"))]);
    let err = SystemdLauncher::new(&recorder)
        .run_probe(&probe(&file, 10, 64))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text) if text == "SystemdRun could not be run: no such program"),
        "{err:?}"
    );
}

/// Catches the exit code or the output lost or unbounded: the code of `systemd-run` (none for a
/// probe killed by a signal) and its output are the probe's exit; an output of exactly the
/// limit is whole, one byte more is cut to the limit and reported, as is an output the capture
/// itself had to cut; the log is given what was captured, under the probe's unit.
#[test]
fn the_exit_code_and_at_most_the_limit_of_the_output_are_the_probe_exit() {
    let (_scratch, file) = binary("probe-exit");
    let mut cut = Ring::with_capacity(4);
    cut.push(b"123456");
    let recorder = Recorder::answering(vec![
        Ok(captured(Some(0), b"1\n")),
        Ok(captured(Some(0), b"1234")),
        Ok(captured(Some(0), b"12345")),
        Ok(captured(None, b"")),
        Ok(Captured {
            code: Some(0),
            stdout: cut,
            stderr: Ring::new(),
        }),
    ]);
    let logged: Logged = Arc::default();
    let log = logged.clone();
    let launcher =
        SystemdLauncher::new(&recorder).with_log(Arc::new(move |unit: &str, got: &Captured| {
            log.lock()
                .unwrap()
                .push((unit.to_owned(), got.stdout.bytes()));
        }));
    let exits: Vec<ProbeExit> = (0..5)
        .map(|_| launcher.run_probe(&probe(&file, 10, 4)).unwrap())
        .collect();
    let exit = |code: Option<i32>, output: &[u8], truncated: bool| ProbeExit {
        code,
        output: output.to_vec(),
        truncated,
    };
    assert_eq!(
        exits,
        [
            exit(Some(0), b"1\n", false),
            exit(Some(0), b"1234", false),
            exit(Some(0), b"1234", true),
            exit(None, b"", false),
            exit(Some(0), b"3456", true),
        ]
    );
    let logged = logged.lock().unwrap();
    assert_eq!(logged.len(), 5);
    assert_eq!(logged[0], (UNIT.to_owned(), b"1\n".to_vec()));
}

/// Catches a probe whose result depends on the log: a launcher with no log gives the same exit.
#[test]
fn a_launcher_with_no_log_still_gives_the_probe_exit() {
    let (_scratch, file) = binary("probe-no-log");
    let recorder = Recorder::answering(vec![Ok(captured(Some(3), b"x"))]);
    let exit = SystemdLauncher::new(&recorder)
        .run_probe(&probe(&file, 10, RING_BYTES))
        .unwrap();
    assert_eq!(
        exit,
        ProbeExit {
            code: Some(3),
            output: b"x".to_vec(),
            truncated: false,
        }
    );
}
