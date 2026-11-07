//! The contract both step launchers keep, run against each through the `StepLauncher` trait: the
//! systemd launcher over a stand-in for `systemd-run` and `systemctl` that keeps `RuntimeMaxSec=`
//! as systemd does, and the launchd launcher over `SimLaunchd`, a stand-in for launchd behind the
//! `launchctl` port. One scenario, one assertion, for both.
//!
//! What each side proves differs where the bound is enforced. On systemd the bound is systemd's:
//! the stand-in stops a hook at the `RuntimeMaxSec=` the launcher passed, so for systemd the
//! bound scenarios prove the value the launcher passes (the seconds, or `infinity`) and how it
//! reads the end `systemd-run` reports. On launchd the bound is the launcher's own polling and
//! boot-out, which these scenarios run in full.
//!
//! - a bounded step that runs past its bound is stopped at the bound and is a failed step;
//! - an unbounded step is never stopped: it runs to its own end, however long;
//! - a step that ends within its bound keeps its own exit code and report line;
//! - a stop of a running step, a second stop, and a stop of a step never started are all `Ok`;
//! - the capture keeps the last 1 MiB of each stream, and the report line from it;
//! - a step run again under the same unit reports what the second run wrote;
//! - an additive step (the hook's `uninstall`) keeps its own exit code and report line within
//!   its bound, and past it is stopped at the bound and is a failed step;
//! - a probe (the daemon asking a new hostkeeper binary for its self-update protocol) keeps its
//!   own exit code and output within its bound, and past it, or past 1 second when its bound is
//!   0, is stopped and did not exit 0.

mod support;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_hook::{Outcome, Step};
use hostkeeper_launchd::{Label, LaunchdLauncher};
use hostkeeper_steps::{
    AdditiveCall, Bound, Captured, ProbeCall, ProbeExit, Program, RING_BYTES, Ring, Spawner,
    StepExit, StepLauncher, StepLog, SystemdLauncher,
};
use hostkeeper_testkit::Scratch;
use support::{FakePace, Hook, PARAMS, SimLaunchd, call, hook_file, scratch, secs};

const UNIT: &str = "updater-web-cutover-7";

/// What a test sees of a launcher, whichever it is.
trait Rig {
    fn name(&self) -> &'static str;
    fn launcher(&self) -> &dyn StepLauncher;
    /// The next run of `unit` starts a hook that does `hook`.
    fn script(&self, unit: &str, hook: Hook);
    /// A step of `unit` is running, and runs until it is stopped.
    fn running(&self, unit: &str);
    /// Whether a step of `unit` is running.
    fn is_running(&self, unit: &str) -> bool;
    /// For each run stopped while its hook was still running: how long it had run.
    fn stopped_after(&self) -> Vec<Duration>;
    /// What the launcher gave its log, per run.
    fn logged(&self) -> Vec<(String, Captured)>;
}

type Logged = Arc<Mutex<Vec<(String, Captured)>>>;

fn log_into(logged: &Logged) -> StepLog {
    let sink = logged.clone();
    Arc::new(move |unit: &str, captured: &Captured| {
        sink.lock()
            .expect("the log")
            .push((unit.to_owned(), captured.clone()))
    })
}

/// What `systemd-run --wait` exits with for a unit that ended other than by its own exit: a unit
/// stopped by `RuntimeMaxSec=` has the result `timeout`, which `systemd-run` reports as 1.
const SYSTEMD_RUN_TIMEOUT: i32 = 1;

#[derive(Default)]
struct SystemdState {
    scripted: BTreeMap<String, VecDeque<Hook>>,
    running: BTreeSet<String>,
    stopped_after: Vec<Duration>,
}

/// `systemd-run --wait --pipe` and `systemctl stop`, keeping `RuntimeMaxSec=`: a hook that runs
/// longer than it is stopped at it.
#[derive(Clone, Default)]
struct SimSystemd(Arc<Mutex<SystemdState>>);

fn ring(bytes: &[u8]) -> Ring {
    let mut ring = Ring::new();
    ring.push(bytes);
    ring
}

impl Spawner for SimSystemd {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured> {
        let args: Vec<&str> = args.iter().map(|a| a.to_str().expect("UTF-8")).collect();
        let mut state = self.0.lock().expect("the stand-in");
        if program == Program::Systemctl {
            let unit = args[2];
            let code = if state.running.remove(unit) { 0 } else { 5 };
            return Ok(Captured {
                code: Some(code),
                stdout: Ring::new(),
                stderr: Ring::new(),
            });
        }
        let value = |prefix: &str| {
            args.iter()
                .find_map(|a| a.strip_prefix(prefix))
                .expect("the argument is there")
                .to_owned()
        };
        let unit = value("--unit=");
        let hook = state
            .scripted
            .get_mut(&unit)
            .and_then(VecDeque::pop_front)
            .expect("a hook is scripted");
        let max = value("--property=RuntimeMaxSec=");
        let code = match max.parse::<u64>() {
            Ok(max) if hook.secs > max => {
                state.stopped_after.push(Duration::from_secs(max));
                Some(SYSTEMD_RUN_TIMEOUT)
            }
            _ => hook.code,
        };
        Ok(Captured {
            code,
            stdout: ring(&hook.stdout),
            stderr: ring(&hook.stderr),
        })
    }
}

struct SystemdRig {
    launcher: SystemdLauncher<SimSystemd>,
    sim: SimSystemd,
    logged: Logged,
}

impl SystemdRig {
    fn new() -> SystemdRig {
        let sim = SimSystemd::default();
        let logged = Logged::default();
        SystemdRig {
            launcher: SystemdLauncher::new(sim.clone()).with_log(log_into(&logged)),
            sim,
            logged,
        }
    }
}

impl Rig for SystemdRig {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn launcher(&self) -> &dyn StepLauncher {
        &self.launcher
    }

    fn script(&self, unit: &str, hook: Hook) {
        let mut state = self.sim.0.lock().expect("the stand-in");
        state
            .scripted
            .entry(unit.to_owned())
            .or_default()
            .push_back(hook);
    }

    fn running(&self, unit: &str) {
        let mut state = self.sim.0.lock().expect("the stand-in");
        state.running.insert(unit.to_owned());
    }

    fn is_running(&self, unit: &str) -> bool {
        self.sim
            .0
            .lock()
            .expect("the stand-in")
            .running
            .contains(unit)
    }

    fn stopped_after(&self) -> Vec<Duration> {
        self.sim
            .0
            .lock()
            .expect("the stand-in")
            .stopped_after
            .clone()
    }

    fn logged(&self) -> Vec<(String, Captured)> {
        self.logged.lock().expect("the log").clone()
    }
}

struct LaunchdRig {
    _jobs: Scratch,
    launcher: LaunchdLauncher,
    sim: SimLaunchd,
    logged: Logged,
}

/// The label of the job of `unit`: a step's, or else a probe's.
fn label(unit: &str) -> String {
    Label::from_unit(unit)
        .or_else(|_| Label::probe(unit))
        .expect("a step or a probe unit")
        .to_string()
}

impl LaunchdRig {
    fn new() -> LaunchdRig {
        let jobs = scratch("equivalence-jobs");
        let pace = FakePace::default();
        let sim = SimLaunchd::new(&pace);
        let logged = Logged::default();
        LaunchdRig {
            launcher: LaunchdLauncher::new(sim.clone(), pace, jobs.path())
                .with_log(log_into(&logged)),
            _jobs: jobs,
            sim,
            logged,
        }
    }
}

impl Rig for LaunchdRig {
    fn name(&self) -> &'static str {
        "launchd"
    }

    fn launcher(&self) -> &dyn StepLauncher {
        &self.launcher
    }

    fn script(&self, unit: &str, hook: Hook) {
        self.sim.script(&label(unit), hook);
    }

    fn running(&self, unit: &str) {
        self.sim.running(&label(unit));
    }

    fn is_running(&self, unit: &str) -> bool {
        self.sim.is_loaded(&label(unit))
    }

    fn stopped_after(&self) -> Vec<Duration> {
        self.sim
            .stopped_after()
            .into_iter()
            .map(|(_, ran)| ran)
            .collect()
    }

    fn logged(&self) -> Vec<(String, Captured)> {
        self.logged.lock().expect("the log").clone()
    }
}

/// Both rigs, and the directory holding the hook they run.
fn rigs(name: &str) -> (Scratch, PathBuf, Vec<Box<dyn Rig>>) {
    let dir = scratch(name);
    let hook = hook_file(dir.path());
    let rigs: Vec<Box<dyn Rig>> = vec![Box::new(SystemdRig::new()), Box::new(LaunchdRig::new())];
    (dir, hook, rigs)
}

fn run(rig: &dyn Rig, hook: &Path, bound: Bound) -> StepExit {
    rig.launcher()
        .run(&call(hook, UNIT, Step::Cutover, bound))
        .unwrap_or_else(|e| panic!("{}: the step ran: {e}", rig.name()))
}

/// Catches a launcher that lets a bounded step run past its bound, stops it early, or reports a
/// stopped step as anything but failed: a hook that would run an hour under a 30 s bound is
/// stopped at 30 s, and its end reads as a failure. On systemd the stop is the stand-in's, at the
/// `RuntimeMaxSec=` the launcher passed; on launchd it is the launcher's own boot-out.
#[test]
fn a_bounded_step_that_overruns_is_stopped_at_its_bound_and_failed() {
    let (_dir, hook, rigs) = rigs("equivalence-overrun");
    for rig in &rigs {
        rig.script(UNIT, Hook::exits(3600, 0, "{}"));
        let exit = run(rig.as_ref(), &hook, secs(30));
        assert_eq!(
            Outcome::from_exit(exit.code),
            Outcome::Failure,
            "{}",
            rig.name()
        );
        assert_eq!(
            rig.stopped_after(),
            [Duration::from_secs(30)],
            "{}",
            rig.name()
        );
        assert!(!rig.is_running(UNIT), "{}", rig.name());
    }
}

/// Catches an unbounded `prepare` that a launcher stops anyway (a bound armed from nothing, or a
/// zero read as a bound): a hook that runs four hours is not stopped, and its exit code and report
/// are the step's.
#[test]
fn an_unbounded_step_is_not_armed() {
    let (_dir, hook, rigs) = rigs("equivalence-unbounded");
    for rig in &rigs {
        rig.script(UNIT, Hook::exits(4 * 3600, 0, "{\"long\":true}"));
        let exit = rig
            .launcher()
            .run(&call(&hook, UNIT, Step::Prepare, Bound::Unbounded))
            .expect("the step ran");
        assert_eq!(
            exit,
            StepExit {
                code: Some(0),
                last_line: Some("{\"long\":true}".to_owned())
            },
            "{}",
            rig.name()
        );
        assert!(rig.stopped_after().is_empty(), "{}", rig.name());
    }
}

/// Catches a launcher that changes a step's own end: a step that ends within its bound keeps its
/// exit code (`75` here) and its last line, and is not stopped.
#[test]
fn a_step_that_ends_within_its_bound_keeps_its_exit_and_report() {
    let (_dir, hook, rigs) = rigs("equivalence-within");
    for rig in &rigs {
        let line = "{\"class\":\"environment\",\"reason\":\"leases_live\"}";
        rig.script(UNIT, Hook::exits(29, 75, line));
        let exit = run(rig.as_ref(), &hook, secs(30));
        assert_eq!(
            exit,
            StepExit {
                code: Some(75),
                last_line: Some(line.to_owned())
            },
            "{}",
            rig.name()
        );
        assert!(rig.stopped_after().is_empty(), "{}", rig.name());
    }
}

/// Catches a stop that is not idempotent: a running step is stopped, a second stop of it is `Ok`,
/// and a stop of a unit that never ran is `Ok`.
#[test]
fn a_double_stop_is_ok() {
    let (_dir, _hook, rigs) = rigs("equivalence-stop");
    for rig in &rigs {
        rig.running(UNIT);
        let launcher = rig.launcher();
        launcher.stop(UNIT).expect("the first stop");
        assert!(!rig.is_running(UNIT), "{}", rig.name());
        launcher.stop(UNIT).expect("the second stop");
        launcher
            .stop("updater-web-verify-8")
            .unwrap_or_else(|e| panic!("{}: a stop of nothing: {e}", rig.name()));
    }
}

/// Catches a capture that keeps the head instead of the tail, keeps more than 1 MiB, or reads the
/// report from outside what it kept: a hook that writes 1 MiB and more to each stream keeps the
/// last [`RING_BYTES`] of each, marked cut, and its last line is the report. A last line whose
/// beginning fell out of the kept bytes is no report.
#[test]
fn capture_truncation_keeps_the_tail() {
    let (_dir, hook, rigs) = rigs("equivalence-capture");
    for rig in &rigs {
        let mut stdout = vec![b'a'; RING_BYTES];
        stdout.extend_from_slice(b"\n{\"tail\":true}\n");
        let mut stderr = vec![b'b'; RING_BYTES + 7];
        stderr.extend_from_slice(b"end of log");
        rig.script(
            UNIT,
            Hook {
                secs: 1,
                code: Some(0),
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                lose_stdout: false,
            },
        );
        let exit = run(rig.as_ref(), &hook, secs(30));
        assert_eq!(
            exit.last_line.as_deref(),
            Some("{\"tail\":true}"),
            "{}",
            rig.name()
        );
        let logged = rig.logged();
        let (unit, captured) = &logged[0];
        assert_eq!(unit, UNIT, "{}", rig.name());
        assert_eq!(
            captured.stdout.bytes(),
            stdout[stdout.len() - RING_BYTES..],
            "{}",
            rig.name()
        );
        assert_eq!(
            captured.stderr.bytes(),
            stderr[stderr.len() - RING_BYTES..],
            "{}",
            rig.name()
        );
        assert!(
            captured.stdout.cut() && captured.stderr.cut(),
            "{}",
            rig.name()
        );

        rig.script(
            UNIT,
            Hook {
                secs: 1,
                code: Some(0),
                stdout: vec![b'c'; RING_BYTES + 1],
                stderr: Vec::new(),
                lose_stdout: false,
            },
        );
        assert_eq!(
            run(rig.as_ref(), &hook, secs(30)).last_line,
            None,
            "{}",
            rig.name()
        );
    }
}

/// Catches a unit name that cannot be used again, or output of a first run read as a second's:
/// the same unit runs twice (a step tried again), and each run reports its own line.
#[test]
fn a_step_run_again_under_its_unit_reports_its_own_output() {
    let (_dir, hook, rigs) = rigs("equivalence-again");
    for rig in &rigs {
        rig.script(UNIT, Hook::exits(1, 75, "{\"first\":true}"));
        rig.script(UNIT, Hook::exits(1, 0, "{\"second\":true}"));
        let first = run(rig.as_ref(), &hook, secs(30));
        let second = run(rig.as_ref(), &hook, secs(30));
        assert_eq!(
            [first.last_line.as_deref(), second.last_line.as_deref()],
            [Some("{\"first\":true}"), Some("{\"second\":true}")],
            "{}",
            rig.name()
        );
        assert_eq!(
            [first.code, second.code],
            [Some(75), Some(0)],
            "{}",
            rig.name()
        );
    }
}

/// Catches a launcher that runs no additive step, or runs one under other rules than a step: the
/// hook's `uninstall`, bounded at 30 s, keeps its own exit code and last line when it ends within
/// the bound, and is stopped at 30 s and failed when it would run an hour.
#[test]
fn an_additive_step_keeps_its_exit_within_its_bound_and_is_stopped_past_it() {
    let (_dir, hook, rigs) = rigs("equivalence-additive");
    let unit = "updater-web-uninstall-7";
    let hook_args = ["update-hook".to_owned()];
    let uninstall = AdditiveCall {
        service: "web",
        step: "uninstall",
        hook_args: &hook_args,
        params: Path::new(PARAMS),
        executable: &hook,
        unit,
        timeout_secs: 30,
    };
    for rig in &rigs {
        let line = "{\"detail\":\"removed\"}";
        rig.script(unit, Hook::exits(29, 0, line));
        rig.script(unit, Hook::exits(3600, 0, "{}"));
        let run = || {
            rig.launcher()
                .run_additive(&uninstall)
                .unwrap_or_else(|e| panic!("{}: the step ran: {e}", rig.name()))
        };
        assert_eq!(
            run(),
            StepExit {
                code: Some(0),
                last_line: Some(line.to_owned())
            },
            "{}",
            rig.name()
        );
        assert!(rig.stopped_after().is_empty(), "{}", rig.name());
        assert_eq!(
            Outcome::from_exit(run().code),
            Outcome::Failure,
            "{}",
            rig.name()
        );
        assert_eq!(
            rig.stopped_after(),
            [Duration::from_secs(30)],
            "{}",
            rig.name()
        );
        assert!(!rig.is_running(unit), "{}", rig.name());
    }
}

/// Catches a launcher that runs no probe (the Mac's did not, so every Mac self-update was
/// refused `protocol_unreadable`), or runs one under other rules than the other launcher: the
/// probe `self-update-protocol`, bounded at 10 s, keeps its exit code 0 and its whole output
/// when it ends within the bound; one that would run an hour is stopped at 10 s and did not exit
/// 0; and with a bound of 0 it is stopped at 1 s, never unbounded.
#[test]
fn a_probe_keeps_its_exit_within_its_bound_and_is_stopped_past_it() {
    let (_dir, binary, rigs) = rigs("equivalence-probe");
    let unit = "hostkeeper-self-update-protocol";
    let call = |timeout_secs| ProbeCall {
        service: "hostkeeper",
        executable: &binary,
        args: &["self-update-protocol"],
        unit,
        timeout_secs,
        output_bytes: 64,
    };
    for rig in &rigs {
        rig.script(unit, Hook::exits(9, 0, "1"));
        rig.script(unit, Hook::exits(3600, 0, "1"));
        rig.script(unit, Hook::exits(3600, 0, "1"));
        let run = |timeout_secs| {
            rig.launcher()
                .run_probe(&call(timeout_secs))
                .unwrap_or_else(|e| panic!("{}: the probe ran: {e}", rig.name()))
        };
        assert_eq!(
            run(10),
            ProbeExit {
                code: Some(0),
                output: b"1\n".to_vec(),
                truncated: false,
            },
            "{}",
            rig.name()
        );
        assert!(rig.stopped_after().is_empty(), "{}", rig.name());
        assert_ne!(run(10).code, Some(0), "{}", rig.name());
        assert_ne!(run(0).code, Some(0), "{}", rig.name());
        assert_eq!(
            rig.stopped_after(),
            [Duration::from_secs(10), Duration::from_secs(1)],
            "{}",
            rig.name()
        );
        assert!(!rig.is_running(unit), "{}", rig.name());
    }
}
