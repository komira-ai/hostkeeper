//! The step launcher of a Mac against the `launchctl` fake and against `SimLaunchd`: the exact
//! `launchctl` calls of a run and of a stop, the job's plist, the polling until the job ends,
//! the boot-out of a job left by an earlier run, and every refusal and failure.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_hook::Step;
use hostkeeper_launchd::launchctl::{
    LaunchctlError, LaunchctlOutput, bootout_args, bootstrap_args, print_args,
};
use hostkeeper_launchd::launcher::{POLL, STDERR_FILE, STDOUT_FILE};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchdLauncher};
use hostkeeper_steps::{Bound, Captured, LaunchError, StepExit, StepLauncher};
use plist::Value;
use support::{FakePace, Hook, PARAMS, SimLaunchd, call, hook_file, scratch, secs};

const UNIT: &str = "updater-web-cutover-7";
const LABEL: &str = "dev.hostkeeper.hostkeeper.step.web.cutover.7";

const RUNNING: &str = include_str!("fixtures/print-running.txt");
const SPAWN_SCHEDULED: &str = include_str!("fixtures/print-spawn-scheduled.txt");
const WAITING_3: &str = include_str!("fixtures/print-waiting-exit-3.txt");

/// A fixture without its leading `# ` comment lines.
fn fixture(text: &str) -> String {
    text.lines()
        .skip_while(|line| line.starts_with("# "))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn label() -> Label {
    Label::parse(LABEL).expect("a label")
}

fn words(args: &[String]) -> Vec<String> {
    args.to_vec()
}

/// The exit and the log of the steps a launcher ran.
type Logged = Arc<Mutex<Vec<(String, Captured)>>>;

/// A launcher over `SimLaunchd` with a fresh pace, its jobs in `jobs`, logging to the result.
fn sim_launcher(jobs: &Path) -> (LaunchdLauncher, SimLaunchd, FakePace, Logged) {
    let pace = FakePace::default();
    let sim = SimLaunchd::new(&pace);
    let logged = Logged::default();
    let sink = logged.clone();
    let launcher = LaunchdLauncher::new(sim.clone(), pace.clone(), jobs).with_log(Arc::new(
        move |unit: &str, captured: &Captured| {
            sink.lock()
                .expect("the log")
                .push((unit.to_owned(), captured.clone()))
        },
    ));
    (launcher, sim, pace, logged)
}

fn launcher_error(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    match result {
        Err(LaunchError::Launcher(why)) => why,
        other => panic!("expected a launcher error, got {other:?}"),
    }
}

fn no_start(result: Result<StepExit, LaunchError>) -> String {
    match result {
        Err(LaunchError::NoStart(why)) => why,
        other => panic!("expected the hook not to start, got {other:?}"),
    }
}

/// Catches a call added, dropped, reordered or misspelt in a run: a job left from an earlier run
/// is looked for, the plist under the job's directory is bootstrapped, the job is read (spawn
/// scheduled, running, then exited 3) with one wait of [`POLL`] between two reads, and the
/// ended job is booted out. The exit code read from `last exit code` is the step's; the job's
/// directory is gone afterwards.
#[test]
fn a_run_makes_exactly_these_launchctl_calls() {
    let root = scratch("launcher-calls");
    let hook = hook_file(root.path());
    let jobs = root.path().join("jobs");
    let fake = FakeLaunchctl::new();
    let print = print_args(&label());
    let plist = jobs.join(LABEL).join(format!("{LABEL}.plist"));
    fake.then(&print, Ok(LaunchctlOutput::not_loaded()));
    fake.then(&bootstrap_args(&plist), Ok(LaunchctlOutput::success("")));
    for answer in [SPAWN_SCHEDULED, RUNNING, WAITING_3] {
        fake.then(&print, Ok(LaunchctlOutput::success(&fixture(answer))));
    }
    fake.then(&bootout_args(&label()), Ok(LaunchctlOutput::success("")));
    let pace = FakePace::default();
    let launcher = LaunchdLauncher::new(fake.clone(), pace.clone(), &jobs);

    let exit = launcher.run(&call(&hook, UNIT, Step::Cutover, secs(600)));

    assert_eq!(
        exit.expect("the step ran"),
        StepExit {
            code: Some(3),
            last_line: None
        }
    );
    let expected: Vec<Vec<String>> = vec![
        vec!["print".into(), format!("system/{LABEL}")],
        vec![
            "bootstrap".into(),
            "system".into(),
            plist.to_str().expect("UTF-8").to_owned(),
        ],
        vec!["print".into(), format!("system/{LABEL}")],
        vec!["print".into(), format!("system/{LABEL}")],
        vec!["print".into(), format!("system/{LABEL}")],
        vec!["bootout".into(), format!("system/{LABEL}")],
    ];
    assert_eq!(fake.calls(), expected);
    assert_eq!(pace.sleeps(), [POLL, POLL]);
    assert!(!jobs.join(LABEL).exists(), "the job's directory is removed");
}

/// Catches a job that would not run the hook as asked, or would run it as another user, with
/// rights, or kept alive: the plist launchd is given decodes to exactly the label of the unit,
/// the executable with the step's arguments, `RunAtLoad`, the process group not abandoned, and
/// the two output files in the job's directory, and to nothing else (no `UserName`, no
/// `KeepAlive`, no sandbox).
#[test]
fn the_job_runs_the_hook_once_as_root_with_its_output_in_its_directory() {
    let root = scratch("launcher-plist");
    let hook = hook_file(root.path());
    let jobs = root.path().join("jobs");
    let (launcher, sim, _, _) = sim_launcher(&jobs);
    sim.script(LABEL, Hook::exits(1, 0, "{}"));

    launcher
        .run(&call(&hook, UNIT, Step::Cutover, secs(60)))
        .expect("the step ran");

    let dir = jobs.join(LABEL);
    let path = |p: PathBuf| Value::String(p.to_str().expect("UTF-8").to_owned());
    let expected: plist::Dictionary = [
        ("Label", Value::String(LABEL.to_owned())),
        (
            "ProgramArguments",
            Value::Array(vec![
                path(hook.clone()),
                Value::String("cutover".to_owned()),
                Value::String("--params".to_owned()),
                Value::String(PARAMS.to_owned()),
            ]),
        ),
        ("RunAtLoad", Value::Boolean(true)),
        ("AbandonProcessGroup", Value::Boolean(false)),
        ("StandardOutPath", path(dir.join(STDOUT_FILE))),
        ("StandardErrorPath", path(dir.join(STDERR_FILE))),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    assert_eq!(sim.plists(), [expected]);
}

/// Catches a step whose end is misread: a job that exited is its code, one killed by a signal
/// (`last terminating signal`) has no code, and so has one that shows both a last exit code and a
/// terminating signal (the code is of an earlier run).
#[test]
fn a_job_ended_by_a_signal_has_no_exit_code() {
    let root = scratch("launcher-signal");
    let hook = hook_file(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    let mut killed = Hook::exits(2, 0, "never read");
    killed.code = None;
    sim.script(LABEL, killed);
    let exit = launcher
        .run(&call(&hook, UNIT, Step::Verify, secs(60)))
        .expect("the step ran");
    assert_eq!(exit.code, None);

    let jobs = root.path().join("jobs-fake");
    let fake = FakeLaunchctl::new();
    let print = print_args(&label());
    fake.then(&print, Ok(LaunchctlOutput::not_loaded()));
    fake.always(
        &bootstrap_args(&jobs.join(LABEL).join(format!("{LABEL}.plist"))),
        Ok(LaunchctlOutput::success("")),
    );
    fake.then(
        &print,
        Ok(LaunchctlOutput::success(&format!(
            "system/{LABEL} = {{\n\tstate = not running\n\tlast exit code = 0\n\
             \tlast terminating signal = Terminated: 15\n}}\n"
        ))),
    );
    fake.always(&bootout_args(&label()), Ok(LaunchctlOutput::success("")));
    let launcher = LaunchdLauncher::new(fake, FakePace::default(), &jobs);
    let exit = launcher
        .run(&call(&hook, UNIT, Step::Verify, secs(60)))
        .expect("the step ran");
    assert_eq!(exit.code, None);
}

/// Catches a run that cannot load its job because an earlier run of the same unit left it
/// loaded (a crash between the end of the step and its unload): that job is booted out first,
/// and the new one is loaded and read.
#[test]
fn a_job_left_loaded_by_an_earlier_run_is_booted_out_first() {
    let root = scratch("launcher-stale");
    let hook = hook_file(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    sim.running(LABEL);
    sim.script(LABEL, Hook::exits(3, 0, "{\"fresh\":true}"));

    let exit = launcher
        .run(&call(&hook, UNIT, Step::Cleanup, secs(60)))
        .expect("the step ran");

    assert_eq!(exit.code, Some(0));
    assert_eq!(exit.last_line.as_deref(), Some("{\"fresh\":true}"));
    let calls = sim.calls();
    assert_eq!(calls[0], words(&print_args(&label())));
    assert_eq!(calls[1], words(&bootout_args(&label())));
    assert_eq!(calls[2][0], "bootstrap");
    assert!(!sim.is_loaded(LABEL), "the ended job is unloaded");
}

/// Catches output of an earlier run read as this run's: launchd appends to the output files, so
/// files left in the job's directory are emptied before the job is loaded.
#[test]
fn output_left_by_an_earlier_run_is_not_read() {
    let root = scratch("launcher-leftover");
    let hook = hook_file(root.path());
    let jobs = root.path().join("jobs");
    let (launcher, sim, _, logged) = sim_launcher(&jobs);
    fs::create_dir_all(jobs.join(LABEL)).expect("the job's directory");
    fs::write(
        jobs.join(LABEL).join(STDOUT_FILE),
        "{\"stale\":true}\npartial",
    )
    .expect("write");
    fs::write(jobs.join(LABEL).join(STDERR_FILE), "stale log\n").expect("write");
    let mut hook_run = Hook::exits(1, 0, "");
    hook_run.stdout = b"no line end".to_vec();
    hook_run.stderr = b"log\n".to_vec();
    sim.script(LABEL, hook_run);

    let exit = launcher
        .run(&call(&hook, UNIT, Step::Cleanup, secs(60)))
        .expect("the step ran");

    assert_eq!(exit.last_line.as_deref(), Some("no line end"));
    let logged = logged.lock().expect("the log");
    assert_eq!(logged[0].1.stdout.bytes(), b"no line end");
    assert_eq!(logged[0].1.stderr.bytes(), b"log\n");
}

/// Catches a step whose known end is turned into a launcher failure because its output file
/// could not be read: nothing is kept of it, the report is then none, and the exit code stands.
#[test]
fn an_output_file_that_cannot_be_read_keeps_nothing() {
    let root = scratch("launcher-lost");
    let hook = hook_file(root.path());
    let (launcher, sim, _, logged) = sim_launcher(&root.path().join("jobs"));
    let mut lost = Hook::exits(1, 75, "{\"class\":\"environment\"}");
    lost.lose_stdout = true;
    sim.script(LABEL, lost);

    let exit = launcher
        .run(&call(&hook, UNIT, Step::Prepare, Bound::Unbounded))
        .expect("the step ran");

    assert_eq!(
        exit,
        StepExit {
            code: Some(75),
            last_line: None
        }
    );
    assert!(
        logged.lock().expect("the log")[0]
            .1
            .stdout
            .bytes()
            .is_empty()
    );
}

/// Catches a bounded step that the launcher stops late, early, or between two reads it does not
/// make: with a read every [`POLL`], a 30 s bound boots the job out at exactly 30 s, after one
/// read more than the waits, and the step has no code. A step that ends before its bound is not
/// booted out while it runs.
#[test]
fn a_bounded_step_is_booted_out_at_the_first_read_past_its_bound() {
    let root = scratch("launcher-bound");
    let hook = hook_file(root.path());
    let (launcher, sim, pace, _) = sim_launcher(&root.path().join("jobs"));
    sim.script(LABEL, Hook::exits(3600, 0, "{}"));

    let exit = launcher
        .run(&call(&hook, UNIT, Step::Cutover, secs(30)))
        .expect("the step ran");

    assert_eq!(exit.code, None);
    assert_eq!(
        sim.stopped_after(),
        [(LABEL.to_owned(), Duration::from_secs(30))]
    );
    assert_eq!(pace.sleeps().len(), 60);
    assert!(pace.sleeps().iter().all(|wait| *wait == POLL));
    let prints = sim.calls().iter().filter(|c| c[0] == "print").count();
    // The look for a left job, 61 reads, and the print of the boot-out.
    assert_eq!(prints, 63);

    sim.script(LABEL, Hook::exits(29, 0, "{}"));
    let exit = launcher
        .run(&call(&hook, UNIT, Step::Cutover, secs(30)))
        .expect("the step ran");
    assert_eq!(exit.code, Some(0));
    assert_eq!(sim.stopped_after().len(), 1);
}

/// Catches a call the launcher should refuse before it touches launchd: a relative executable or
/// params file and a unit name that is not a step unit are launcher errors; a missing hook, a
/// directory, and a file that cannot be executed are a hook that did not start; a path the plist
/// cannot hold (a control character) is a launcher error. None of them runs `launchctl`.
#[test]
fn a_bad_call_is_refused_before_any_launchctl_call() {
    let root = scratch("launcher-refuse");
    let hook = hook_file(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));

    let relative = Path::new("bin/hook");
    assert_eq!(
        launcher_error(launcher.run(&call(relative, UNIT, Step::Cutover, secs(5)))),
        "the hook executable \"bin/hook\" is not an absolute path"
    );
    let mut relative_params = call(&hook, UNIT, Step::Cutover, secs(5));
    relative_params.invocation.params = Some(PathBuf::from("params.toml"));
    assert_eq!(
        launcher_error(launcher.run(&relative_params)),
        "the params file \"params.toml\" is not an absolute path"
    );
    assert_eq!(
        launcher_error(launcher.run(&call(&hook, "web.service", Step::Cutover, secs(5)))),
        "\"web.service\" is not the name of a step unit (updater-<service>-<step>-<generation>)"
    );

    let missing = root.path().join("missing");
    assert_eq!(
        no_start(launcher.run(&call(&missing, UNIT, Step::Cutover, secs(5)))),
        format!(
            "{}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    assert_eq!(
        no_start(launcher.run(&call(root.path(), UNIT, Step::Cutover, secs(5)))),
        format!("{} is not an executable file", root.path().display())
    );
    let plain = root.path().join("plain");
    fs::write(&plain, "data").expect("write");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(
        no_start(launcher.run(&call(&plain, UNIT, Step::Cutover, secs(5)))),
        format!("{} is not an executable file", plain.display())
    );

    let bell = root.path().join("hook\u{7}");
    fs::copy(&hook, &bell).expect("copy the hook");
    assert_eq!(
        launcher_error(launcher.run(&call(&bell, UNIT, Step::Cutover, secs(5)))),
        format!(
            "plist text {:?} holds a control character",
            bell.to_str().expect("UTF-8")
        )
    );
    assert!(
        sim.calls().is_empty(),
        "no launchctl call: {:?}",
        sim.calls()
    );
}

/// Catches a failure of launchd or of the job's files read as a step's end: each is a launcher
/// error naming the job (the runner then leaves the step running and resume stops it by name).
/// The look for a left job that cannot run, a job directory that cannot be written, a bootstrap
/// that fails or cannot run, a read of the running job that fails, and a boot-out at the bound
/// that fails while the job is still loaded. A job that was loaded is booted out once more and
/// its directory removed before the error is returned.
#[test]
fn a_failure_of_launchd_is_a_launcher_error() {
    let root = scratch("launcher-fail");
    let hook = hook_file(root.path());
    let jobs = root.path().join("jobs");
    let (launcher, sim, _, _) = sim_launcher(&jobs);
    let print = ["print", &format!("system/{LABEL}")].map(str::to_owned);
    let print = [print[0].as_str(), print[1].as_str()];
    let bootout = ["bootout", &format!("system/{LABEL}")].map(str::to_owned);
    let bootout = [bootout[0].as_str(), bootout[1].as_str()];
    let plist = jobs.join(LABEL).join(format!("{LABEL}.plist"));
    let bootstrap = ["bootstrap", "system", plist.to_str().expect("UTF-8")];
    let spawn = |args: &str| LaunchctlError::Spawn {
        args: args.to_owned(),
        why: "gone".to_owned(),
    };
    let run = || launcher.run(&call(&hook, UNIT, Step::Cutover, secs(5)));

    sim.fail_next(&print, Err(spawn("print")));
    assert_eq!(
        launcher_error(run()),
        format!("cannot stop the job {LABEL}: cannot run launchctl print: gone")
    );

    sim.fail_next(
        &bootstrap,
        Ok(LaunchctlOutput::exit(5, "Bootstrap failed: 5\n")),
    );
    assert_eq!(
        launcher_error(run()),
        format!("launchctl bootstrap of the job {LABEL} exited with Some(5): Bootstrap failed: 5")
    );
    sim.fail_next(&bootstrap, Err(spawn("bootstrap")));
    assert_eq!(
        launcher_error(run()),
        format!("cannot load the job {LABEL}: cannot run launchctl bootstrap: gone")
    );

    sim.script(LABEL, Hook::exits(60, 0, "{}"));
    sim.fail_next(&print, Ok(LaunchctlOutput::not_loaded()));
    sim.fail_next(&print, Ok(LaunchctlOutput::exit(1, "denied")));
    assert_eq!(
        launcher_error(run()),
        format!("cannot read the job {LABEL}: launchctl print exited with Some(1): denied")
    );
    assert!(
        !sim.is_loaded(LABEL),
        "a job that could not be read is booted out"
    );
    assert!(!jobs.join(LABEL).exists(), "and its directory removed");
    launcher.stop(UNIT).expect("stopped");

    sim.script(LABEL, Hook::exits(60, 0, "{}"));
    sim.fail_next(&bootout, Ok(LaunchctlOutput::exit(1, "busy")));
    assert_eq!(
        launcher_error(run()),
        format!("cannot stop the job {LABEL}: launchctl bootout exited with Some(1): busy")
    );
    assert!(
        !sim.is_loaded(LABEL),
        "a job whose boot-out at the bound failed is booted out once more"
    );
    launcher.stop(UNIT).expect("stopped");

    let file_jobs = root.path().join("not-a-dir");
    fs::write(&file_jobs, "").expect("write");
    let (launcher, _, _, _) = sim_launcher(&file_jobs);
    let why = launcher_error(launcher.run(&call(&hook, UNIT, Step::Cutover, secs(5))));
    assert_eq!(
        why,
        format!(
            "cannot write the job {}: Not a directory (os error 20)",
            file_jobs.join(LABEL).display()
        )
    );
}

/// Catches a stop that is not idempotent or names the wrong job: a running job is booted out
/// (print, then bootout); a job that is not loaded is stopped already (print only); a boot-out
/// that fails while the job stays loaded is a launcher error; a unit that is not a step unit is
/// refused before any call.
#[test]
fn stop_boots_out_the_job_and_a_job_already_gone_is_stopped() {
    let root = scratch("launcher-stop");
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    sim.running(LABEL);
    launcher.stop(UNIT).expect("stopped");
    launcher.stop(UNIT).expect("stopped again");
    assert_eq!(
        sim.calls(),
        [
            words(&print_args(&label())),
            words(&bootout_args(&label())),
            words(&print_args(&label())),
        ]
    );

    sim.running(LABEL);
    let bootout = format!("system/{LABEL}");
    sim.fail_next(&["bootout", &bootout], Ok(LaunchctlOutput::exit(1, "busy")));
    assert_eq!(
        launcher_error(launcher.stop(UNIT)),
        format!("cannot stop the job {LABEL}: launchctl bootout exited with Some(1): busy")
    );
    let before = sim.calls().len();
    assert_eq!(
        launcher_error(launcher.stop("not-a-unit")),
        "\"not-a-unit\" is not the name of a step unit (updater-<service>-<step>-<generation>)"
    );
    assert_eq!(sim.calls().len(), before);
}

/// Catches a run that misreads a step the dead-man stopped from its own thread: while `run`
/// reads an unbounded job, `stop` boots it out; `run` then finds the label no longer loaded and
/// reports the step ended with no code, a failed step, and `stop` is `Ok`.
#[test]
fn a_step_stopped_from_another_thread_while_it_runs_has_no_exit_code() {
    let root = scratch("launcher-stopped");
    let hook = hook_file(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    sim.script(LABEL, Hook::exits(u64::MAX, 0, "{}"));
    let exit = std::thread::scope(|scope| {
        let running =
            scope.spawn(|| launcher.run(&call(&hook, UNIT, Step::Prepare, Bound::Unbounded)));
        // A run that ends before its job is loaded is joined, and fails below, never waited on.
        while !sim.is_loaded(LABEL) && !running.is_finished() {
            std::thread::yield_now();
        }
        launcher.stop(UNIT).expect("stopped");
        running.join().expect("the run thread")
    });
    assert_eq!(exit.expect("the step ran").code, None);
    assert!(!sim.is_loaded(LABEL));
}

/// Catches a Mac launcher that loses a deferral: a `prepare` job that ends in exit 75 with the
/// `leases_live` line (the testkit's `defer_leases_live` hook) reports exactly that code and that
/// last line, as a systemd unit does, and the hook contract reads them as a deferral of class
/// `environment`, reason `leases_live`, retry 60 seconds; never a failure.
#[test]
fn a_launchd_job_that_ends_in_exit_75_reports_deferred_through_the_launcher() {
    const PREPARE_UNIT: &str = "updater-web-prepare-7";
    const PREPARE_LABEL: &str = "dev.hostkeeper.hostkeeper.step.web.prepare.7";
    let mode = hostkeeper_testkit::HookMode::DeferLeasesLive;
    let root = scratch("launcher-exit-75");
    let hook = hook_file(root.path());
    let (launcher, sim, _, logged) = sim_launcher(&root.path().join("jobs"));
    sim.script(
        PREPARE_LABEL,
        Hook::exits(2, mode.exit_code(), mode.last_line()),
    );

    let exit = launcher
        .run(&call(&hook, PREPARE_UNIT, Step::Prepare, secs(60)))
        .expect("the step ran");

    assert_eq!(
        exit,
        StepExit {
            code: Some(75),
            last_line: Some(mode.last_line().to_owned())
        }
    );
    assert_eq!(
        hostkeeper_hook::Outcome::from_exit(exit.code),
        hostkeeper_hook::Outcome::Deferred
    );
    let report = hostkeeper_hook::Report::read(exit.last_line.as_deref().unwrap_or_default());
    assert_eq!(report.class, hostkeeper_hook::Class::Environment);
    assert_eq!(
        report.reason.as_deref(),
        Some(hostkeeper_hook::REASON_LEASES_LIVE)
    );
    assert_eq!(report.retry_after_secs, Some(60));
    let logged = logged.lock().expect("the log");
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, PREPARE_UNIT);
    assert!(!sim.is_loaded(PREPARE_LABEL), "the ended job is booted out");
}
