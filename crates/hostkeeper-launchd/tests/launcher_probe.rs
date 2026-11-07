//! The probe of the step launcher of a Mac (`StepLauncher::run_probe`: the daemon asking a new
//! hostkeeper binary for its self-update protocol) against the `launchctl` fake and against
//! `SimLaunchd`: the exact `launchctl` calls and plist of a probe, its label apart from every step
//! label, its bound, how its exit and output are read, and every refusal and failure.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_launchd::launchctl::{
    LaunchctlError, LaunchctlOutput, bootout_args, bootstrap_args, print_args,
};
use hostkeeper_launchd::launcher::{POLL, STDERR_FILE, STDOUT_FILE};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchdLauncher};
use hostkeeper_steps::{Captured, LaunchError, ProbeCall, ProbeExit, RING_BYTES, StepLauncher};
use plist::Value;
use support::{FakePace, Hook, SimLaunchd, scratch};

const UNIT: &str = "hostkeeper-self-update-protocol";
const LABEL: &str = "dev.hostkeeper.hostkeeper.probe.hostkeeper-self-update-protocol";
const ARGS: &[&str] = &["self-update-protocol"];

fn label() -> Label {
    Label::parse(LABEL).expect("a label")
}

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

/// An executable file `hostkeeper` in `dir`.
fn binary(dir: &Path) -> PathBuf {
    let file = dir.join("hostkeeper");
    fs::write(&file, "#!hostkeeper\n").expect("write the binary");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("make it executable");
    file
}

/// What the launcher gave its log, per probe.
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

/// A hook that runs `secs` and exits `code`, printing exactly `stdout`.
fn printing(secs: u64, code: Option<i32>, stdout: &[u8]) -> Hook {
    Hook {
        secs,
        code,
        stdout: stdout.to_vec(),
        stderr: b"log\n".to_vec(),
        lose_stdout: false,
    }
}

fn exit(code: Option<i32>, output: &[u8], truncated: bool) -> ProbeExit {
    ProbeExit {
        code,
        output: output.to_vec(),
        truncated,
    }
}

/// Catches a probe that runs no job (the trait's default, which answered every Mac self-update
/// `protocol_unreadable`), or a call added, dropped, reordered or misspelt in its run: a job
/// left from an earlier probe is looked for under the probe's label, the plist under the job's
/// directory is bootstrapped, the job is read until it has exited 0 with one wait of [`POLL`]
/// between two reads, and the ended job is booted out. The exit is the job's code and the whole
/// of what it printed; the job's directory is gone afterwards.
#[test]
fn a_probe_runs_its_binary_as_one_job_with_exactly_these_launchctl_calls() {
    let root = scratch("probe-calls");
    let file = binary(root.path());
    let jobs = root.path().join("jobs");
    let fake = FakeLaunchctl::new();
    let print = print_args(&label());
    let plist = jobs.join(LABEL).join(format!("{LABEL}.plist"));
    fake.then(&print, Ok(LaunchctlOutput::not_loaded()));
    fake.then(&bootstrap_args(&plist), Ok(LaunchctlOutput::success("")));
    fake.then(
        &print,
        Ok(LaunchctlOutput::success(&format!(
            "system/{LABEL} = {{\n\tstate = running\n\tpid = 4242\n}}\n"
        ))),
    );
    fake.then(
        &print,
        Ok(LaunchctlOutput::success(&format!(
            "system/{LABEL} = {{\n\tstate = not running\n\tlast exit code = 0\n}}\n"
        ))),
    );
    fake.then(&bootout_args(&label()), Ok(LaunchctlOutput::success("")));
    let pace = FakePace::default();
    let launcher = LaunchdLauncher::new(fake.clone(), pace.clone(), &jobs);

    let ran = launcher.run_probe(&probe(&file, 10, 64));

    // The fake writes no output: the job printed nothing.
    assert_eq!(ran.expect("the probe ran"), exit(Some(0), b"", false));
    let expected: Vec<Vec<String>> = vec![
        vec!["print".into(), format!("system/{LABEL}")],
        vec![
            "bootstrap".into(),
            "system".into(),
            plist.to_str().expect("UTF-8").to_owned(),
        ],
        vec!["print".into(), format!("system/{LABEL}")],
        vec!["print".into(), format!("system/{LABEL}")],
        vec!["bootout".into(), format!("system/{LABEL}")],
    ];
    assert_eq!(fake.calls(), expected);
    assert_eq!(pace.sleeps(), [POLL]);
    assert!(!jobs.join(LABEL).exists(), "the job's directory is removed");
}

/// Catches a probe job that would run something else than `<binary> self-update-protocol`, or
/// run it as another user, kept alive, or with its output elsewhere: the plist decodes to
/// exactly the probe's label, the binary with the probe's arguments, `RunAtLoad`, the process
/// group not abandoned, and the two output files in the job's directory, and to nothing else.
/// What the binary printed is the probe's output.
#[test]
fn the_probe_job_runs_the_binary_once_with_its_arguments_and_reads_what_it_printed() {
    let root = scratch("probe-plist");
    let file = binary(root.path());
    let jobs = root.path().join("jobs");
    let (launcher, sim, _, _) = sim_launcher(&jobs);
    sim.script(LABEL, printing(1, Some(0), b"1\n"));

    let ran = launcher
        .run_probe(&probe(&file, 10, 64))
        .expect("the probe ran");

    assert_eq!(ran, exit(Some(0), b"1\n", false));
    let dir = jobs.join(LABEL);
    let path = |p: PathBuf| Value::String(p.to_str().expect("UTF-8").to_owned());
    let expected: plist::Dictionary = [
        ("Label", Value::String(LABEL.to_owned())),
        (
            "ProgramArguments",
            Value::Array(vec![
                path(file.clone()),
                Value::String("self-update-protocol".to_owned()),
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
    assert!(!sim.is_loaded(LABEL), "the ended job is unloaded");
}

/// Catches a probe that is unbounded, bounded late or early, or whose overrun reads as a probe
/// that answered: a probe that would run an hour is booted out at its 10 s bound with no exit
/// code, and a bound of 0 is 1 second (a probe is never unbounded). Time is the fake pace's.
#[test]
fn a_probe_past_its_bound_is_booted_out_with_no_exit_code_and_a_bound_of_0_is_1_second() {
    let root = scratch("probe-bound");
    let file = binary(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    sim.script(LABEL, printing(3600, Some(0), b"1\n"));
    sim.script(LABEL, printing(3600, Some(0), b"1\n"));

    let bounded = launcher
        .run_probe(&probe(&file, 10, 64))
        .expect("the probe ran");
    let zero = launcher
        .run_probe(&probe(&file, 0, 64))
        .expect("the probe ran");

    assert_eq!(bounded.code, None);
    assert_eq!(zero.code, None);
    assert_eq!(
        sim.stopped_after(),
        [
            (LABEL.to_owned(), Duration::from_secs(10)),
            (LABEL.to_owned(), Duration::from_secs(1)),
        ]
    );
    assert!(!sim.is_loaded(LABEL));
}

/// Catches the exit code or the output lost, unbounded, or misread: an exit code that is not 0
/// is the probe's, one killed by a signal has none; an output of exactly the limit is whole, one
/// byte more is cut to the limit and reported, as is an output longer than the capture keeps
/// (its last [`RING_BYTES`]); the log is given what was captured, standard error included, under
/// the probe's unit.
#[test]
fn the_exit_code_and_at_most_the_limit_of_the_output_are_the_probe_exit() {
    let root = scratch("probe-exit");
    let file = binary(root.path());
    let (launcher, sim, _, logged) = sim_launcher(&root.path().join("jobs"));
    let mut long = vec![b'7'; RING_BYTES];
    long.extend_from_slice(b"1234");
    for hook in [
        printing(1, Some(3), b"x"),
        printing(1, None, b""),
        printing(1, Some(0), b"1234"),
        printing(1, Some(0), b"12345"),
        printing(1, Some(0), &long),
    ] {
        sim.script(LABEL, hook);
    }

    let exits: Vec<ProbeExit> = (0..5)
        .map(|_| {
            launcher
                .run_probe(&probe(&file, 10, 4))
                .expect("the probe ran")
        })
        .collect();

    assert_eq!(
        exits,
        [
            exit(Some(3), b"x", false),
            exit(None, b"", false),
            exit(Some(0), b"1234", false),
            exit(Some(0), b"1234", true),
            exit(Some(0), b"7777", true),
        ]
    );
    let logged = logged.lock().expect("the log");
    assert_eq!(logged.len(), 5);
    assert_eq!(logged[0].0, UNIT);
    assert_eq!(logged[0].1.code, Some(3));
    assert_eq!(logged[0].1.stdout.bytes(), b"x");
    assert_eq!(logged[0].1.stderr.bytes(), b"log\n");
}

/// Catches a probe whose result depends on the log: a launcher with no log gives the same exit.
#[test]
fn a_launcher_with_no_log_still_gives_the_probe_exit() {
    let root = scratch("probe-no-log");
    let file = binary(root.path());
    let pace = FakePace::default();
    let sim = SimLaunchd::new(&pace);
    let launcher = LaunchdLauncher::new(sim.clone(), pace, &root.path().join("jobs"));
    sim.script(LABEL, printing(1, Some(0), b"2\n"));
    assert_eq!(
        launcher
            .run_probe(&probe(&file, 10, 64))
            .expect("the probe ran"),
        exit(Some(0), b"2\n", false)
    );
}

/// Catches a probe that boots out, or is mistaken for, a step's job: a probe named after a step
/// unit runs under the probe's label, and the step job of that unit, running, is left loaded;
/// a probe job left loaded by an earlier probe (a daemon killed while it ran) is booted out
/// before the new one is loaded.
#[test]
fn a_probe_never_touches_a_step_job_and_boots_out_a_probe_job_left_loaded() {
    let root = scratch("probe-apart");
    let file = binary(root.path());
    let (launcher, sim, _, _) = sim_launcher(&root.path().join("jobs"));
    let step_unit = "updater-web-cutover-7";
    let step_label = Label::from_unit(step_unit).expect("a step unit");
    let probe_label = Label::probe(step_unit).expect("a probe label");
    assert_eq!(
        probe_label.as_str(),
        "dev.hostkeeper.hostkeeper.probe.updater-web-cutover-7"
    );
    sim.running(step_label.as_str());
    sim.script(probe_label.as_str(), printing(1, Some(0), b"1\n"));
    let call = ProbeCall {
        unit: step_unit,
        ..probe(&file, 10, 64)
    };
    assert_eq!(
        launcher.run_probe(&call).expect("the probe ran"),
        exit(Some(0), b"1\n", false)
    );
    assert!(sim.is_loaded(step_label.as_str()), "the step still runs");
    assert!(
        sim.calls()
            .iter()
            .all(|args| args.last() != Some(&format!("system/{step_label}"))),
        "nothing was asked of the step's job"
    );

    sim.running(LABEL);
    sim.script(LABEL, printing(1, Some(0), b"1\n"));
    let before = sim.calls().len();
    assert_eq!(
        launcher
            .run_probe(&probe(&file, 10, 64))
            .expect("the probe ran"),
        exit(Some(0), b"1\n", false)
    );
    let calls = sim.calls();
    assert_eq!(calls[before], print_args(&label()));
    assert_eq!(calls[before + 1], bootout_args(&label()));
    assert_eq!(calls[before + 2][0], "bootstrap");
}

fn launcher_error(result: Result<ProbeExit, LaunchError>) -> String {
    match result {
        Err(LaunchError::Launcher(why)) => why,
        other => panic!("expected a launcher error, got {other:?}"),
    }
}

fn no_start(result: Result<ProbeExit, LaunchError>) -> String {
    match result {
        Err(LaunchError::NoStart(why)) => why,
        other => panic!("expected the probe not to start, got {other:?}"),
    }
}

/// Catches a probe started from a path that cannot run, or under a name that is no label: a
/// relative path and a unit that makes no label are launcher errors, a missing file, a directory
/// and a file that cannot be executed are a probe that did not start, each named exactly; none
/// of them runs `launchctl`.
#[test]
fn a_binary_that_cannot_run_is_refused_before_launchctl_is_run() {
    let root = scratch("probe-refused");
    let plain = root.path().join("plain");
    fs::write(&plain, "data").expect("write");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).expect("chmod");
    let missing = root.path().join("missing");
    let fake = FakeLaunchctl::new();
    let launcher = LaunchdLauncher::new(fake.clone(), FakePace::default(), root.path());

    assert_eq!(
        launcher_error(launcher.run_probe(&probe(Path::new("bin/hostkeeper"), 10, 64))),
        "the probed binary \"bin/hostkeeper\" is not an absolute path"
    );
    let file = binary(root.path());
    let bad_unit = ProbeCall {
        unit: "hostkeeper self-update",
        ..probe(&file, 10, 64)
    };
    assert_eq!(
        launcher_error(launcher.run_probe(&bad_unit)),
        "launchd label \"dev.hostkeeper.hostkeeper.probe.hostkeeper self-update\" holds the \
         byte 0x20, which a label may not"
    );
    assert_eq!(
        no_start(launcher.run_probe(&probe(&missing, 10, 64))),
        format!(
            "{}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    assert_eq!(
        no_start(launcher.run_probe(&probe(root.path(), 10, 64))),
        format!("{} is not an executable file", root.path().display())
    );
    assert_eq!(
        no_start(launcher.run_probe(&probe(&plain, 10, 64))),
        format!("{} is not an executable file", plain.display())
    );
    assert!(fake.calls().is_empty());
}

/// Catches a probe job left behind, or read as an answer, when `launchctl` fails: a bootstrap
/// that fails and a read that fails are launcher errors naming the job, the job is booted out
/// once more and its directory removed.
#[test]
fn a_launchctl_failure_is_a_launcher_error_and_leaves_no_job() {
    let root = scratch("probe-launchctl");
    let file = binary(root.path());
    let jobs = root.path().join("jobs");
    let (launcher, sim, _, _) = sim_launcher(&jobs);
    let plist = jobs.join(LABEL).join(format!("{LABEL}.plist"));
    let plist = plist.to_str().expect("UTF-8");
    sim.fail_next(
        &["bootstrap", "system", plist],
        Ok(LaunchctlOutput::exit(
            5,
            "Bootstrap failed: 5: Input/output error",
        )),
    );
    assert_eq!(
        launcher_error(launcher.run_probe(&probe(&file, 10, 64))),
        format!(
            "launchctl bootstrap of the job {LABEL} exited with Some(5): Bootstrap failed: 5: \
             Input/output error"
        )
    );
    assert!(!jobs.join(LABEL).exists());
    assert_eq!(
        sim.calls().last(),
        Some(&bootout_args(&label())),
        "booted out once more"
    );

    sim.script(LABEL, printing(3600, Some(0), b"1\n"));
    let target = format!("system/{LABEL}");
    sim.fail_next(&["print", &target], Ok(LaunchctlOutput::not_loaded()));
    sim.fail_next(
        &["print", &target],
        Err(LaunchctlError::Spawn {
            args: format!("print {target}"),
            why: "gone".to_owned(),
        }),
    );
    let why = launcher_error(launcher.run_probe(&probe(&file, 10, 64)));
    assert!(
        why.starts_with(&format!("cannot read the job {LABEL}: ")),
        "{why}"
    );
    assert!(!jobs.join(LABEL).exists());
    assert!(!sim.is_loaded(LABEL), "the job is booted out");
}
