//! The launcher of the host against a spawner that records what it is asked to run: the command
//! line of a step, the unit name, the refusals, `NoStart` and `Launcher`, and the stop.

mod support;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_steps::systemd::{EXIT_NOT_LOADED, SYSTEMCTL, SYSTEMD_RUN, command, unit_name};
use hostkeeper_steps::{
    Bound, Captured, LaunchError, Program, Ring, Spawner, StepCall, StepExit, StepLauncher,
    SystemdLauncher, SystemdRunProcess,
};
use support::{Scratch, secs};

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

fn invocation(step: Step, params: &str) -> Invocation {
    Invocation {
        step,
        artifact: (step == Step::Prepare).then(|| PathBuf::from("/var/lib/releases/svc/abc")),
        params: Some(PathBuf::from(params)),
    }
}

fn call<'a>(executable: &'a Path, unit: &'a str, step: Step, bound: Bound) -> StepCall<'a> {
    StepCall {
        service: "svc",
        hook_args: &[],
        invocation: invocation(step, "/var/lib/state/svc/params.toml"),
        executable,
        unit,
        bound,
    }
}

fn args(call: &StepCall<'_>) -> Vec<String> {
    command(call)
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

/// A scratch directory with an executable file `hook` in it.
fn hook_file(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let hook = scratch.path().join("hook");
    std::fs::write(&hook, "#!hook\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    (scratch, hook)
}

/// Catches a flag of the step's line added, dropped or moved: a bounded `cutover` is exactly the
/// documented line.
#[test]
fn the_command_is_exactly_the_documented_line() {
    let exe = Path::new("/var/lib/releases/svc/abc/hook");
    let unit = unit_name("svc", Step::Cutover, 3);
    let got = args(&call(exe, &unit, Step::Cutover, secs(600)));
    assert_eq!(
        got,
        [
            "--system",
            "--quiet",
            "--wait",
            "--collect",
            "--pipe",
            "--unit=updater-svc-cutover-3",
            "--property=RuntimeMaxSec=600",
            "--",
            "/var/lib/releases/svc/abc/hook",
            "cutover",
            "--params",
            "/var/lib/state/svc/params.toml",
        ]
    );
    // `prepare` is given the staged release after its step name.
    let unit = unit_name("svc", Step::Prepare, 3);
    let got = args(&call(exe, &unit, Step::Prepare, Bound::Unbounded));
    assert_eq!(
        got[7..],
        [
            "--",
            "/var/lib/releases/svc/abc/hook",
            "prepare",
            "/var/lib/releases/svc/abc",
            "--params",
            "/var/lib/state/svc/params.toml",
        ]
    );
}

/// The hook command's leading arguments come between the executable and the step, for a step
/// that carries the staged release too; with none, the step follows the executable at once.
#[test]
fn the_hook_args_are_given_before_the_step() {
    let exe = Path::new("/var/lib/releases/svc/abc/hook");
    let hook_args = ["update-hook".to_owned(), "--fast".to_owned()];
    let unit = unit_name("svc", Step::Prepare, 3);
    let mut with = call(exe, &unit, Step::Prepare, Bound::Unbounded);
    with.hook_args = &hook_args;
    assert_eq!(
        args(&with)[7..],
        [
            "--",
            "/var/lib/releases/svc/abc/hook",
            "update-hook",
            "--fast",
            "prepare",
            "/var/lib/releases/svc/abc",
            "--params",
            "/var/lib/state/svc/params.toml",
        ]
    );
    let unit = unit_name("svc", Step::Verify, 4);
    let mut with = call(exe, &unit, Step::Verify, secs(30));
    with.hook_args = &hook_args[..1];
    assert_eq!(
        args(&with)[7..],
        [
            "--",
            "/var/lib/releases/svc/abc/hook",
            "update-hook",
            "verify",
            "--params",
            "/var/lib/state/svc/params.toml",
        ]
    );
    let without = call(exe, &unit, Step::Verify, secs(30));
    assert_eq!(args(&without)[9], "verify");
}

/// Catches any confinement coming back: for every step, bounded and unbounded, there is exactly
/// one property, `RuntimeMaxSec=`, and no argument names a user, a group, a capability, a sandbox
/// property, a path list, an environment or a system call filter.
#[test]
fn the_only_property_is_runtime_max_sec() {
    let exe = Path::new("/var/lib/releases/svc/abc/hook");
    let banned = [
        "User",
        "Group",
        "Capabilit",
        "ProtectSystem",
        "ReadWritePaths",
        "ReadOnlyPaths",
        "Environment",
        "SystemCallFilter",
    ];
    for step in Step::ALL {
        for bound in [secs(600), Bound::Unbounded] {
            let unit = unit_name("svc", step, 1);
            let got = args(&call(exe, &unit, step, bound));
            let properties: Vec<&String> = got
                .iter()
                .filter(|a| a.starts_with("--property="))
                .collect();
            assert_eq!(properties.len(), 1, "{step} {bound:?}: {got:?}");
            assert!(
                properties[0].starts_with("--property=RuntimeMaxSec="),
                "{step} {bound:?}: {got:?}"
            );
            for arg in &got {
                for word in banned {
                    assert!(!arg.contains(word), "{step} {bound:?}: {arg}");
                }
            }
        }
    }
}

fn runtime_max_sec(bound: Bound) -> String {
    let unit = unit_name("svc", Step::Prepare, 1);
    let exe = Path::new("/r/hook");
    args(&call(exe, &unit, Step::Prepare, bound))
        .into_iter()
        .find_map(|a| {
            a.strip_prefix("--property=RuntimeMaxSec=")
                .map(str::to_owned)
        })
        .unwrap()
}

/// Catches a bound written as anything but its seconds.
#[test]
fn a_bounded_step_sets_runtime_max_sec_to_its_timeout() {
    assert_eq!(runtime_max_sec(secs(1)), "1");
    assert_eq!(runtime_max_sec(secs(600)), "600");
    assert_eq!(runtime_max_sec(secs(86_400)), "86400");
}

/// Catches no bound written as `0`, which systemd reads as no limit only by an accident of its
/// version: the design names `infinity`.
#[test]
fn an_unbounded_prepare_sets_runtime_max_sec_infinity() {
    assert_eq!(runtime_max_sec(Bound::Unbounded), "infinity");
}

/// Catches a unit name a resume cannot find: generation 7 of each step is
/// `updater-svc-<step>-7`, the same every time, and it is the `--unit=` of the line.
#[test]
fn the_unit_name_carries_service_step_and_generation() {
    for step in Step::ALL {
        let name = unit_name("svc", step, 7);
        assert_eq!(name, format!("updater-svc-{step}-7"));
        assert_eq!(unit_name("svc", step, 7), name);
        let got = args(&call(Path::new("/r/hook"), &name, step, secs(5)));
        assert_eq!(got[5], format!("--unit={name}"));
    }
}

/// Catches a step started from a relative path, which the unit would resolve from `/`: a
/// relative executable or params file is a launcher error and nothing is spawned.
#[test]
fn a_relative_executable_or_params_path_is_refused() {
    let unit = unit_name("svc", Step::Verify, 1);
    let relative_exe = call(Path::new("hook"), &unit, Step::Verify, secs(5));
    let mut relative_params = call(Path::new("/r/hook"), &unit, Step::Verify, secs(5));
    relative_params.invocation = invocation(Step::Verify, "params.toml");
    let want = [
        r#"the launcher failed: the hook executable "hook" is not an absolute path"#,
        r#"the launcher failed: the params file "params.toml" is not an absolute path"#,
    ];
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);
    for (call, want) in [(relative_exe, want[0]), (relative_params, want[1])] {
        let err = command(&call).unwrap_err();
        assert!(matches!(err, LaunchError::Launcher(_)), "{err:?}");
        assert_eq!(err.to_string(), want);
        let err = launcher.run(&call).unwrap_err();
        assert_eq!(err.to_string(), want);
    }
    assert!(recorder.calls().is_empty());
}

/// Catches the two failures being confused: a hook that is missing, not executable or not a
/// file is `NoStart` (the runner may then try the previous release's `rollback`) and nothing is
/// spawned; `systemd-run` that cannot be run is `Launcher` (the step stays `running`).
#[test]
fn a_missing_hook_executable_is_no_start_and_a_failed_spawn_is_a_launcher_error() {
    let (scratch, hook) = hook_file("sd-nostart");
    let unit = unit_name("svc", Step::Rollback, 2);
    let plain = scratch.path().join("plain");
    std::fs::write(&plain, "data").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let missing = scratch.path().join("missing");
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);
    for (path, why) in [
        (
            missing.clone(),
            "No such file or directory (os error 2)".to_owned(),
        ),
        (plain.clone(), String::new()),
        (scratch.path().to_path_buf(), String::new()),
    ] {
        let err = launcher
            .run(&call(&path, &unit, Step::Rollback, secs(5)))
            .unwrap_err();
        let want = if why.is_empty() {
            format!("{} is not an executable file", path.display())
        } else {
            format!("{}: {why}", path.display())
        };
        assert!(
            matches!(&err, LaunchError::NoStart(text) if *text == want),
            "{err:?}"
        );
    }
    assert!(recorder.calls().is_empty());

    let recorder = Recorder::answering(vec![Err(io::Error::other("no such program"))]);
    let err = SystemdLauncher::new(&recorder)
        .run(&call(&hook, &unit, Step::Rollback, secs(5)))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text) if text == "SystemdRun could not be run: no such program"),
        "{err:?}"
    );
    assert_eq!(recorder.calls().len(), 1);
}

/// Catches the exit of `systemd-run` or the last line not reaching the runner, the line of the
/// step not being what is spawned, or the log not being given what was captured.
#[test]
fn the_exit_and_last_line_of_systemd_run_are_the_step_exit() {
    let (_scratch, hook) = hook_file("sd-exit");
    let unit = unit_name("svc", Step::Verify, 4);
    let out = b"checking\n{\"retry_after_secs\":30}\n";
    let recorder = Recorder::answering(vec![
        Ok(captured(Some(75), out, b"a warning\n")),
        Ok(captured(None, b"", b"")),
    ]);
    let logged: Arc<Mutex<Vec<(String, Captured)>>> = Arc::default();
    let log = logged.clone();
    let launcher =
        SystemdLauncher::new(&recorder).with_log(Arc::new(move |unit: &str, got: &Captured| {
            log.lock().unwrap().push((unit.to_owned(), got.clone()));
        }));
    let step = call(&hook, &unit, Step::Verify, secs(30));
    assert_eq!(
        launcher.run(&step).unwrap(),
        StepExit {
            code: Some(75),
            last_line: Some("{\"retry_after_secs\":30}".to_owned()),
        }
    );
    assert_eq!(
        launcher.run(&step).unwrap(),
        StepExit {
            code: None,
            last_line: None,
        }
    );
    let want: Vec<String> = command(&step)
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    assert_eq!(
        recorder.calls(),
        [
            (Program::SystemdRun, want.clone()),
            (Program::SystemdRun, want)
        ]
    );
    let logged = logged.lock().unwrap();
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[0].0, unit);
    assert_eq!(logged[0].1.stderr.bytes(), b"a warning\n");
    assert_eq!(logged[0].1.stdout.bytes(), out);
}

/// Catches a launcher built without a log losing the step's exit: with no log, the exit code and
/// last line of `systemd-run` are still the step's exit.
#[test]
fn a_launcher_with_no_log_still_gives_the_step_exit() {
    let (_scratch, hook) = hook_file("sd-no-log");
    let unit = unit_name("svc", Step::Cleanup, 2);
    let recorder =
        Recorder::answering(vec![Ok(captured(Some(0), b"{\"reason\":\"tidy\"}\n", b""))]);
    let launcher = SystemdLauncher::new(&recorder);
    assert_eq!(
        launcher
            .run(&call(&hook, &unit, Step::Cleanup, secs(60)))
            .unwrap(),
        StepExit {
            code: Some(0),
            last_line: Some("{\"reason\":\"tidy\"}".to_owned()),
        }
    );
    assert_eq!(recorder.calls().len(), 1);
}

/// Catches a stop that names another unit, waits on nothing, or reads a unit that is not loaded
/// as a failure: `systemctl stop --system <unit>`, and exit `0` and `5` are both stopped.
#[test]
fn stop_names_the_unit_and_treats_an_unloaded_unit_as_stopped() {
    assert_eq!(EXIT_NOT_LOADED, 5);
    let recorder = Recorder::answering(vec![
        Ok(captured(Some(0), b"", b"")),
        Ok(captured(
            Some(EXIT_NOT_LOADED),
            b"",
            b"Failed to stop updater-svc-verify-9.service: Unit updater-svc-verify-9.service not loaded.\n",
        )),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    launcher.stop("updater-svc-cutover-3").unwrap();
    launcher.stop("updater-svc-verify-9").unwrap();
    let stop = |unit: &str| {
        (
            Program::Systemctl,
            vec!["stop".to_owned(), "--system".to_owned(), unit.to_owned()],
        )
    };
    assert_eq!(
        recorder.calls(),
        [stop("updater-svc-cutover-3"), stop("updater-svc-verify-9")]
    );
}

/// Catches a failed stop being swallowed: any other exit status, a `systemctl` killed by a
/// signal, and a `systemctl` that cannot be run are launcher errors that carry what it said.
#[test]
fn stop_reports_any_other_failure() {
    let recorder = Recorder::answering(vec![
        Ok(captured(
            Some(1),
            b"",
            b"Failed to stop updater-svc-cleanup-2.service: Access denied\n",
        )),
        Ok(captured(None, b"", b"")),
        Err(io::Error::other("no such program")),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    let errors: Vec<String> = (0..3)
        .map(
            |_| match launcher.stop("updater-svc-cleanup-2").unwrap_err() {
                LaunchError::Launcher(text) => text,
                other => panic!("{other:?}"),
            },
        )
        .collect();
    assert_eq!(
        errors,
        [
            "systemctl stop updater-svc-cleanup-2 exited with Some(1): Failed to stop updater-svc-cleanup-2.service: Access denied",
            "systemctl stop updater-svc-cleanup-2 exited with None: ",
            "Systemctl could not be run: no such program",
        ]
    );
    assert_eq!(recorder.calls().len(), 3);
}

/// Catches the host launcher running other programs than the host's `systemd-run` and
/// `systemctl`.
#[test]
fn the_host_launcher_runs_the_host_programs() {
    let host = SystemdRunProcess::system();
    assert_eq!(host.program(Program::SystemdRun), Path::new(SYSTEMD_RUN));
    assert_eq!(host.program(Program::Systemctl), Path::new(SYSTEMCTL));
    assert_eq!(SYSTEMD_RUN, "/usr/bin/systemd-run");
    assert_eq!(SYSTEMCTL, "/usr/bin/systemctl");
    // Built without starting anything.
    let _launcher: SystemdLauncher<SystemdRunProcess> = SystemdLauncher::system();
}
