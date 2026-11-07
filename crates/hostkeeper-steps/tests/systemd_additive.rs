//! An additive step (the hook's `uninstall`, or a `[hook] boot` step) on the launcher of the host,
//! against a spawner that records what it is asked to run: the `systemd-run` line of the step, its
//! refusals, and the exit and last line it gives. Before `SystemdLauncher` ran additive steps, every
//! one of them was the trait's default, `NoStart`, so no uninstall order completed on Linux
//! (komira-ai/hostkeeper#110).

mod support;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_steps::systemd::additive_command;
use hostkeeper_steps::{
    AdditiveCall, Captured, LaunchError, Program, Ring, Spawner, StepExit, StepLauncher,
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

fn captured(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Captured {
    let (mut out, mut err) = (Ring::new(), Ring::new());
    out.push(stdout);
    err.push(stderr);
    Captured {
        code,
        stdout: out,
        stderr: err,
    }
}

/// What the log was given: each unit, with the standard error captured (the hook's log).
type Logged = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

const UNIT: &str = "updater-svc-uninstall-4";

/// A scratch directory with an executable file `hook` in it, and the path of a params file there.
fn hook_file(name: &str) -> (Scratch, PathBuf, PathBuf) {
    let scratch = Scratch::new(name);
    let hook = scratch.path().join("hook");
    std::fs::write(&hook, "#!hook\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let params = scratch.path().join("params.toml");
    (scratch, hook, params)
}

fn uninstall<'a>(
    executable: &'a Path,
    hook_args: &'a [String],
    params: &'a Path,
    timeout_secs: u64,
) -> AdditiveCall<'a> {
    AdditiveCall {
        service: "svc",
        step: "uninstall",
        hook_args,
        params,
        executable,
        unit: UNIT,
        timeout_secs,
    }
}

/// The documented line of an additive step: `systemd-run`'s options, the unit, `RuntimeMaxSec=`
/// at `max`, then the hook and its arguments.
fn line(executable: &Path, max: &str, rest: &[&str]) -> Vec<String> {
    let mut line: Vec<String> = [
        "--system",
        "--quiet",
        "--wait",
        "--collect",
        "--pipe",
        &format!("--unit={UNIT}"),
        &format!("--property=RuntimeMaxSec={max}"),
        "--",
        executable.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .to_vec();
    line.extend(rest.iter().map(|a| (*a).to_owned()));
    line
}

/// Catches an additive step not run, or run with other arguments, another unit or another bound:
/// the line is exactly the documented one, with the hook command's own arguments before the
/// step's name and `--params <file>` after it; `RuntimeMaxSec=` is the step's bound, and a bound
/// of 0 is `infinity`; a hook command with no arguments gives the step's name right after the
/// executable. `additive_command` gives the same line as the run.
#[test]
fn the_additive_step_runs_in_its_bounded_transient_unit() {
    let (_scratch, hook, params) = hook_file("additive-line");
    let args = vec!["update-hook".to_owned()];
    let recorder = Recorder::answering(vec![
        Ok(captured(Some(0), b"{}\n", b"")),
        Ok(captured(Some(0), b"{}\n", b"")),
        Ok(captured(Some(0), b"{}\n", b"")),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    launcher
        .run_additive(&uninstall(&hook, &args, &params, 60))
        .unwrap();
    launcher
        .run_additive(&uninstall(&hook, &args, &params, 0))
        .unwrap();
    launcher
        .run_additive(&uninstall(&hook, &[], &params, 5))
        .unwrap();
    let file = params.to_str().unwrap();
    assert_eq!(
        recorder.calls(),
        [
            (
                Program::SystemdRun,
                line(&hook, "60", &["update-hook", "uninstall", "--params", file])
            ),
            (
                Program::SystemdRun,
                line(
                    &hook,
                    "infinity",
                    &["update-hook", "uninstall", "--params", file]
                )
            ),
            (
                Program::SystemdRun,
                line(&hook, "5", &["uninstall", "--params", file])
            ),
        ]
    );
    let made: Vec<String> = additive_command(&uninstall(&hook, &args, &params, 60))
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    assert_eq!(
        made,
        line(&hook, "60", &["update-hook", "uninstall", "--params", file])
    );
}

/// Catches an additive step started from a hook or params file that cannot be used: a relative
/// executable or params file is a launcher failure, a missing or non-executable hook is
/// `NoStart` (the caller reports the step failed), each before anything is spawned; a spawn
/// that fails is a launcher failure.
#[test]
fn a_hook_that_cannot_run_is_refused_before_anything_is_spawned() {
    let (scratch, hook, params) = hook_file("additive-refused");
    let plain = scratch.path().join("plain");
    std::fs::write(&plain, "data").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let missing = scratch.path().join("missing");
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);

    let err = launcher
        .run_additive(&uninstall(Path::new("bin/hook"), &[], &params, 5))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text)
            if text == "the hook executable \"bin/hook\" is not an absolute path"),
        "{err:?}"
    );
    let err = launcher
        .run_additive(&uninstall(&hook, &[], Path::new("params.toml"), 5))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text)
            if text == "the params file \"params.toml\" is not an absolute path"),
        "{err:?}"
    );
    let err = launcher
        .run_additive(&uninstall(&missing, &[], &params, 5))
        .unwrap_err();
    let want = format!(
        "{}: No such file or directory (os error 2)",
        missing.display()
    );
    assert!(
        matches!(&err, LaunchError::NoStart(text) if *text == want),
        "{err:?}"
    );
    let err = launcher
        .run_additive(&uninstall(&plain, &[], &params, 5))
        .unwrap_err();
    let want = format!("{} is not an executable file", plain.display());
    assert!(
        matches!(&err, LaunchError::NoStart(text) if *text == want),
        "{err:?}"
    );
    assert!(recorder.calls().is_empty());

    let recorder = Recorder::answering(vec![Err(io::Error::other("no such program"))]);
    let err = SystemdLauncher::new(&recorder)
        .run_additive(&uninstall(&hook, &[], &params, 5))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text) if text == "SystemdRun could not be run: no such program"),
        "{err:?}"
    );
}

/// Catches the hook's exit, last line or log lost: the exit code of `systemd-run` (none for a
/// hook killed by a signal) and the last line the hook printed are the step's exit, whether it
/// succeeded or failed; the log is given the hook's standard error under the step's unit, once
/// per run; a launcher with no log gives the same exit.
#[test]
fn the_exit_and_last_line_of_systemd_run_are_the_additive_step_exit() {
    let (_scratch, hook, params) = hook_file("additive-exit");
    let recorder = Recorder::answering(vec![
        Ok(captured(
            Some(0),
            b"working\n{\"detail\":\"removed\"}\n",
            b"svc: uninstall ok\n",
        )),
        Ok(captured(Some(1), b"{\"reason\":\"busy\"}\n", b"")),
        Ok(captured(None, b"", b"")),
    ]);
    let logged: Logged = Arc::default();
    let log = logged.clone();
    let launcher =
        SystemdLauncher::new(&recorder).with_log(Arc::new(move |unit: &str, got: &Captured| {
            log.lock()
                .unwrap()
                .push((unit.to_owned(), got.stderr.bytes()));
        }));
    let exits: Vec<StepExit> = (0..3)
        .map(|_| {
            launcher
                .run_additive(&uninstall(&hook, &[], &params, 5))
                .unwrap()
        })
        .collect();
    let exit = |code: Option<i32>, last_line: Option<&str>| StepExit {
        code,
        last_line: last_line.map(str::to_owned),
    };
    assert_eq!(
        exits,
        [
            exit(Some(0), Some(r#"{"detail":"removed"}"#)),
            exit(Some(1), Some(r#"{"reason":"busy"}"#)),
            exit(None, None),
        ]
    );
    assert_eq!(
        *logged.lock().unwrap(),
        [
            (UNIT.to_owned(), b"svc: uninstall ok\n".to_vec()),
            (UNIT.to_owned(), Vec::new()),
            (UNIT.to_owned(), Vec::new()),
        ]
    );

    let recorder = Recorder::answering(vec![Ok(captured(Some(3), b"x\n", b""))]);
    let exit = SystemdLauncher::new(&recorder)
        .run_additive(&uninstall(&hook, &[], &params, 5))
        .unwrap();
    assert_eq!(
        exit,
        StepExit {
            code: Some(3),
            last_line: Some("x".to_owned()),
        }
    );
}
