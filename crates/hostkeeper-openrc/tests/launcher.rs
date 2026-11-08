//! The step launcher of an OpenRC host against real child processes (small `/bin/sh` scripts):
//! how a step, an additive step and a probe are started and read, how the bound and the end of
//! the leader take the process group down, the file kept for each running step, and every
//! refusal. Time is a clock that moves when it is read, so no test waits for a bound or asserts
//! a time.
#![cfg(target_os = "linux")]

mod launcher_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_openrc::OpenrcLauncher;
use hostkeeper_openrc::launcher::{POLL, READER_GRACE, STEPS_DIR, STOP_TIMEOUT_SECS};
use hostkeeper_steps::{
    Bound, LAST_LINE_MAX, LaunchError, ProbeExit, RING_BYTES, StepExit, StepLauncher,
};
use launcher_support::{
    PARAMS, Rig, UNIT, additive_call, gone, parse_stat, pid_in, probe_call, secs, step_call, text,
    wait_for, wait_gone,
};

/// The exit of a step that ended with `code` after printing `last_line`.
fn exit(code: Option<i32>, last_line: Option<&str>) -> StepExit {
    StepExit {
        code,
        last_line: last_line.map(str::to_owned),
    }
}

/// Runs `run` on a thread and, once the launcher has read the clock `reads` times (so it has
/// supervised the step for that many looks), creates `go`; what the step waits for. The result of
/// `run`. A run that ends before that is returned as it is.
fn run_then_release<T: Send>(
    rig: &Rig,
    go: &Path,
    reads: u64,
    run: impl FnOnce(&OpenrcLauncher) -> T + Send,
) -> T {
    std::thread::scope(|scope| {
        let running = scope.spawn(|| run(&rig.launcher));
        while !running.is_finished() && rig.clock.reads() < reads {
            std::thread::yield_now();
        }
        fs::write(go, b"").expect("release the step");
        running.join().expect("the run thread")
    })
}

#[test]
fn constants_are_the_contracts() {
    assert_eq!(STEPS_DIR, "/run/hostkeeper/steps");
    assert_eq!(STOP_TIMEOUT_SECS, 90);
    assert_eq!(READER_GRACE, Duration::from_secs(5));
    assert_eq!(POLL, Duration::from_millis(50));
    // The host's launcher builds without touching the host.
    let _ = OpenrcLauncher::system();
}

#[test]
fn a_step_exit_is_the_code_and_the_last_line() {
    let rig = Rig::new("exit-codes", 1);
    for code in [0, 75, 76, 3] {
        let hook = rig.script(
            &format!("hook{code}"),
            &format!("echo first\necho '{{\"report\":{code}}}'\nexit {code}"),
        );
        let got = rig
            .launcher
            .run(&step_call(&hook, UNIT, secs(1000)))
            .expect("the step ran");
        assert_eq!(
            got,
            exit(Some(code), Some(&format!("{{\"report\":{code}}}")))
        );
    }
}

#[test]
fn a_step_is_run_as_its_invocation_prints_it_in_a_clean_environment() {
    let rig = Rig::new("argv", 1);
    let hook = rig.script(
        "hook",
        "printf 'arg:%s\\n' \"$@\"\npwd\necho \"path:$PATH\"\necho \"home:${HOME-unset}\"\n\
         read line\necho \"stdin:[$line]\"\necho oops >&2",
    );
    let call = hostkeeper_steps::StepCall {
        service: "web",
        invocation: Invocation {
            step: Step::Prepare,
            artifact: Some(PathBuf::from("/stage/release")),
            params: Some(PathBuf::from(PARAMS)),
        },
        executable: &hook,
        unit: UNIT,
        bound: Bound::Unbounded,
    };
    rig.launcher.run(&call).expect("the step ran");
    let log = rig.log();
    assert_eq!(log.len(), 1);
    let (unit, captured) = &log[0];
    assert_eq!(unit, UNIT);
    assert_eq!(
        text(&captured.stdout),
        format!(
            "arg:prepare\narg:/stage/release\narg:--params\narg:{PARAMS}\n/\n\
             path:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
             home:unset\nstdin:[]\n"
        )
    );
    assert_eq!(text(&captured.stderr), "oops\n");
    assert_eq!(captured.code, Some(0));
}

#[test]
fn a_launcher_without_a_log_runs_steps_all_the_same() {
    let rig = Rig::new("no-log", 1);
    let hook = rig.script("hook", "echo quiet\nexit 75");
    let got = rig
        .launcher_over(Path::new("/proc"))
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(Some(75), Some("quiet")));
    assert!(rig.log().is_empty());
}

#[test]
fn a_step_ended_by_a_signal_has_no_code() {
    let rig = Rig::new("signal", 1);
    let hook = rig.script("hook", "kill -KILL $$");
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(None, None));
}

#[test]
fn output_past_the_ring_is_cut_to_its_last_bytes() {
    let rig = Rig::new("big-output", 1);
    let line = "x".repeat(999);
    let hook = rig.script(
        "hook",
        &format!(
            "i=0\nwhile [ $i -lt 1100 ]; do echo '{line}'; i=$((i+1)); done\necho END\n\
             echo err >&2"
        ),
    );
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("END")));
    let log = rig.log();
    let captured = &log[0].1;
    assert!(captured.stdout.cut());
    assert_eq!(captured.stdout.bytes().len(), RING_BYTES);
    assert!(text(&captured.stdout).ends_with("x\nEND\n"));
    assert_eq!(text(&captured.stderr), "err\n");
}

#[test]
fn a_last_line_past_the_limit_is_no_report() {
    let rig = Rig::new("long-line", 1);
    let line = "y".repeat(LAST_LINE_MAX + 1);
    let hook = rig.script("hook", &format!("echo '{line}'"));
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(Some(0), None));
}

#[test]
fn a_hook_that_cannot_start_is_refused_before_it_runs() {
    let rig = Rig::new("refusals", 1);
    let plain = rig.path("plain");
    fs::write(&plain, "#!/bin/sh\nexit 0\n").expect("write");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).expect("chmod");
    let broken = rig.script("broken", "");
    fs::write(&broken, "#!/no/such/interpreter\n").expect("write");
    let missing = rig.path("missing");
    let dir = rig.path("dir");
    fs::create_dir(&dir).expect("mkdir");

    for (executable, wants) in [
        (&missing, "No such file"),
        (&plain, "is not an executable file"),
        (&dir, "is not an executable file"),
        (&broken, "No such file"),
    ] {
        let err = rig
            .launcher
            .run(&step_call(executable, UNIT, secs(5)))
            .expect_err("the hook cannot start");
        assert!(
            matches!(&err, LaunchError::NoStart(m) if m.contains(wants)),
            "{executable:?}: {err}"
        );
    }
    assert!(rig.log().is_empty());
    assert!(!rig.steps_dir().exists());
}

#[test]
fn a_relative_path_or_a_bad_unit_name_is_the_launchers_refusal() {
    let rig = Rig::new("relative", 1);
    let hook = rig.script("hook", "exit 0");

    let relative = Path::new("hook");
    let err = rig
        .launcher
        .run(&step_call(relative, UNIT, secs(5)))
        .expect_err("a relative executable");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m == "the hook executable \"hook\" is not an absolute path"),
        "{err}"
    );

    let mut call = step_call(&hook, UNIT, secs(5));
    call.invocation.params = Some(PathBuf::from("params.toml"));
    let err = rig.launcher.run(&call).expect_err("a relative params file");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m == "the params file \"params.toml\" is not an absolute path"),
        "{err}"
    );

    for unit in ["", "a/b", ".hidden"] {
        let err = rig
            .launcher
            .run(&step_call(&hook, unit, secs(5)))
            .expect_err("a unit name that is no file name");
        assert!(
            matches!(&err, LaunchError::Launcher(m) if m.contains("is not a unit name")),
            "{unit:?}: {err}"
        );
    }

    let args: [&str; 0] = [];
    let err = rig
        .launcher
        .run_probe(&probe_call(relative, &args, 5, 10))
        .expect_err("a relative probed binary");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m == "the probed binary \"hook\" is not an absolute path"),
        "{err}"
    );
    assert!(rig.log().is_empty());
}

#[test]
fn a_step_that_runs_past_its_bound_gets_sigterm_and_has_no_code() {
    let rig = Rig::new("bound-term", 1);
    let hook = rig.script("hook", "while :; do :; done");
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(3)))
        .expect("the step ran");
    assert_eq!(got, exit(None, None));
}

#[test]
fn a_step_that_exits_zero_after_the_bounds_sigterm_still_has_no_code() {
    let rig = Rig::new("bound-term-exit", 1);
    let hook = rig.script("hook", "trap 'echo late; exit 0' TERM\nwhile :; do :; done");
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(3)))
        .expect("the step ran");
    assert_eq!(got, exit(None, Some("late")));
    assert_eq!(rig.log()[0].1.code, None);
}

#[test]
fn a_step_that_ignores_sigterm_gets_sigkill_after_the_stop_timeout() {
    // Each read of the clock is 30 seconds: the bound (1) is over at the first look, and the
    // stop timeout (90) three looks later.
    let rig = Rig::new("bound-kill", 30);
    let hook = rig.script("hook", "trap '' TERM\nwhile :; do :; done");
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1)))
        .expect("the step ran");
    assert_eq!(got, exit(None, None));
}

#[test]
fn a_step_inside_its_bound_is_not_signalled() {
    let rig = Rig::new("inside-bound", 1);
    let go = rig.path("go");
    let hook = rig.script(
        "hook",
        &format!("while [ ! -e '{}' ]; do :; done\necho done", go.display()),
    );
    let got = run_then_release(&rig, &go, 4, |launcher| {
        launcher.run(&step_call(&hook, UNIT, secs(1000)))
    })
    .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("done")));
}

#[test]
fn an_unbounded_step_is_never_ended_by_the_launcher() {
    // Each read of the clock is a million seconds: any bound would have passed at the first look.
    let rig = Rig::new("unbounded", 1_000_000);
    let go = rig.path("go");
    let hook = rig.script(
        "hook",
        &format!("while [ ! -e '{}' ]; do :; done\necho done", go.display()),
    );
    let got = run_then_release(&rig, &go, 5, |launcher| {
        launcher.run(&step_call(&hook, UNIT, Bound::Unbounded))
    })
    .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("done")));
}

#[test]
fn what_the_leader_left_in_its_process_group_is_killed_when_it_ends() {
    let rig = Rig::new("group-kill", 1);
    let pid_file = rig.path("grandchild");
    let hook = rig.script(
        "hook",
        &format!(
            "(trap '' TERM; while :; do :; done) &\necho $! > '{}'\necho leader-done",
            pid_file.display()
        ),
    );
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("leader-done")));
    wait_gone(pid_in(&pid_file));
}

/// The test binary run as a child of a step: it leaves the step's session, tells the step it is
/// there, and holds its output pipes open until it is released. Run by
/// `a_descendant_that_left_the_group_...`, never by itself.
#[test]
#[ignore = "run as a child process by a_descendant_that_left_the_group_holds_the_pipe_open_only_for_the_grace"]
fn escapee_helper() {
    let args: Vec<String> = std::env::args().collect();
    // The path of the fifo arrives as the value of `--skip`, a libtest option that takes one.
    let Some(hold) = args
        .iter()
        .position(|a| a == "--skip")
        .and_then(|at| args.get(at + 1))
    else {
        return;
    };
    let hold = PathBuf::from(hold);
    rustix::process::setsid().expect("a session of its own");
    fs::write(hold.with_extension("ready"), b"").expect("tell the step");
    // Opening a fifo for reading waits for a writer, which the test is, when it lets go.
    let _ = fs::read(&hold);
}

#[test]
fn a_descendant_that_left_the_group_holds_the_pipe_open_only_for_the_grace() {
    let rig = Rig::with("escapee", 1, |launcher| {
        launcher.with_reader_grace(Duration::ZERO)
    });
    let hold = rig.path("hold");
    hostkeeper_testkit::fifo(&hold, 0o600).expect("a fifo");
    let pid_file = rig.path("escapee");
    let exe = std::env::current_exe().expect("the test binary");
    let hook = rig.script(
        "hook",
        &format!(
            "'{}' --exact --ignored --nocapture escapee_helper --skip '{}' &\necho $! > '{}'\n\
             while [ ! -e '{}' ]; do :; done\necho leader-done",
            exe.display(),
            hold.display(),
            pid_file.display(),
            hold.with_extension("ready").display(),
        ),
    );
    let got = rig
        .launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    // The step ended, and the launcher did not wait for the pipe the escapee holds.
    assert_eq!(got.code, Some(0));
    // The group's kill did not reach it.
    let escapee = pid_in(&pid_file);
    assert!(!gone(escapee));
    // Let it go; the writer's open and close end its read.
    fs::write(&hold, b"").expect("release the escapee");
}

#[test]
fn a_file_names_the_running_step_and_is_gone_when_it_ends() {
    let rig = Rig::new("record", 1);
    let file = rig.steps_dir().join(UNIT);
    let hook = rig.script(
        "hook",
        &format!(
            "while [ ! -s '{f}' ]; do :; done\necho $$\nread stat < /proc/$$/stat\necho \"$stat\"\n\
             read record < '{f}'\necho \"$record\"",
            f = file.display()
        ),
    );
    rig.launcher
        .run(&step_call(&hook, UNIT, secs(1000)))
        .expect("the step ran");
    let log = rig.log();
    let out = text(&log[0].1.stdout);
    let mut lines = out.lines();
    let pid = lines.next().expect("the pid");
    let (_, start) = parse_stat(lines.next().expect("the stat"));
    assert_eq!(lines.next().expect("the file"), format!("{pid} {start}"));
    assert!(!file.exists());
    let mode = fs::metadata(rig.steps_dir())
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn a_step_whose_file_cannot_be_written_is_killed_and_fails_the_launcher() {
    let rig = Rig::new("record-fails", 1);
    let hook = rig.script("hook", "while :; do :; done");

    // The directory cannot be made: a file is in its place.
    fs::write(rig.steps_dir(), b"not a directory").expect("write");
    let err = rig
        .launcher
        .run(&step_call(&hook, UNIT, Bound::Unbounded))
        .expect_err("no directory");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m.contains("cannot write") && m.contains(UNIT)),
        "{err}"
    );
    fs::remove_file(rig.steps_dir()).expect("remove");

    // The file cannot be written: a directory is in its place.
    fs::create_dir_all(rig.steps_dir().join(UNIT)).expect("mkdir");
    let err = rig
        .launcher
        .run(&step_call(&hook, UNIT, Bound::Unbounded))
        .expect_err("no file");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m.contains("cannot write")),
        "{err}"
    );
    fs::remove_dir(rig.steps_dir().join(UNIT)).expect("remove");

    // The failed runs left nothing running under the name: it runs again.
    let quick = rig.script("quick", "echo fine");
    let got = rig
        .launcher
        .run(&step_call(&quick, UNIT, secs(1000)))
        .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("fine")));
    assert_eq!(rig.log().len(), 1);
}

#[test]
fn a_step_whose_process_is_not_in_the_process_table_fails_the_launcher() {
    let rig = Rig::with("no-proc", 1, |launcher| {
        launcher.with_proc_root(Path::new("/nonexistent-proc-root"))
    });
    let hook = rig.script("hook", "while :; do :; done");
    let err = rig
        .launcher
        .run(&step_call(&hook, UNIT, Bound::Unbounded))
        .expect_err("no process table");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m.contains("is not in /nonexistent-proc-root")),
        "{err}"
    );
}

#[test]
fn a_unit_that_is_running_cannot_be_run_again() {
    let rig = Rig::new("twice", 1);
    let go = rig.path("go");
    let started = rig.path("started");
    let hook = rig.script(
        "hook",
        &format!(
            ": > '{}'\nwhile [ ! -e '{}' ]; do :; done",
            started.display(),
            go.display()
        ),
    );
    let quick = rig.script("quick", "exit 0");
    std::thread::scope(|scope| {
        let first = scope.spawn(|| rig.launcher.run(&step_call(&hook, UNIT, Bound::Unbounded)));
        wait_for(&started);
        let err = rig
            .launcher
            .run(&step_call(&quick, UNIT, Bound::Unbounded))
            .expect_err("the unit is running");
        assert!(
            matches!(&err, LaunchError::Launcher(m) if m == &format!("{UNIT} is already running")),
            "{err}"
        );
        fs::write(&go, b"").expect("release");
        first.join().expect("the first run").expect("the step ran");
    });
    // Once it has ended the name is free again.
    rig.launcher
        .run(&step_call(&quick, UNIT, Bound::Unbounded))
        .expect("the step ran again");
}

#[test]
fn an_additive_step_is_run_with_the_hooks_arguments_before_its_step() {
    let rig = Rig::new("additive", 1);
    let params = rig.path("params.toml");
    let hook = rig.script("hook", "printf 'arg:%s\\n' \"$@\"\nexit 76");
    let hook_args = ["update-hook".to_owned(), "--fast".to_owned()];
    let got = rig
        .launcher
        .run_additive(&additive_call(&hook, &hook_args, &params, 100))
        .expect("the step ran");
    assert_eq!(got.code, Some(76));
    assert_eq!(
        text(&rig.log()[0].1.stdout),
        format!(
            "arg:update-hook\narg:--fast\narg:uninstall\narg:--params\narg:{}\n",
            params.display()
        )
    );
}

#[test]
fn an_additive_step_is_bounded_by_its_timeout() {
    let rig = Rig::new("additive-bound", 1);
    let params = rig.path("params.toml");
    let hook = rig.script("hook", "while :; do :; done");
    let got = rig
        .launcher
        .run_additive(&additive_call(&hook, &[], &params, 2))
        .expect("the step ran");
    assert_eq!(got, exit(None, None));
}

#[test]
fn an_additive_step_with_a_timeout_of_zero_is_not_ended_by_the_launcher() {
    // A clock that jumps a million seconds a look does not end the step.
    let rig = Rig::new("additive-unbounded", 1_000_000);
    let params = rig.path("params.toml");
    let go = rig.path("go");
    let waiting = rig.script(
        "waiting",
        &format!("while [ ! -e '{}' ]; do :; done\necho done", go.display()),
    );
    let got = run_then_release(&rig, &go, 5, |launcher| {
        launcher.run_additive(&additive_call(&waiting, &[], &params, 0))
    })
    .expect("the step ran");
    assert_eq!(got, exit(Some(0), Some("done")));
}

#[test]
fn a_probe_keeps_the_start_of_its_output_up_to_the_limit() {
    let rig = Rig::new("probe-output", 1);
    let hook = rig.script("hook", "printf 'arg:%s\\n' \"$@\"\nprintf 0123456789abcdef");
    let args = ["--version", "now"];
    let got = rig
        .launcher
        .run_probe(&probe_call(&hook, &args, 100, 24))
        .expect("the probe ran");
    assert_eq!(
        got,
        ProbeExit {
            code: Some(0),
            output: b"arg:--version\narg:now\n01".to_vec(),
            truncated: true,
        }
    );
    // Within the limit nothing is dropped.
    let got = rig
        .launcher
        .run_probe(&probe_call(&hook, &args, 100, 1000))
        .expect("the probe ran");
    assert!(!got.truncated);
    assert!(got.output.ends_with(b"0123456789abcdef"));
    let log = rig.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].0, UNIT);
}

#[test]
fn a_probe_exit_code_and_a_probe_past_its_bound() {
    let rig = Rig::new("probe-bound", 1);
    let failing = rig.script("failing", "exit 3");
    let args: [&str; 0] = [];
    let got = rig
        .launcher
        .run_probe(&probe_call(&failing, &args, 100, 10))
        .expect("the probe ran");
    assert_eq!(got.code, Some(3));

    // A timeout of 0 is read as 1 second, never as no bound.
    let forever = rig.script("forever", "echo started\nwhile :; do :; done");
    let got = rig
        .launcher
        .run_probe(&probe_call(&forever, &args, 0, 100))
        .expect("the probe ran");
    assert_eq!(got.code, None);

    // A probe that handles the bound's SIGTERM and exits 0 has no code either.
    let polite = rig.script("polite", "trap 'exit 0' TERM\nwhile :; do :; done");
    let got = rig
        .launcher
        .run_probe(&probe_call(&polite, &args, 5, 100))
        .expect("the probe ran");
    assert_eq!(got.code, None);
}
