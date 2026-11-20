//! Tests of the host's reboot, over a fake of the program that asks for it.

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// Proves: under systemd the reboot runs `systemctl` once with exactly `reboot` and succeeds on
/// exit 0.
#[test]
fn systemd_runs_systemctl_reboot_once() {
    let tool = FakeLaunchctl::new();
    tool.always(&args(&["reboot"]), Ok(LaunchctlOutput::success("")));
    let rebooter = HostRebooter::systemd(Box::new(tool.clone()));
    assert_eq!(rebooter.reboot(), Ok(()));
    assert_eq!(tool.calls(), vec![args(&["reboot"])]);
}

/// Proves: under OpenRC the reboot runs `reboot` once with no arguments and succeeds on exit 0.
#[test]
fn openrc_runs_reboot_once_with_no_arguments() {
    let tool = FakeLaunchctl::new();
    tool.always(&[], Ok(LaunchctlOutput::success("")));
    let rebooter = HostRebooter::openrc(Box::new(tool.clone()));
    assert_eq!(rebooter.reboot(), Ok(()));
    assert_eq!(tool.calls(), vec![Vec::<String>::new()]);
}

/// Proves: a `systemctl` that exits 1 fails the reboot, the reason naming the step, the exit and
/// the first line of its standard error.
///
/// Catches: a refused reboot reported as asked for, so the run waits for a reboot that never
/// comes.
#[test]
fn a_failed_systemctl_names_its_exit_and_first_stderr_line() {
    let tool = FakeLaunchctl::new();
    tool.always(
        &args(&["reboot"]),
        Ok(LaunchctlOutput::exit(
            1,
            "Failed to reboot: Access denied\nsecond line\n",
        )),
    );
    let rebooter = HostRebooter::systemd(Box::new(tool));
    assert_eq!(
        rebooter.reboot(),
        Err(RebootError::Failed(
            "systemctl reboot failed with exit code 1: Failed to reboot: Access denied".to_owned()
        ))
    );
}

/// Proves: an OpenRC `reboot` that exits 2 with nothing on its standard error fails with the
/// step and the exit alone.
#[test]
fn a_failed_openrc_reboot_names_its_exit() {
    let tool = FakeLaunchctl::new();
    tool.always(&[], Ok(LaunchctlOutput::exit(2, "")));
    let rebooter = HostRebooter::openrc(Box::new(tool));
    assert_eq!(
        rebooter.reboot(),
        Err(RebootError::Failed(
            "reboot failed with exit code 2".to_owned()
        ))
    );
}

/// Proves: a program that cannot be started fails the reboot, and a program that does not end by
/// its deadline fails it too, each naming the step.
#[test]
fn a_program_that_does_not_run_or_does_not_end_fails_the_reboot() {
    let unscripted = HostRebooter::systemd(Box::new(FakeLaunchctl::new()));
    let Err(RebootError::Failed(reason)) = unscripted.reboot() else {
        panic!("an unscripted program must fail the reboot");
    };
    assert_eq!(
        reason,
        "systemctl reboot could not be run: the fake has no answer"
    );

    let slow = FakeLaunchctl::new();
    slow.always(
        &args(&["reboot"]),
        Err(LaunchctlError::TimedOut {
            args: "reboot".to_owned(),
        }),
    );
    let rebooter = HostRebooter::systemd(Box::new(slow));
    assert_eq!(
        rebooter.reboot(),
        Err(RebootError::Failed(
            "systemctl reboot did not finish within its deadline".to_owned()
        ))
    );
}

/// Proves: systemd and OpenRC hosts have a reboot and a Mac has none here.
#[test]
fn only_the_linux_platforms_have_a_reboot() {
    assert!(HostRebooter::for_platform(PlatformKind::Linux).is_some());
    assert!(HostRebooter::for_platform(PlatformKind::Openrc).is_some());
    assert!(HostRebooter::for_platform(PlatformKind::Macos).is_none());
}
