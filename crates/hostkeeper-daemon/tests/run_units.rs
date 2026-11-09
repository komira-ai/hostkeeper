//! What the daemon's start does around the package manager's host: it removes the `policy-rc.d`
//! file a killed run left, and starts the run units of apt services that no enablement link
//! starts (packages-native.md, the units of a package).

mod support;
mod support_apt;

use std::fs;
use std::path::PathBuf;

use hostkeeper_daemon::boot::{BootReport, Stage};
use hostkeeper_daemon::start;
use hostkeeper_install::repository::policy;
use hostkeeper_placed::testing::UnitCall;
use hostkeeper_proto::hostkeeper::release::v1::{Manifest, Target};
use support::OWN;
use support_apt::seen::{install_of, succeeded, unit_calls};
use support_apt::{AptRig, UNIT, package, unlinked_package};

/// The start of the daemon on `apt`'s rig, as a restart of the daemon.
fn restart(apt: &AptRig) -> hostkeeper_daemon::start::Started {
    start::startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts")
}

/// Where the rig's host root holds the `policy-rc.d` file.
fn policy_file(apt: &AptRig) -> PathBuf {
    apt.rig.scratch.path().join(policy::POLICY_RC_D)
}

/// Proves a run unit whose unit file has no `[Install]` section is started by hostkeeper at the
/// cutover of its install (the built-in steps), and started again by the start pass of a daemon
/// restart, in the `StartRunUnits` stage before the `boot` steps.
///
/// Catches: a unit nothing starts after a reboot because the package enables nothing.
#[test]
fn a_run_unit_with_no_install_section_is_started_at_cutover_and_at_every_start() {
    let apt = AptRig::new("run-units-unlinked");
    let one = apt.publish(unlinked_package("1.0.0-1"));
    let daemon = apt.daemon();
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert!(
        unit_calls(&apt).contains(&UnitCall::Start(UNIT.to_owned())),
        "the cutover starts it"
    );
    drop(daemon);

    apt.rig.placed_log.clear();
    let started = restart(&apt);
    assert_eq!(unit_calls(&apt), [UnitCall::Start(UNIT.to_owned())]);
    let stages = &started.boot_report().stages;
    let at = |stage| stages.iter().position(|s| *s == stage).expect("a stage");
    assert!(at(Stage::StartRunUnits) < at(Stage::BootSteps));
}

/// Proves a run unit whose unit file has an `[Install]` target (so the installer links it for
/// systemd) is not started by the start pass.
///
/// Catches: a start pass that restarts what systemd already starts at boot.
#[test]
fn a_run_unit_with_an_install_section_is_left_to_systemd_at_start() {
    let apt = AptRig::new("run-units-linked");
    let one = apt.publish(package("1.0.0-1"));
    let daemon = apt.daemon();
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    drop(daemon);

    apt.rig.placed_log.clear();
    let started = restart(&apt);
    assert_eq!(unit_calls(&apt), []);
    assert!(started.boot_report().stages.contains(&Stage::StartRunUnits));
}

/// Proves the start removes a `policy-rc.d` file that is byte-equal to the one hostkeeper writes
/// (a killed run left it), in the `ClearPolicyRcD` stage right after the registration socket, and
/// keeps one an operator wrote.
///
/// Catches: a stale policy file that keeps every service from starting after a package-manager
/// run was killed, or a start that deletes the host's own policy.
#[test]
fn the_start_removes_a_stale_policy_rc_d_of_ours_and_keeps_a_foreign_one() {
    let apt = AptRig::new("run-units-policy");
    let file = policy_file(&apt);
    fs::create_dir_all(file.parent().expect("a directory")).expect("usr/sbin");
    fs::write(&file, policy::TEXT).expect("a stale file");
    let started = restart(&apt);
    assert!(!file.exists(), "the stale file is removed");
    let stages = &started.boot_report().stages;
    assert_eq!(
        stages[..2],
        [Stage::UnlinkRegistrationSocket, Stage::ClearPolicyRcD]
    );
    drop(started);

    fs::write(&file, "#!/bin/sh\nexit 0\n").expect("the operator's file");
    let _ = restart(&apt);
    assert_eq!(
        fs::read_to_string(&file).expect("kept"),
        "#!/bin/sh\nexit 0\n"
    );
}

/// Proves a run unit that cannot be started is an alert naming the service and the unit, and
/// that the start pass still records its stage and goes on.
///
/// Catches: a failed start that stops the daemon's start, or one that no alert reports.
#[test]
fn a_run_unit_that_cannot_be_started_is_an_alert_and_the_start_goes_on() {
    let apt = AptRig::new("run-units-failing");
    let one = apt.publish(unlinked_package("1.0.0-1"));
    let (daemon, log) = support::alerting(apt.daemon());
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.rig.units.fail_start(UNIT);
    let mut report = BootReport::default();
    daemon.start_run_units(&mut report);
    assert_eq!(report.stages, [Stage::StartRunUnits]);
    let said = support::lines(&log).join("\n");
    assert!(
        said.contains(&format!("service=web unit={UNIT}: it could not be started")),
        "{said}"
    );
}

/// Proves the start pass starts nothing of a `current` version staged by another installer than
/// `apt`, even when its tree gives a run unit no link.
///
/// Catches: a start pass that starts the units of a `macos-pkg` version (launchd loads them) or
/// of an archive version.
#[test]
fn a_current_version_of_another_installer_is_left_alone() {
    let apt = AptRig::new("run-units-other-installer");
    let dir = apt.rig.packages.join("web").join("1.0.0");
    fs::create_dir_all(dir.join("tree")).expect("the version directory");
    let entry = Target {
        installer: "macos-pkg".to_owned(),
        manifest: Some(Manifest {
            run: vec![UNIT.to_owned()],
            ..Manifest::default()
        }),
        ..Target::default()
    };
    hostkeeper_daemon::target::write(&dir, &entry).expect("target.json");
    std::os::unix::fs::symlink("1.0.0", apt.rig.packages.join("web").join("current"))
        .expect("current");
    let mut report = BootReport::default();
    apt.daemon().start_run_units(&mut report);
    assert_eq!(unit_calls(&apt), []);
}

/// Proves a `policy-rc.d` that cannot be looked at (its directory is a file) is an alert, and
/// that the stage is still recorded and the start goes on.
///
/// Catches: a start that stops at an unreadable policy file, or one whose failure no alert says.
#[test]
fn a_policy_file_that_cannot_be_looked_at_is_an_alert_and_the_start_goes_on() {
    let apt = AptRig::new("run-units-policy-unreadable");
    let file = policy_file(&apt);
    let sbin = file.parent().expect("usr/sbin");
    fs::create_dir_all(sbin.parent().expect("usr")).expect("usr");
    fs::write(sbin, "not a directory").expect("usr/sbin as a file");
    let (daemon, log) = support::alerting(apt.daemon());
    let mut report = BootReport::default();
    daemon.clear_policy_rc_d(&mut report);
    assert_eq!(report.stages, [Stage::ClearPolicyRcD]);
    let said = support::lines(&log).join("\n");
    assert!(
        said.contains("the stale policy-rc.d could not be removed"),
        "{said}"
    );
}
