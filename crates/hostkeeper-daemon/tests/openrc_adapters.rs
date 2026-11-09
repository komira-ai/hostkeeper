//! The OpenRC adapters ([`Adapters::openrc`]): the unit control's `rc-service` commands through
//! the launcher it is given, each one child process of a [`RunnerLauncher`] over the fake command
//! runner, and the observer's `rc-service <service> status` through the runner it is given, so
//! that every argument vector, its deadline and how each end is read are seen without OpenRC.

mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_daemon::adapters::{
    Adapters, OPENRC_SERVICE, RC_SERVICE_DEADLINE, RunnerLauncher, host_managed, host_rc_service,
};
use hostkeeper_daemon::cli::{HostFiles, PKI_DIR};
use hostkeeper_daemon::observer::SHOW_DEADLINE;
use hostkeeper_enroll::ManagedFile;
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_observe::{ActiveState, CommandOutput, RunError, UnitState};
use hostkeeper_openrc::OpenrcLauncher;
use hostkeeper_placed::UnitControlError;
use support::Scratch;

const DEADLINE: Duration = Duration::from_secs(7);

/// An end of a command with exit code `code` (`None`: a signal) and nothing printed.
fn exit(code: Option<i32>) -> Result<CommandOutput, RunError> {
    Ok(CommandOutput {
        exit_code: code,
        stdout: Vec::new(),
        truncated: false,
    })
}

fn argv(words: &[&str], deadline: Duration) -> (Vec<String>, Duration) {
    (words.iter().map(|w| (*w).to_owned()).collect(), deadline)
}

/// The OpenRC adapters of `platform`, the unit control over `rc`, each command bounded by
/// [`DEADLINE`], the observer over `status`, the child pids under `run_dir`.
fn openrc(
    platform: &Platform,
    rc: &FakeCommandRunner,
    status: &FakeCommandRunner,
    run_dir: &Path,
) -> Adapters {
    Adapters::openrc(
        platform,
        Arc::new(OpenrcLauncher::new(
            &run_dir.join("steps"),
            hostkeeper_daemon::testing::FakeClock::new(),
        )),
        RunnerLauncher::new(rc.clone(), DEADLINE),
        status.clone(),
        run_dir,
    )
}

/// Catches an OpenRC set that is another platform's (systemd's `systemctl` would fail on every
/// call, and the observer would watch `hostkeeper.service`): the kind, the unit directory, no
/// package check, the daemon's own service `hostkeeper`; `daemon_reload` runs nothing,
/// `stop_unit`, `start_unit` and `is_active` each run the one `rc-service` command that does it,
/// within the launcher's deadline.
#[test]
fn the_openrc_unit_control_runs_rc_service_for_each_call() {
    let scratch = Scratch::new("openrc-adapters-units");
    let rc = FakeCommandRunner::default();
    for _ in 0..3 {
        rc.then(exit(Some(0)));
    }
    let status = FakeCommandRunner::default();
    let adapters = openrc(&Platform::openrc(), &rc, &status, scratch.path());
    assert_eq!(adapters.kind, PlatformKind::Openrc);
    assert_eq!(adapters.unit_dir, Path::new("/etc/init.d"));
    assert!(adapters.package_check.is_none());
    assert_eq!(adapters.own_unit, "hostkeeper");
    assert_eq!(OPENRC_SERVICE, "hostkeeper");
    let units = &adapters.units;

    assert_eq!(units.daemon_reload(), Ok(()));
    assert_eq!(units.stop_unit("web-a"), Ok(()));
    assert_eq!(units.start_unit("web-b"), Ok(()));
    assert_eq!(units.is_active("web-b"), Ok(true));
    assert_eq!(
        rc.calls(),
        vec![
            argv(&["/sbin/rc-service", "web-a", "stop"], DEADLINE),
            argv(&["/sbin/rc-service", "web-b", "start"], DEADLINE),
            argv(&["/sbin/rc-service", "web-b", "status"], DEADLINE),
        ]
    );
    assert!(status.calls().is_empty());
}

/// Catches a failed `rc-service` read as done: an exit that is not 0 and an end by a signal are
/// each an error naming the command and why, and `status` reads them as not active.
#[test]
fn an_rc_service_that_fails_is_an_error_naming_the_command() {
    let scratch = Scratch::new("openrc-adapters-fail");
    let rc = FakeCommandRunner::default();
    rc.then(exit(Some(1)));
    rc.then(exit(None));
    rc.then(exit(Some(3)));
    let units = openrc(
        &Platform::openrc(),
        &rc,
        &FakeCommandRunner::default(),
        scratch.path(),
    )
    .units;
    assert_eq!(
        units.stop_unit("web-a"),
        Err(UnitControlError {
            action: "/sbin/rc-service web-a stop".to_owned(),
            why: "exit status 1".to_owned(),
        })
    );
    assert_eq!(
        units.start_unit("web-a"),
        Err(UnitControlError {
            action: "/sbin/rc-service web-a start".to_owned(),
            why: "ended by a signal".to_owned(),
        })
    );
    // `rc-service status` exits 3 for a stopped service.
    assert_eq!(units.is_active("web-a"), Ok(false));
}

/// Catches an observer that asks OpenRC through the unit control's launcher (bounded by minutes)
/// or looks for init scripts elsewhere than the platform's unit directory: a service with a
/// script there is asked `rc-service <service> status` through the observer's runner within the
/// observer's deadline, and its main process is the child supervise-daemon runs; one without a
/// script is not found, and nothing is run.
#[test]
fn the_openrc_observer_reads_the_status_and_the_supervised_child() {
    let scratch = Scratch::new("openrc-adapters-observer");
    let init = scratch.path().join("init.d");
    fs::create_dir(&init).expect("the init directory");
    fs::write(init.join("hostkeeper"), "#!/sbin/openrc-run\n").expect("an init script");
    let options = scratch.path().join("options").join("hostkeeper");
    fs::create_dir_all(&options).expect("its options");
    fs::write(options.join("child_pid"), "4242\n").expect("its child");
    let platform = Platform {
        unit_dir: init,
        ..Platform::openrc()
    };
    let rc = FakeCommandRunner::default();
    let status = FakeCommandRunner::default();
    status.then(exit(Some(0)));
    let adapters = openrc(&platform, &rc, &status, scratch.path());

    assert_eq!(
        adapters.manager.unit_state("hostkeeper"),
        Ok(UnitState {
            active: ActiveState::Active,
            sub: "started".to_owned(),
            main_pid: Some(4242),
        })
    );
    assert_eq!(
        adapters.manager.unit_state("web"),
        Ok(UnitState {
            active: ActiveState::NotFound,
            sub: "no init script".to_owned(),
            main_pid: None,
        })
    );
    assert_eq!(
        status.calls(),
        vec![argv(
            &["/sbin/rc-service", "hostkeeper", "status"],
            SHOW_DEADLINE
        )]
    );
    assert!(rc.calls().is_empty());
}

/// Catches a host launcher bounded tighter than an OpenRC start that waits for the daemon's
/// readiness, or a stop that waits for supervise-daemon's kill: the host's `rc-service` launcher
/// waits [`RC_SERVICE_DEADLINE`], minutes, and the host's OpenRC set is chosen for the platform.
#[test]
fn the_host_openrc_set_is_chosen_for_the_platform_and_waits_minutes() {
    assert_eq!(host_rc_service().deadline(), RC_SERVICE_DEADLINE);
    assert!(RC_SERVICE_DEADLINE > Duration::from_secs(2 * 90));
    let adapters = Adapters::for_platform(&Platform::openrc()).expect("the OpenRC set");
    assert_eq!(adapters.kind, PlatformKind::Openrc);
    assert_eq!(adapters.own_unit, "hostkeeper");
}

/// Catches an OpenRC host configured as a Mac: it reads no managed preferences, and its files
/// are the Linux ones with no console (busybox's getty reads no issue directory and has no
/// `--reload`).
#[test]
fn an_openrc_host_keeps_its_files_as_linux_with_no_console() {
    let scratch = Scratch::new("openrc-adapters-files");
    let managed = ManagedFile::new(&scratch.path().join("managed.plist"), 0);
    assert!(host_managed(PlatformKind::Openrc, managed).is_none());

    for present in [false, true] {
        let linux = HostFiles::for_platform(PlatformKind::Linux, |_| present);
        let rc = HostFiles::for_platform(PlatformKind::Openrc, |_| present);
        assert_eq!(rc.console, None);
        assert_eq!(rc.managed_config, None);
        assert_eq!(
            (&rc.ca_bundle, &rc.gh, &rc.trusted_root, &rc.pki),
            (&linux.ca_bundle, &linux.gh, &linux.trusted_root, &linux.pki)
        );
        assert_eq!(rc.pki, Path::new(PKI_DIR));
    }
}
