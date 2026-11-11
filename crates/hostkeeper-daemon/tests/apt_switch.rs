//! The daemon's switch for an `apt` service when the work after the installer's `activate`
//! fails, when the host's files fail, and when two services name one package
//! (packages-repositories.md section 4.5; design sections 5.4 and 5.5).

mod support;
mod support_apt;

use std::sync::{Arc, Mutex};

use hostkeeper_daemon::{Daemon, Placement, Ports, Reply};
use hostkeeper_hook::{Class, Step};
use hostkeeper_placed::testing::FaultFs;
use hostkeeper_placed::{CommandLauncher, LaunchError, Systemctl};
use support::{OWN, alerting, config, install, lines, roots};
use support_apt::seen::{failed, held, install_hooked, install_of, linked, recorded, succeeded};
use support_apt::{
    AptRig, HOOK_PROGRAM, SERVICE, UNIT, entry, hooked_package, package, with_entry,
};

/// A daemon on the rig whose reads of the host's files fail at the `n`-th, once.
fn daemon_failing_read(apt: &AptRig, n: u64) -> (Daemon, hostkeeper_daemon::testing::Log) {
    let fs =
        FaultFs::new(apt.rig.scratch.path(), apt.rig.placed_log.clone()).failing_read(n, false);
    let ports = Ports {
        placement: Placement {
            roots: roots(),
            fs: Arc::new(fs),
        },
        ..apt.ports()
    };
    alerting(Daemon::new(config(), apt.rig.dirs(), ports, OWN))
}

/// A `systemctl` whose every `stop` exits `stop_exit` and every other command exits 0; it keeps
/// each argument vector.
struct StopExits {
    stop_exit: i32,
    runs: Arc<Mutex<Vec<String>>>,
}

impl CommandLauncher for StopExits {
    fn run(&self, argv: &[&str]) -> Result<(), LaunchError> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(argv.join(" "));
        match argv.get(1) {
            Some(&"stop") => Err(LaunchError::Exit(self.stop_exit)),
            _ => Ok(()),
        }
    }
}

/// A daemon on the rig whose unit control is `systemctl` over [`StopExits`], with its log, and
/// the commands that `systemctl` was asked to run.
fn daemon_whose_stop_exits(
    apt: &AptRig,
    stop_exit: i32,
) -> (
    Daemon,
    hostkeeper_daemon::testing::Log,
    Arc<Mutex<Vec<String>>>,
) {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let launcher = StopExits {
        stop_exit,
        runs: Arc::clone(&runs),
    };
    let ports = Ports {
        units: Arc::new(Systemctl::new(launcher)),
        ..apt.ports()
    };
    let (daemon, log) = alerting(Daemon::new(config(), apt.rig.dirs(), ports, OWN));
    (daemon, log, runs)
}

/// The last line of a `prepare` that failed on the host's state.
const STATE_INVALID: &str = r#"{"class":"environment","reason":"state_invalid"}"#;

/// Proves the rollback of a first install whose `prepare` failed, so that its package was never
/// installed and the service manager has not loaded its run unit (`systemctl stop` exits 5), is
/// no failure of the switch: the order ends `Failed` before the commit with the class and the
/// reason `prepare` gave (`environment`, `state_invalid`, which the coordinator tries again), not
/// interrupted with `link_failed`; the switch is tried once, nothing is installed and no record
/// is left.
///
/// Catches: a first install whose rollback fails `link_failed` after five tries of the switch
/// because its run unit does not exist yet.
#[test]
fn a_first_install_whose_prepare_fails_ends_with_the_prepares_reason_when_its_unit_is_not_loaded() {
    let apt = AptRig::new("apt-first-install-prepare-fails");
    let (daemon, log, runs) = daemon_whose_stop_exits(&apt, 5);
    apt.rig
        .launcher
        .replies(SERVICE, Step::Prepare, &[(1, STATE_INVALID)]);
    let one = apt.publish(hooked_package("1.0.0-1"));
    let command = [&format!("/{HOOK_PROGRAM}")[..], "update-hook"];
    assert_eq!(
        daemon.order(&install_hooked("1.0.0", "1.0.0-1", &one, 1, &command)),
        failed(Class::Environment, "state_invalid")
    );
    let retried = lines(&log)
        .iter()
        .filter(|line| line.contains("alert=switch_retried"))
        .count();
    assert_eq!(retried, 0);
    assert_eq!(
        *runs.lock().expect("the runs"),
        vec![format!("systemctl stop -- {UNIT}")]
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current"), None);
    assert_eq!(recorded(&daemon), None);
}

/// Proves a `systemctl stop` that fails in another way (exit 1) still fails the switch of that
/// same first install: tried five times, then the order is interrupted with `link_failed`.
///
/// Catches: every failure of a stop read as a unit that is not loaded.
#[test]
fn a_first_install_whose_stop_really_fails_is_still_interrupted_link_failed() {
    let apt = AptRig::new("apt-first-install-stop-fails");
    let (daemon, log, runs) = daemon_whose_stop_exits(&apt, 1);
    apt.rig
        .launcher
        .replies(SERVICE, Step::Prepare, &[(1, STATE_INVALID)]);
    let one = apt.publish(hooked_package("1.0.0-1"));
    let command = [&format!("/{HOOK_PROGRAM}")[..], "update-hook"];
    let reply = daemon.order(&install_hooked("1.0.0", "1.0.0-1", &one, 1, &command));
    let Reply::Interrupted { refusal, .. } = reply else {
        panic!("not interrupted: {reply:?}");
    };
    assert_eq!(refusal.token(), "link_failed");
    let retried = lines(&log)
        .iter()
        .filter(|line| line.contains("alert=switch_retried") && line.contains("exit status 1"))
        .count();
    assert_eq!(retried, 4);
    assert_eq!(
        *runs.lock().expect("the runs"),
        vec![format!("systemctl stop -- {UNIT}"); 5]
    );
}

/// Proves a first install whose unit does not start after the package was activated is rolled
/// back whole: the verdict is `Failed`, `release`, `unit_inactive`; the package is purged and
/// unheld, `current` (which the activation had switched to the version) is removed, and
/// `placed.json` holds no record.
///
/// Catches: a rolled-back first install that leaves `current` naming a version whose package is
/// gone.
#[test]
fn a_first_install_whose_unit_does_not_start_is_rolled_back_with_current_removed() {
    let apt = AptRig::new("apt-first-install-unit-down");
    apt.rig.units.fail_start(UNIT);
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        failed(Class::Release, "unit_inactive")
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current"), None);
    assert_eq!(recorded(&daemon), None);
    assert!(apt.host.labels().contains(&"dpkg --purge".to_owned()));
}

/// Proves a rollback of a first install that cannot stop the service's unit is not a verdict: the
/// switch is tried five times (four lines `switch_retried` in the log) and the order is
/// interrupted with `link_failed`, the step still pending; the package is not purged, and the
/// record stays, so that the order sent again does the rollback.
///
/// Catches: a rollback that purges a package whose unit still runs, or that ends the order
/// with a verdict when the rollback did not happen.
#[test]
fn a_rollback_that_cannot_stop_the_unit_is_interrupted_and_purges_nothing() {
    let apt = AptRig::new("apt-rollback-stop-fails");
    apt.rig.units.fail_stop(UNIT);
    let (daemon, log) = alerting(apt.daemon());
    let one = apt.publish(package("1.0.0-1"));
    apt.host
        .edit(|state| state.faults.script_fails_for = Some("1.0.0-1".to_owned()));
    let reply = daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1));
    let Reply::Interrupted { refusal, .. } = reply else {
        panic!("not interrupted: {reply:?}");
    };
    assert_eq!(refusal.token(), "link_failed");
    let retried = lines(&log)
        .iter()
        .filter(|line| line.contains("alert=switch_retried") && line.contains("the switch Back"))
        .count();
    assert_eq!(retried, 4);
    assert!(!apt.host.labels().contains(&"dpkg --purge".to_owned()));
    assert!(recorded(&daemon).is_some());
}

/// Proves a failure of the host's files in the switch is tried again, not a verdict of the
/// package: when the read of `placed.json` that the installer's `activate` makes fails once, the
/// switch is tried again after the daemon's wait, logs `switch_retried`, and the install ends
/// `Succeeded` with the package installed and held.
///
/// Catches: an `io_error` of the record that fails an install the second try would complete.
#[test]
fn an_activate_whose_record_cannot_be_read_once_is_tried_again() {
    let apt = AptRig::new("apt-activate-io-retry");
    // The reads of placed.json in an order: 1 the order's own, 2 the stage's, 3 the activation's.
    let (daemon, log) = daemon_failing_read(&apt, 3);
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    let retried = lines(&log)
        .iter()
        .filter(|line| line.contains("alert=switch_retried"))
        .count();
    assert_eq!(retried, 1);
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
}

/// Proves a stage whose read of the record fails is the order's verdict, not a refusal: `Failed`
/// before the commit, `environment`, `io_error`, and nothing is installed or staged.
///
/// Catches: a host-file failure of the stage reported with the class of the release.
#[test]
fn a_stage_whose_record_cannot_be_read_is_failed_environment_io_error() {
    let apt = AptRig::new("apt-stage-io");
    let (daemon, _) = daemon_failing_read(&apt, 2);
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        failed(Class::Environment, "io_error")
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(recorded(&daemon), None);
    assert!(!apt.version_dir("1.0.0").exists());
}

/// Proves one package is one service's: a second service whose entry names the package that
/// `web` holds ends `Failed`, `release`, `package_conflict` at the stage, and the first service
/// is untouched.
///
/// Catches: two services whose purge and hold apply to the same package.
#[test]
fn a_package_another_service_holds_is_a_conflict() {
    let apt = AptRig::new("apt-package-conflict");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    let mut second = entry("1.0.0-1", &one);
    second
        .manifest
        .as_mut()
        .expect("a manifest")
        .run
        .clone_from(&vec!["api.service".to_owned()]);
    let order = with_entry(install("api", "1.0.0", &one, 1), &second);
    assert_eq!(
        daemon.order(&order),
        failed(Class::Release, "package_conflict")
    );
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
}
