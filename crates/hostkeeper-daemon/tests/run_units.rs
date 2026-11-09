//! What the daemon's start does around the package manager's host: it removes the `policy-rc.d`
//! file a killed run left, and starts the run units of apt services that no enablement link
//! starts (packages-native.md, the units of a package).

mod support;
mod support_apt;

use std::fs;
use std::path::PathBuf;

use hostkeeper_daemon::boot::Stage;
use hostkeeper_daemon::start;
use hostkeeper_install::repository::policy;
use hostkeeper_placed::testing::UnitCall;
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
