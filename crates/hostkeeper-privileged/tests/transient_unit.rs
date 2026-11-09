#![cfg(feature = "privileged")]
//! The production step launcher (`hostkeeper_steps::SystemdLauncher` over the host's
//! `systemd-run` and `systemctl`) and the dead-man, as root, against the real systemd of the
//! host: a step is a transient unit with `RuntimeMaxSec=` and nothing else, systemd ends it at
//! its bound with its whole control group, an unbounded `prepare` gets `RuntimeMaxSec=infinity`
//! and the dead-man leaves it alone, the dead-man stops a unit by name, a unit's name can be used
//! again after it failed, and what the launcher keeps of a long output. A tool run
//! (`StepLauncher::run_tool`, docs/design/packages.md section 5.3) is a transient unit with
//! `PrivateNetwork=yes`: the tool sees the loopback interface alone.
//!
//! The fixture's `sleep` starts one child in the step's control group and writes both pids,
//! which is how a test knows the step is running and checks that nothing of the group is left.
//! No test asserts how long anything took (design section 5.3; AGENTS.md T10).

mod support;

use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Invocation, Outcome, Report, Step};
use hostkeeper_privileged::fixture::{self, FAILED_REASON, Mode, ok_report};
use hostkeeper_privileged::{Rig, SERVICE};
use hostkeeper_steps::{
    Bound, Captured, DeadMan, RING_BYTES, StepCall, StepExit, StepLauncher, SystemdLauncher,
    SystemdRunProcess, ToolCall, ToolNetwork, Watch,
};
use support::{Bench, FakeClock, StopOnDrop};

/// The production launcher.
fn launcher() -> SystemdLauncher<SystemdRunProcess> {
    SystemdLauncher::system()
}

fn secs(n: u64) -> Bound {
    Bound::Secs(NonZeroU64::new(n).expect("not 0"))
}

/// The call of `step` of the fixture in the unit `unit`, with the params file `params`.
fn call<'a>(rig: &'a Rig, unit: &'a str, step: Step, params: &Path, bound: Bound) -> StepCall<'a> {
    StepCall {
        service: SERVICE,
        invocation: Invocation {
            step,
            // `prepare` is given a staged release; the fixture only reads its params.
            artifact: (step == Step::Prepare).then(|| rig.dir().to_path_buf()),
            params: Some(params.to_path_buf()),
        },
        executable: rig.fixture(),
        unit,
        bound,
    }
}

/// The params of a `sleep` step named `name` with its pid file, and that pid file.
fn sleeping(rig: &Rig, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let pids = rig.pid_file(name);
    let params = rig
        .params(name, &Mode::Sleep, Some(pids.as_path()))
        .expect("write the params");
    (params, pids)
}

/// The pids the `sleep` step wrote once it ran: its own and its child's.
fn step_pids(pids: &Path) -> Vec<u32> {
    let found = fixture::read_pid_file(pids).expect("the step wrote its pid file");
    assert_eq!(found.pids.len(), 2, "the step and its child: {found:?}");
    found.pids
}

/// Waits until `unit` is gone (`--collect` has systemd unload it once it has ended, which may come
/// just after `systemd-run` returns), then asserts that no process of its control group is left.
fn assert_unit_and_group_gone(unit: &str, pids: &[u32]) {
    support::wait_until(|| support::property(unit, "LoadState") == "not-found");
    for pid in pids {
        assert!(
            support::ended(*pid),
            "process {pid} of {unit} is still running"
        );
    }
}

/// Catches a launcher that leaves `RuntimeMaxSec=` off the unit, or sets it to another bound: a
/// `sleep` step with a bound of 10 seconds is ended by systemd itself (its result `timeout`), the
/// launcher reads a failed step, and neither the step nor its child is left.
#[test]
fn a_step_runs_in_a_real_transient_unit_and_is_ended_at_its_bound() {
    let bench = Bench::new("ended_at_bound");
    let rig = &bench.rig;
    let unit = rig.unit();
    let (params, pids) = sleeping(rig, "sleep");
    let launcher = launcher();
    // The bound starts with the unit; the pid file is waited for while the step runs, so the
    // test does not depend on the fixture starting within the bound's first seconds.
    let exit = std::thread::scope(|scope| {
        let running =
            scope.spawn(|| launcher.run(&call(rig, &unit, Step::Cutover, &params, secs(10))));
        let _stop = StopOnDrop(&unit);
        support::wait_for_file(&pids, || running.is_finished());
        running.join().expect("the run thread")
    })
    .expect("the launcher runs the step");
    // systemd-run --wait exits 1 for a unit whose result is `timeout`: not a code of the contract
    // that reads as anything but a failure.
    assert_eq!(Outcome::from_exit(exit.code), Outcome::Failure, "{exit:?}");
    assert_ne!(exit.code, Some(0), "{exit:?}");
    assert_eq!(exit.last_line, None, "a sleeping step prints nothing");
    let pids = step_pids(&pids);
    assert_unit_and_group_gone(&unit, &pids);
    let journal = support::journal(&unit);
    assert!(
        journal.contains("UNIT_RESULT=timeout") || journal.contains("Failed with result 'timeout'"),
        "the journal of {unit} names no timeout:\n{journal}"
    );
}

/// Catches a launcher that gives the unit a hardening or identity property (design 5.3: the unit
/// confines nothing): a running step's unit has `RuntimeMaxSec=` of its bound, and `User=`,
/// `AmbientCapabilities=`, `ProtectSystem=`, `ReadWritePaths=`, `ReadOnlyPaths=` and the rest
/// equal to those of a transient unit started with no property at all.
#[test]
fn the_transient_unit_carries_runtime_max_sec_and_nothing_else() {
    let bench = Bench::new("only_runtime_max_sec");
    let rig = &bench.rig;
    let (unit, plain) = (rig.unit(), rig.unit());
    let (params, pids) = sleeping(rig, "sleep");
    let launcher = launcher();
    let compared = [
        "User",
        "Group",
        "DynamicUser",
        "AmbientCapabilities",
        "CapabilityBoundingSet",
        "NoNewPrivileges",
        "ProtectSystem",
        "ProtectHome",
        "PrivateTmp",
        "PrivateDevices",
        "PrivateNetwork",
        "ReadWritePaths",
        "ReadOnlyPaths",
        "InaccessiblePaths",
        "KillMode",
        "TimeoutStopUSec",
    ];
    std::thread::scope(|scope| {
        let running =
            scope.spawn(|| launcher.run(&call(rig, &unit, Step::Verify, &params, secs(600))));
        let _stop = StopOnDrop(&unit);
        support::wait_for_file(&pids, || running.is_finished());
        support::start_serve(rig, &plain);
        let mut asked = compared.to_vec();
        asked.push("RuntimeMaxUSec");
        let step = support::show(&unit, &asked);
        let base = support::show(&plain, &asked);
        assert_eq!(
            step.get("RuntimeMaxUSec").map(String::as_str),
            Some("10min")
        );
        assert_eq!(
            base.get("RuntimeMaxUSec").map(String::as_str),
            Some("infinity")
        );
        for property in compared {
            assert!(
                step.contains_key(property),
                "systemctl show printed no {property}"
            );
            assert_eq!(step.get(property), base.get(property), "{property}");
        }
        launcher.stop(&unit).expect("the launcher stops the step");
        let exit = running.join().expect("the run thread").expect("the run");
        assert_eq!(
            exit.code, None,
            "a step the launcher stopped has no exit code"
        );
    });
    assert_unit_and_group_gone(&unit, &step_pids(&pids));
}

/// Catches a launcher that reads a bound of 0 as 0 seconds, or a dead-man that stops a step with
/// no bound: a `prepare` declared with 0 runs as a unit with `RuntimeMaxSec=infinity`, is still
/// running after the dead-man's tick at a second far past any deadline (the runner does not arm
/// the watch for it), and ends when it is stopped.
#[test]
fn an_unbounded_prepare_gets_runtime_max_sec_infinity() {
    let bench = Bench::new("unbounded_prepare");
    let rig = &bench.rig;
    let unit = rig.unit();
    let (params, pids) = sleeping(rig, "prepare");
    let launcher = Arc::new(launcher());
    let clock = Arc::new(FakeClock::at(1000));
    let watch = Watch::new();
    let dead_man = DeadMan::new(
        Arc::clone(&watch),
        Arc::clone(&launcher) as Arc<dyn StepLauncher>,
        Arc::clone(&clock) as Arc<dyn hostkeeper_steps::Clock>,
    );
    std::thread::scope(|scope| {
        let running = scope
            .spawn(|| launcher.run(&call(rig, &unit, Step::Prepare, &params, Bound::Unbounded)));
        let _stop = StopOnDrop(&unit);
        support::wait_for_file(&pids, || running.is_finished());
        assert_eq!(support::property(&unit, "RuntimeMaxUSec"), "infinity");
        clock.set(u64::MAX / 2);
        assert!(
            !dead_man.tick().expect("a tick"),
            "nothing is armed to stop"
        );
        assert_eq!(support::property(&unit, "ActiveState"), "active");
        for pid in step_pids(&pids) {
            assert!(!support::ended(pid), "process {pid} of {unit} ended");
        }
        launcher.stop(&unit).expect("the launcher stops the step");
        let exit = running.join().expect("the run thread").expect("the run");
        assert_eq!(
            exit.code, None,
            "a step the launcher stopped has no exit code"
        );
    });
    assert_unit_and_group_gone(&unit, &step_pids(&pids));
}

/// Catches a dead-man stop by pid rather than by the unit's name, a stop that leaves the step's
/// child behind, and a stopped step read as a success: the dead-man, armed with a deadline its
/// clock has passed, stops the unit at one tick; the unit and its whole control group are gone,
/// and the launcher reports the step with no exit code, a failure.
#[test]
fn the_dead_man_stops_a_unit_by_name() {
    let bench = Bench::new("dead_man_stops");
    let rig = &bench.rig;
    let unit = rig.unit();
    let (params, pids) = sleeping(rig, "sleep");
    let launcher = Arc::new(launcher());
    let clock = Arc::new(FakeClock::at(1000));
    let watch = Watch::new();
    let dead_man = DeadMan::new(
        Arc::clone(&watch),
        Arc::clone(&launcher) as Arc<dyn StepLauncher>,
        Arc::clone(&clock) as Arc<dyn hostkeeper_steps::Clock>,
    );
    let exit = std::thread::scope(|scope| {
        let running =
            scope.spawn(|| launcher.run(&call(rig, &unit, Step::Cutover, &params, secs(3600))));
        let _stop = StopOnDrop(&unit);
        support::wait_for_file(&pids, || running.is_finished());
        assert_eq!(support::property(&unit, "ActiveState"), "active");
        watch.arm(&unit, 1010);
        assert!(
            !dead_man.tick().expect("a tick"),
            "the deadline has not come"
        );
        clock.set(1011);
        assert!(
            dead_man.tick().expect("the stop works"),
            "the dead-man stopped it"
        );
        running.join().expect("the run thread").expect("the run")
    });
    assert!(watch.disarm().is_none(), "the dead-man's stop worked");
    assert_eq!(
        exit,
        StepExit {
            code: None,
            last_line: None,
        }
    );
    assert_eq!(Outcome::from_exit(exit.code), Outcome::Failure);
    assert_unit_and_group_gone(&unit, &step_pids(&pids));
}

/// Catches a launcher that leaves a failed unit loaded (without `--collect`, systemd keeps a
/// failed unit and refuses a new one of its name): a step that fails is run again under the
/// same name, and the second run starts and fails as the fixture says, with its own last line.
#[test]
fn a_unit_name_is_reusable_after_it_failed() {
    let bench = Bench::new("name_reused");
    let rig = &bench.rig;
    let unit = rig.unit();
    let params = rig
        .params("fail", &Mode::FailAt(Step::Rollback), None)
        .expect("write the params");
    let launcher = launcher();
    for run in 0..2 {
        let exit = launcher
            .run(&call(rig, &unit, Step::Rollback, &params, secs(600)))
            .expect("the launcher runs the step");
        assert_eq!(exit.code, Some(1), "run {run}: {exit:?}");
        let line = exit.last_line.expect("the fixture's last line");
        let report = Report::parse_line(&line).expect("a report");
        assert_eq!(report.reason.as_deref(), Some(FAILED_REASON), "run {run}");
        assert_eq!(report, fixture::failed_report(Step::Rollback));
    }
    support::wait_until(|| support::property(&unit, "LoadState") == "not-found");
}

/// Catches output lost or kept unbounded through the real `systemd-run --pipe`: a step that
/// writes 3 MiB and then its report exits `0`, the last line is the report, the log is given the
/// last [`RING_BYTES`] of standard output (cut at its start) and the hook's line on standard
/// error.
#[test]
fn a_long_output_is_kept_to_its_last_bytes_with_the_last_line() {
    let bench = Bench::new("long_output");
    let rig = &bench.rig;
    let unit = rig.unit();
    let flood = 3 * RING_BYTES as u64;
    let params = rig
        .params("flood", &Mode::Flood(flood), None)
        .expect("write the params");
    let logged: Arc<Mutex<Vec<(String, Captured)>>> = Arc::default();
    let log = Arc::clone(&logged);
    let launcher = launcher().with_log(Arc::new(move |unit: &str, captured: &Captured| {
        log.lock()
            .expect("the log")
            .push((unit.to_owned(), captured.clone()));
    }));
    let exit = launcher
        .run(&call(rig, &unit, Step::Cleanup, &params, secs(600)))
        .expect("the launcher runs the step");
    let report = ok_report(Step::Cleanup);
    assert_eq!(
        exit,
        StepExit {
            code: Some(0),
            last_line: Some(report.to_line()),
        }
    );
    let logged = logged.lock().expect("the log");
    assert_eq!(logged.len(), 1);
    let (logged_unit, captured) = &logged[0];
    assert_eq!(logged_unit, &unit);
    assert!(captured.stdout.cut(), "more than the ring was written");
    let kept = captured.stdout.bytes();
    assert_eq!(kept.len(), RING_BYTES);
    assert!(kept.ends_with(format!("{}\n", report.to_line()).as_bytes()));
    let log_text = String::from_utf8_lossy(&captured.stderr.bytes()).into_owned();
    assert!(log_text.contains("fixture: cleanup ok"), "{log_text:?}");
}

/// The tool `program` with `args`, in the unit `unit`, bounded by `timeout_secs`.
fn tool<'a>(unit: &'a str, program: &'a Path, args: &'a [std::ffi::OsString]) -> ToolCall<'a> {
    ToolCall {
        service: SERVICE,
        unit,
        program,
        args,
        timeout_secs: 600,
        network: ToolNetwork::Offline,
        writable: None,
        working_dir: None,
        env: &[],
    }
}

/// Catches a tool run that keeps the host's network, or whose end is not read as a step's: `wc
/// -l /proc/net/dev` run by the launcher's `run_tool` exits 0 and its last line counts the two
/// header lines and `lo` alone, the one interface of a network namespace of its own (the host
/// the job runs on has more); and a running tool's unit has `PrivateNetwork=yes` where a
/// transient unit started with no property has `no`, its `RuntimeMaxSec=` the call's bound, and
/// it ends with no exit code when the launcher stops it.
#[test]
fn a_tool_runs_in_a_real_transient_unit_with_no_network() {
    let bench = Bench::new("tool_no_network");
    let rig = &bench.rig;
    let launcher = launcher();

    let counted = rig.unit();
    let args = ["-l", "/proc/net/dev"].map(std::ffi::OsString::from);
    let exit = launcher
        .run_tool(&tool(&counted, Path::new("/usr/bin/wc"), &args))
        .expect("the launcher runs the tool");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    assert_eq!(
        String::from_utf8_lossy(&exit.stdout).trim(),
        "3 /proc/net/dev",
        "{exit:?}"
    );

    let (sleeping, plain) = (rig.unit(), rig.unit());
    let args = ["600"].map(std::ffi::OsString::from);
    std::thread::scope(|scope| {
        let running =
            scope.spawn(|| launcher.run_tool(&tool(&sleeping, Path::new("/usr/bin/sleep"), &args)));
        let _stop = StopOnDrop(&sleeping);
        support::wait_until(|| {
            running.is_finished() || support::property(&sleeping, "ActiveState") == "active"
        });
        support::start_serve(rig, &plain);
        let _plain = StopOnDrop(&plain);
        let asked = ["PrivateNetwork", "RuntimeMaxUSec"];
        let unit = support::show(&sleeping, &asked);
        let base = support::show(&plain, &asked);
        assert_eq!(unit.get("PrivateNetwork").map(String::as_str), Some("yes"));
        assert_eq!(base.get("PrivateNetwork").map(String::as_str), Some("no"));
        assert_eq!(
            unit.get("RuntimeMaxUSec").map(String::as_str),
            Some("10min")
        );
        launcher
            .stop(&sleeping)
            .expect("the launcher stops the tool");
        let exit = running.join().expect("the run thread").expect("the run");
        assert_eq!(
            exit.code, None,
            "a tool the launcher stopped has no exit code"
        );
    });
}

/// Catches a fetch run that loses the host's network or writes outside its paths: the
/// repository installer's two fetch commands run as `ToolNetwork::Fetch`, confined to the paths
/// they may write (packages-repositories.md section 4.6). `wc -l /proc/net/dev` in such a unit
/// counts more than the two header lines and `lo`, so it keeps the network; `touch` in the
/// writable directory, in its working directory, with the call's environment, exits 0, and
/// `touch` of a file beside that directory, which `ProtectSystem=strict` leaves read-only, fails.
#[test]
fn a_fetch_tool_keeps_the_network_and_writes_only_its_paths() {
    let bench = Bench::new("tool_fetch");
    let rig = &bench.rig;
    let launcher = launcher();
    let base =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("tool-fetch-{}", std::process::id()));
    let writable = base.join("work");
    std::fs::create_dir_all(&writable).expect("the writable directory");
    let paths = [writable.as_path()];
    let fetch = |unit: &str, program: &str, args: &[&str]| {
        let args: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
        launcher
            .run_tool(&ToolCall {
                network: ToolNetwork::Fetch,
                writable: Some(&paths),
                working_dir: Some(&writable),
                env: &[("HK_FIXTURE", "x")],
                ..tool(unit, Path::new(program), &args)
            })
            .expect("the launcher runs the tool")
    };

    let exit = fetch(&rig.unit(), "/usr/bin/wc", &["-l", "/proc/net/dev"]);
    let lines: u32 = String::from_utf8_lossy(&exit.stdout)
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a count");
    assert!(
        lines > 3,
        "a fetch unit sees the host's interfaces: {exit:?}"
    );

    let inside = fetch(&rig.unit(), "/usr/bin/sh", &["-c", "touch \"$HK_FIXTURE\""]);
    assert_eq!(inside.code, Some(0), "{inside:?}");
    assert!(
        writable.join("x").is_file(),
        "touch ran in the working directory"
    );

    let outside = base.join("outside");
    let refused = fetch(
        &rig.unit(),
        "/usr/bin/touch",
        &[&outside.display().to_string()],
    );
    assert_ne!(refused.code, Some(0), "{refused:?}");
    assert!(
        !outside.exists(),
        "nothing was written beside the writable directory"
    );
    let _ = std::fs::remove_dir_all(&base);
}
