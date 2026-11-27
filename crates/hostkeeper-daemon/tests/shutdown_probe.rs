//! Whether the system is going down at SIGTERM (`hostkeeper_daemon::shutdown_probe`): systemd's
//! state and queued jobs through a fake `systemctl`, OpenRC's runlevel from a scratch run
//! directory, and a Mac's advisory answer. No real program runs.

mod support;

use std::fs;

use hostkeeper_daemon::shutdown_probe::{
    Advisory, GoingDown, IS_SYSTEM_RUNNING, LIST_JOBS, OPENRC_SOFTLEVEL, Openrc, ShutdownProbe,
    Systemd, for_platform, shutdown_job,
};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput, PlatformKind};
use support::Scratch;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// A line of `systemctl list-jobs --no-legend` for `unit` (named less its `.target`) of `kind`.
fn job(id: u32, unit: &str, kind: &str) -> String {
    format!("{id} {unit}{} {kind} waiting\n", ".target")
}

/// What `is-system-running` printed, with the exit status systemd gives for it.
fn state(text: &str, code: i32) -> LaunchctlOutput {
    LaunchctlOutput {
        code: Some(code),
        stdout: format!("{text}\n").into_bytes(),
        stderr: Vec::new(),
    }
}

/// The probe over a fake `systemctl` answering `running` with `state`, and `list-jobs` with
/// `jobs` when given.
fn systemd(
    running: LaunchctlOutput,
    jobs: Option<Result<LaunchctlOutput, LaunchctlError>>,
) -> (Systemd, FakeLaunchctl) {
    let tool = FakeLaunchctl::new();
    tool.then(&args(&IS_SYSTEM_RUNNING), Ok(running));
    if let Some(answer) = jobs {
        tool.then(&args(&LIST_JOBS), answer);
    }
    (Systemd::new(Box::new(tool.clone())), tool)
}

/// Proves `stopping` is going down whatever its exit status, and no job list is asked for then.
///
/// Catches: the state read from the exit status, which is non-zero for `stopping`.
#[test]
fn a_stopping_system_is_going_down() {
    let (probe, tool) = systemd(state("stopping", 1), None);
    assert_eq!(
        probe.going_down(),
        GoingDown::Yes("the system is stopping".to_owned())
    );
    assert_eq!(tool.calls(), [args(&IS_SYSTEM_RUNNING)]);
}

/// Proves a running or degraded system with a start job of the reboot, poweroff, halt or kexec
/// target queued is going down, and with none, or only another job of those targets, is not.
///
/// Catches: a reboot queued but not yet stopping read as a daemon restart, or any job read as a
/// shutdown.
#[test]
fn a_queued_shutdown_job_is_going_down_and_nothing_else_is() {
    for target in ["reboot", "poweroff", "halt", "kexec"] {
        let jobs = format!(
            "{}{}",
            job(9, "multi-user", "start"),
            job(10, target, "start")
        );
        let (probe, _) = systemd(
            state("degraded", 1),
            Some(Ok(LaunchctlOutput::success(&jobs))),
        );
        assert_eq!(
            probe.going_down(),
            GoingDown::Yes(format!("a start job of the {target} target is queued"))
        );
    }
    let jobs = format!(
        "{}{}",
        job(11, "reboot", "stop"),
        job(12, "rescue", "start")
    );
    let (probe, tool) = systemd(
        state("running", 0),
        Some(Ok(LaunchctlOutput::success(&jobs))),
    );
    assert_eq!(
        probe.going_down(),
        GoingDown::No("the system is running, with no shutdown job queued".to_owned())
    );
    assert_eq!(tool.calls(), [args(&IS_SYSTEM_RUNNING), args(&LIST_JOBS)]);
}

/// Proves a job list's lines are read by their second and third words: a short line, a unit with
/// no `.target`, and an empty list find nothing.
///
/// Catches: a line of another shape read as a shutdown job, or a panic on it.
#[test]
fn a_job_line_of_another_shape_is_no_shutdown_job() {
    assert_eq!(shutdown_job(""), None);
    assert_eq!(shutdown_job("1\n"), None);
    assert_eq!(shutdown_job("1 reboot start waiting\n"), None);
    assert_eq!(shutdown_job(&job(2, "reboot", "")), None);
    assert_eq!(shutdown_job(&job(3, "poweroff", "start")), Some("poweroff"));
}

/// Proves a `systemctl` that does not run, for either question, cannot tell.
///
/// Catches: a probe failure read as going down, which would write a marker a crash then hides
/// behind.
#[test]
fn a_systemctl_that_does_not_run_cannot_tell() {
    let tool = FakeLaunchctl::new();
    let probe = Systemd::new(Box::new(tool.clone()));
    assert_eq!(
        probe.going_down(),
        GoingDown::Unknown(
            "systemctl is-system-running did not run: cannot run launchctl is-system-running: \
             the fake has no answer"
                .to_owned()
        )
    );
    let timed_out = Err(LaunchctlError::TimedOut {
        args: "list-jobs --no-legend".to_owned(),
    });
    let (probe, _) = systemd(state("running", 0), Some(timed_out));
    assert_eq!(
        probe.going_down(),
        GoingDown::Unknown(
            "systemctl list-jobs --no-legend did not run: launchctl list-jobs --no-legend did not \
             finish within its deadline and was killed"
                .to_owned()
        )
    );
}

/// Proves OpenRC changing to `shutdown` or `reboot` is going down, any other runlevel is not,
/// and a run directory with no `softlevel` cannot tell; `for_platform` reads the directory given.
///
/// Catches: an Alpine host's restart of the daemon read as a shutdown, or the reverse.
#[test]
fn openrc_is_going_down_while_changing_to_shutdown_or_reboot() {
    let run = Scratch::new("probe-openrc");
    let probe = Openrc::new(run.path());
    let missing = probe.going_down();
    assert!(matches!(&missing, GoingDown::Unknown(why) if why.contains(OPENRC_SOFTLEVEL)));
    for level in ["shutdown", "reboot"] {
        fs::write(run.path().join(OPENRC_SOFTLEVEL), format!("{level}\n")).expect("write");
        assert_eq!(
            probe.going_down(),
            GoingDown::Yes(format!("OpenRC is changing to the runlevel {level}"))
        );
    }
    fs::write(run.path().join(OPENRC_SOFTLEVEL), "default").expect("write");
    assert_eq!(
        for_platform(PlatformKind::Openrc, run.path()).going_down(),
        GoingDown::No("OpenRC's runlevel is default".to_owned())
    );
}

/// Proves a Mac counts every SIGTERM, and `for_platform` gives it that probe; a Linux host's
/// probe is made without running anything.
///
/// Catches: a Mac that never writes its advisory marker.
#[test]
fn a_mac_counts_every_sigterm() {
    let yes = GoingDown::Yes("launchd sent SIGTERM (advisory on a Mac)".to_owned());
    assert_eq!(Advisory.going_down(), yes);
    let run = Scratch::new("probe-mac");
    assert_eq!(
        for_platform(PlatformKind::Macos, run.path()).going_down(),
        yes
    );
    let _linux = for_platform(PlatformKind::Linux, run.path());
}
