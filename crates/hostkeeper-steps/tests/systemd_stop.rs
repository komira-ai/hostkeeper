//! A step that the launcher of the host stopped is reported with no exit code. systemd counts the
//! `SIGTERM` of `systemctl stop` as a clean end of a service, so `systemd-run --wait` exits `0`
//! for a hook stopped that way; the spawner here answers as it does, and the launcher must still
//! report a failed step. A stop of another unit, or one made before the run started, changes
//! nothing of the run. An additive step (the hook's `uninstall`, whose caller stops its unit just
//! before it runs it) is held to the same rules.

mod support;

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_steps::{
    AdditiveCall, Captured, Program, Ring, Spawner, StepCall, StepExit, StepLauncher,
    SystemdLauncher,
};
use support::{Scratch, secs};

/// What `systemd-run --wait` prints and exits with for a hook the stop's `SIGTERM` ended.
const LAST_LINE: &str = r#"{"class":"release"}"#;

#[derive(Default)]
struct State {
    /// The next `systemd-run` waits for a `systemctl stop` before it answers.
    hold_next_run: bool,
    /// A `systemd-run` is waiting.
    running: bool,
    /// The units `systemctl stop` was run for, in order.
    stops: Vec<String>,
}

/// A spawner whose `systemd-run` can be held until a stop arrives, as a real unit runs until
/// systemd stops it.
#[derive(Default)]
struct Held {
    state: Mutex<State>,
    changed: Condvar,
}

impl Held {
    fn holding() -> Arc<Held> {
        let held = Arc::new(Held::default());
        held.state.lock().unwrap().hold_next_run = true;
        held
    }

    /// Waits until a held `systemd-run` is waiting.
    fn wait_running(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.running {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn stops(&self) -> Vec<String> {
        self.state.lock().unwrap().stops.clone()
    }
}

struct Spawns(Arc<Held>);

fn answer(code: Option<i32>, stdout: &str) -> Captured {
    let mut out = Ring::new();
    out.push(stdout.as_bytes());
    Captured {
        code,
        stdout: out,
        stderr: Ring::new(),
    }
}

impl Spawner for Spawns {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured> {
        let held = &self.0;
        let mut state = held.state.lock().unwrap();
        match program {
            Program::SystemdRun => {
                if std::mem::take(&mut state.hold_next_run) {
                    let before = state.stops.len();
                    state.running = true;
                    held.changed.notify_all();
                    while state.stops.len() == before {
                        state = held.changed.wait(state).unwrap();
                    }
                    state.running = false;
                }
                Ok(answer(Some(0), &format!("{LAST_LINE}\n")))
            }
            Program::Systemctl => {
                let unit = args.last().unwrap().to_str().unwrap().to_owned();
                state.stops.push(unit);
                held.changed.notify_all();
                Ok(answer(Some(0), ""))
            }
        }
    }
}

/// A scratch directory with an executable file `hook` in it.
fn hook_file(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let hook = scratch.path().join("hook");
    std::fs::write(&hook, "#!hook\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    (scratch, hook)
}

fn call<'a>(executable: &'a Path, params: &Path, unit: &'a str) -> StepCall<'a> {
    StepCall {
        service: "svc",
        hook_args: &[],
        invocation: Invocation {
            step: Step::Cutover,
            artifact: None,
            params: Some(params.to_path_buf()),
        },
        executable,
        unit,
        bound: secs(600),
    }
}

/// Runs a step of `unit` on a thread of its own, stops `stop` once the step is running, and
/// gives back how the step ended.
fn run_and_stop(
    launcher: &Arc<SystemdLauncher<Spawns>>,
    held: &Held,
    unit: &str,
    stop: &str,
) -> StepExit {
    let (scratch, hook) = hook_file("stop-run");
    let params = scratch.path().join("params.toml");
    std::thread::scope(|scope| {
        let running = scope.spawn(|| launcher.run(&call(&hook, &params, unit)));
        held.wait_running();
        launcher.stop(stop).unwrap();
        running.join().unwrap().unwrap()
    })
}

/// Catches a step that the dead-man stopped at its bound read as a success: `systemd-run`
/// exits `0` for it, and the launcher must report no exit code (a failed step), keeping the last
/// line. A second run under the same name that nobody stops is read by its own exit code again.
#[test]
fn a_step_the_launcher_stopped_reports_no_exit_code() {
    let held = Held::holding();
    let launcher = Arc::new(SystemdLauncher::new(Spawns(Arc::clone(&held))));
    let unit = "updater-svc-cutover-7";
    let exit = run_and_stop(&launcher, &held, unit, unit);
    assert_eq!(
        exit,
        StepExit {
            code: None,
            last_line: Some(LAST_LINE.to_owned()),
        }
    );
    assert_eq!(held.stops(), [unit]);
    // The mark of that stop went with its run.
    let (scratch, hook) = hook_file("stop-again");
    let params = scratch.path().join("params.toml");
    let again = launcher.run(&call(&hook, &params, unit)).unwrap();
    assert_eq!(again.code, Some(0));
}

/// Catches a stop marked on every unit, or by a name the run does not have: stopping another
/// unit while a step runs leaves the step's exit code as `systemd-run` gave it.
#[test]
fn a_stop_of_another_unit_leaves_the_running_step_its_exit_code() {
    let held = Held::holding();
    let launcher = Arc::new(SystemdLauncher::new(Spawns(Arc::clone(&held))));
    let exit = run_and_stop(
        &launcher,
        &held,
        "updater-svc-cutover-7",
        "updater-other-cutover-7",
    );
    assert_eq!(exit.code, Some(0));
    assert_eq!(held.stops(), ["updater-other-cutover-7"]);
}

/// Catches a stop made before a run (the resume stops the unit it finds running, then runs the
/// step again under the same name) carried over to that run: the new run is read by its own exit
/// code.
#[test]
fn a_stop_before_the_run_says_nothing_of_the_run() {
    let held = Arc::new(Held::default());
    let launcher = SystemdLauncher::new(Spawns(Arc::clone(&held)));
    let unit = "updater-svc-cutover-7";
    launcher.stop(unit).unwrap();
    let (scratch, hook) = hook_file("stop-before");
    let params = scratch.path().join("params.toml");
    let exit = launcher.run(&call(&hook, &params, unit)).unwrap();
    assert_eq!(
        exit,
        StepExit {
            code: Some(0),
            last_line: Some(LAST_LINE.to_owned()),
        }
    );
    assert_eq!(held.stops(), [unit]);
}

fn uninstall<'a>(executable: &'a Path, params: &'a Path, unit: &'a str) -> AdditiveCall<'a> {
    AdditiveCall {
        service: "svc",
        step: "uninstall",
        hook_args: &[],
        params,
        executable,
        unit,
        timeout_secs: 600,
    }
}

/// Catches an uninstall the dead-man or an abort stopped read as a success: the additive step
/// stopped while it runs reports no exit code, keeping its last line, as a step does; a stop of
/// another unit while it runs leaves it its exit code. Each launcher first runs the step once
/// unstopped, read by its exit code, so a launcher that runs no additive step fails here instead
/// of waiting for a run that never starts.
#[test]
fn an_additive_step_the_launcher_stopped_reports_no_exit_code() {
    let (scratch, hook) = hook_file("stop-additive");
    let params = scratch.path().join("params.toml");
    for (stop, code) in [
        ("updater-svc-uninstall-3", None),
        ("updater-other-uninstall-3", Some(0)),
    ] {
        let held = Arc::new(Held::default());
        let launcher = SystemdLauncher::new(Spawns(Arc::clone(&held)));
        let unit = "updater-svc-uninstall-3";
        let first = launcher
            .run_additive(&uninstall(&hook, &params, unit))
            .unwrap();
        assert_eq!(first.code, Some(0));
        held.state.lock().unwrap().hold_next_run = true;
        let exit = std::thread::scope(|scope| {
            let running = scope.spawn(|| launcher.run_additive(&uninstall(&hook, &params, unit)));
            held.wait_running();
            launcher.stop(stop).unwrap();
            running.join().unwrap().unwrap()
        });
        assert_eq!(
            exit,
            StepExit {
                code,
                last_line: Some(LAST_LINE.to_owned()),
            },
            "stopping {stop}"
        );
        assert_eq!(held.stops(), [stop]);
    }
}

/// Catches the stop an uninstall makes of its unit just before it runs it (a unit of that name
/// left by a daemon that died mid-uninstall) carried over to the run: the uninstall is read by its
/// own exit code, so a hook that succeeded is not reported failed.
#[test]
fn a_stop_before_an_additive_step_says_nothing_of_it() {
    let held = Arc::new(Held::default());
    let launcher = SystemdLauncher::new(Spawns(Arc::clone(&held)));
    let unit = "updater-svc-uninstall-3";
    launcher.stop(unit).unwrap();
    let (scratch, hook) = hook_file("stop-before-additive");
    let params = scratch.path().join("params.toml");
    let exit = launcher
        .run_additive(&uninstall(&hook, &params, unit))
        .unwrap();
    assert_eq!(
        exit,
        StepExit {
            code: Some(0),
            last_line: Some(LAST_LINE.to_owned()),
        }
    );
    assert_eq!(held.stops(), [unit]);
}
