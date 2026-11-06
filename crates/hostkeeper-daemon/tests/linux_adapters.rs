//! The Linux adapters' unit control: `systemctl` through the launcher [`Adapters::linux`] is
//! given, each command one child process of a [`RunnerLauncher`], here over the fake command
//! runner, so that every argument vector, its deadline and how each end is read are seen without
//! a service manager.

use std::path::Path;
use std::time::Duration;

use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE, host_systemctl};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_observe::{CommandOutput, RunError};
use hostkeeper_placed::{CommandLauncher, UnitControlError};

const DEADLINE: Duration = Duration::from_secs(7);

/// An end of a command with exit code `code` (`None`: a signal) and nothing printed.
fn exit(code: Option<i32>) -> Result<CommandOutput, RunError> {
    Ok(CommandOutput {
        exit_code: code,
        stdout: Vec::new(),
        truncated: false,
    })
}

/// The Linux adapters over `runner`, each command bounded by [`DEADLINE`].
fn linux(runner: &FakeCommandRunner) -> Adapters {
    Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(runner.clone(), DEADLINE),
    )
}

fn argv(words: &[&str]) -> (Vec<String>, Duration) {
    (words.iter().map(|w| (*w).to_owned()).collect(), DEADLINE)
}

/// Catches a Linux set whose unit control is not `systemctl` (the unwired one refused every
/// call, so a hooked update stopped before `cutover` and a rollback could not start the old
/// version): `daemon_reload`, `stop_unit`, `start_unit` and `is_active` each run the one
/// `systemctl` command that does it, the unit after `--`, within the launcher's deadline.
#[test]
fn the_linux_unit_control_runs_systemctl_for_each_call() {
    let runner = FakeCommandRunner::default();
    for _ in 0..4 {
        runner.then(exit(Some(0)));
    }
    let adapters = linux(&runner);
    assert_eq!(adapters.kind, PlatformKind::Linux);
    assert_eq!(adapters.unit_dir, Path::new("/etc/systemd/system"));
    assert!(adapters.package_check.is_none());
    let units = &adapters.units;

    assert_eq!(units.daemon_reload(), Ok(()));
    assert_eq!(units.stop_unit("web-a.service"), Ok(()));
    assert_eq!(units.start_unit("web-b.service"), Ok(()));
    assert_eq!(units.is_active("web-b.service"), Ok(true));
    assert_eq!(
        runner.calls(),
        vec![
            argv(&["systemctl", "daemon-reload"]),
            argv(&["systemctl", "stop", "--", "web-a.service"]),
            argv(&["systemctl", "start", "--", "web-b.service"]),
            argv(&["systemctl", "is-active", "--quiet", "--", "web-b.service"]),
        ]
    );
}

/// Catches a failed `systemctl` read as done: an exit that is not 0, an end by a signal, a
/// command that did not finish by its deadline, and one that could not start are each an error
/// naming the command and why, and `is-active` reads every such end as not active.
#[test]
fn a_systemctl_that_fails_is_an_error_naming_the_command() {
    let runner = FakeCommandRunner::default();
    runner.then(exit(Some(1)));
    runner.then(exit(None));
    runner.then(Err(RunError::TimedOut {
        program: "systemctl".to_owned(),
    }));
    runner.then(Err(RunError::Failed {
        program: "systemctl".to_owned(),
        why: "No such file or directory (os error 2)".to_owned(),
    }));
    runner.then(exit(Some(3)));
    runner.then(exit(None));
    let units = linux(&runner).units;
    let failed = |action: &str, why: &str| {
        Err(UnitControlError {
            action: action.to_owned(),
            why: why.to_owned(),
        })
    };

    assert_eq!(
        units.daemon_reload(),
        failed("systemctl daemon-reload", "exit status 1")
    );
    assert_eq!(
        units.stop_unit("web-a.service"),
        failed("systemctl stop -- web-a.service", "ended by a signal")
    );
    assert_eq!(
        units.start_unit("web-b.service"),
        failed(
            "systemctl start -- web-b.service",
            "systemctl did not finish within its deadline and was killed"
        )
    );
    assert_eq!(
        units.start_unit("web-b.service"),
        failed(
            "systemctl start -- web-b.service",
            "cannot run systemctl: No such file or directory (os error 2)"
        )
    );
    // `systemctl is-active` exits 3 for an inactive unit.
    assert_eq!(units.is_active("web-b.service"), Ok(false));
    assert_eq!(units.is_active("web-b.service"), Ok(false));
    assert_eq!(runner.calls().len(), 6);
}

/// Catches a runner launcher that passes on other words than it was given, or an empty argument
/// vector as a command: the words reach the runner as they are, and the runner's refusal of
/// nothing to run is the error.
#[test]
fn the_runner_launcher_passes_the_words_as_they_are() {
    let runner = FakeCommandRunner::default();
    runner.then(exit(Some(0)));
    let launcher = RunnerLauncher::new(runner.clone(), DEADLINE);
    assert_eq!(launcher.deadline(), DEADLINE);
    assert_eq!(launcher.run(&["prog", "a b", ""]), Ok(()));
    assert_eq!(
        launcher.run(&[]),
        Err("there is no program to run".to_owned())
    );
    assert_eq!(
        runner.calls(),
        vec![argv(&["prog", "a b", ""]), (Vec::new(), DEADLINE)]
    );
}

/// Catches a host launcher bounded tighter than systemd's own default start and stop timeouts
/// (90 s each), which would report a stop or start that was still going as failed.
#[test]
fn the_host_systemctl_waits_past_systemds_default_timeouts() {
    assert_eq!(host_systemctl().deadline(), SYSTEMCTL_DEADLINE);
    assert!(SYSTEMCTL_DEADLINE > Duration::from_secs(2 * 90));
}
