//! The tests of the `apk` installer: the arguments of each step in order, the pin of the packages
//! hostkeeper installs, the kernel-only security scope and the failures, over scripted programs.

use std::collections::BTreeSet;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};

use super::*;
use crate::maintenance_linux::REASON_INSTALL_FAILED;

const LISTING: &str = "Installed:                                Available:\n\
hostkeeper-0.1.80-r0           < 0.1.81-r0\n\
linux-lts-6.12.1-r0           < 6.12.5-r0\n\
busybox-1.36.1-r29            < 1.36.1-r30\n";

const NO_OWN_LISTING: &str = "Installed:                                Available:\n\
linux-lts-6.12.1-r0           < 6.12.5-r0\n\
busybox-1.36.1-r29            < 1.36.1-r30\n";

const NO_KERNEL_LISTING: &str = "Installed:                                Available:\n\
hostkeeper-0.1.80-r0           < 0.1.81-r0\n\
busybox-1.36.1-r29            < 1.36.1-r30\n";

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

struct Ports {
    refresh: FakeLaunchctl,
    list: FakeLaunchctl,
    install: FakeLaunchctl,
}

/// An installer over three scripted programs; refresh and listing succeed, the listing with
/// `text`, and every run of the install port that is scripted in `install_ok` succeeds.
fn installer(text: &str, lock: u64, install_ok: &[&[&str]]) -> (ApkUpgrade, Ports) {
    let ports = Ports {
        refresh: FakeLaunchctl::new(),
        list: FakeLaunchctl::new(),
        install: FakeLaunchctl::new(),
    };
    let wait = lock.to_string();
    ports.refresh.always(
        &args(&["--wait", &wait, "update", "-q"]),
        Ok(LaunchctlOutput::success("")),
    );
    ports.list.always(
        &args(&["version", "-l", "<"]),
        Ok(LaunchctlOutput::success(text)),
    );
    for call in install_ok {
        ports
            .install
            .always(&args(call), Ok(LaunchctlOutput::success("")));
    }
    let upgrader = ApkUpgrade::new(
        Box::new(ports.refresh.clone()),
        Box::new(ports.list.clone()),
        Box::new(ports.install.clone()),
        lock,
    );
    (upgrader, ports)
}

fn run(upgrader: &ApkUpgrade, scope: LinuxScope) -> Result<(), KindError> {
    let own: BTreeSet<String> = ["hostkeeper".to_owned()].into();
    upgrader.upgrade(&Upgrade {
        scope,
        own_packages: &own,
    })
}

/// Proves the `all` scope refreshes, lists, pins the own package at its installed version and
/// upgrades everything, with exactly these arguments in this order.
#[test]
fn all_scope_refreshes_lists_pins_and_upgrades() {
    let (upgrader, ports) = installer(
        LISTING,
        300,
        &[
            &["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"],
            &["--wait", "300", "upgrade", "-q"],
        ],
    );
    assert_eq!(run(&upgrader, LinuxScope::All), Ok(()));
    assert_eq!(
        ports.refresh.calls(),
        vec![args(&["--wait", "300", "update", "-q"])]
    );
    assert_eq!(ports.list.calls(), vec![args(&["version", "-l", "<"])]);
    assert_eq!(
        ports.install.calls(),
        vec![
            args(&["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"]),
            args(&["--wait", "300", "upgrade", "-q"]),
        ]
    );
}

/// Proves the `security` scope pins the own package and upgrades the kernel alone, not the other
/// packages with an update.
#[test]
fn security_scope_upgrades_the_kernel_only() {
    let (upgrader, ports) = installer(
        LISTING,
        300,
        &[
            &["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"],
            &["--wait", "300", "upgrade", "-q", "linux-lts"],
        ],
    );
    assert_eq!(run(&upgrader, LinuxScope::Security), Ok(()));
    assert_eq!(
        ports.install.calls(),
        vec![
            args(&["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"]),
            args(&["--wait", "300", "upgrade", "-q", "linux-lts"]),
        ]
    );
}

/// Proves the `security` scope with no kernel update runs no upgrade.
#[test]
fn security_scope_without_a_kernel_installs_nothing() {
    let (upgrader, ports) = installer(
        NO_KERNEL_LISTING,
        300,
        &[&["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"]],
    );
    assert_eq!(run(&upgrader, LinuxScope::Security), Ok(()));
    assert_eq!(
        ports.install.calls(),
        vec![args(&[
            "--wait",
            "300",
            "add",
            "-q",
            "hostkeeper=0.1.80-r0"
        ])]
    );
}

/// Proves no pin is run when no own package has an update.
#[test]
fn no_own_package_with_an_update_runs_no_pin() {
    let (upgrader, ports) = installer(NO_OWN_LISTING, 300, &[&["--wait", "300", "upgrade", "-q"]]);
    assert_eq!(run(&upgrader, LinuxScope::All), Ok(()));
    assert_eq!(
        ports.install.calls(),
        vec![args(&["--wait", "300", "upgrade", "-q"])]
    );
}

/// Proves a failed upgrade is an install failure naming the step, its exit and the first line of
/// its standard error.
#[test]
fn failed_upgrade_is_an_install_failure() {
    let (upgrader, ports) = installer(NO_OWN_LISTING, 300, &[]);
    ports.install.always(
        &args(&["--wait", "300", "upgrade", "-q"]),
        Ok(LaunchctlOutput::exit(
            1,
            "ERROR: unable to lock database: Resource temporarily unavailable",
        )),
    );
    assert_eq!(
        run(&upgrader, LinuxScope::All),
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apk upgrade failed with exit code 1: ERROR: unable to lock database: \
                     Resource temporarily unavailable"
                .to_owned(),
        })
    );
}

/// Proves a failed refresh stops the install: nothing is listed, pinned or upgraded after it.
#[test]
fn failed_refresh_runs_nothing_after_it() {
    let (upgrader, ports) = installer(LISTING, 300, &[]);
    ports.refresh.always(
        &args(&["--wait", "300", "update", "-q"]),
        Ok(LaunchctlOutput::exit(99, "")),
    );
    assert_eq!(
        run(&upgrader, LinuxScope::All),
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apk update failed with exit code 99".to_owned(),
        })
    );
    assert_eq!(ports.list.calls(), Vec::<Vec<String>>::new());
    assert_eq!(ports.install.calls(), Vec::<Vec<String>>::new());
}

/// Proves the lock wait reaches every run that takes the lock as `--wait <secs>`, and the listing
/// takes none.
#[test]
fn lock_wait_reaches_each_locking_run() {
    let (upgrader, ports) = installer(
        LISTING,
        42,
        &[
            &["--wait", "42", "add", "-q", "hostkeeper=0.1.80-r0"],
            &["--wait", "42", "upgrade", "-q"],
        ],
    );
    assert_eq!(run(&upgrader, LinuxScope::All), Ok(()));
    assert_eq!(
        ports.refresh.calls(),
        vec![args(&["--wait", "42", "update", "-q"])]
    );
    assert_eq!(ports.list.calls(), vec![args(&["version", "-l", "<"])]);
    assert_eq!(
        ports.install.calls(),
        vec![
            args(&["--wait", "42", "add", "-q", "hostkeeper=0.1.80-r0"]),
            args(&["--wait", "42", "upgrade", "-q"]),
        ]
    );
}

/// Proves the installer's name is `apk`.
#[test]
fn name_is_apk() {
    let (upgrader, _) = installer("", 1, &[]);
    assert_eq!(upgrader.name(), "apk");
}

/// Proves the installer of the host is built for any lock wait, the largest included.
#[test]
fn at_host_builds_with_the_largest_lock_wait() {
    assert_eq!(ApkUpgrade::at_host(u64::MAX).name(), "apk");
}

/// Proves a listing that fails stops the install: it names `apk version` and its exit, and
/// nothing is pinned or upgraded.
#[test]
fn failed_listing_pins_and_upgrades_nothing() {
    let (upgrader, ports) = installer(LISTING, 300, &[]);
    ports.list.always(
        &args(&["version", "-l", "<"]),
        Ok(LaunchctlOutput::exit(1, "ERROR: No such file or directory")),
    );
    assert_eq!(
        run(&upgrader, LinuxScope::All),
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apk version failed with exit code 1: ERROR: No such file or directory"
                .to_owned(),
        })
    );
    assert_eq!(ports.install.calls(), Vec::<Vec<String>>::new());
}

/// Proves a pin that fails stops the install before the upgrade: it names `apk add (pin)`, and
/// no upgrade runs, so hostkeeper's package is never upgraded unpinned.
#[test]
fn failed_pin_runs_no_upgrade() {
    let (upgrader, ports) = installer(LISTING, 300, &[]);
    let pin = args(&["--wait", "300", "add", "-q", "hostkeeper=0.1.80-r0"]);
    ports
        .install
        .always(&pin, Ok(LaunchctlOutput::exit(1, "ERROR: unable to lock")));
    assert_eq!(
        run(&upgrader, LinuxScope::All),
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apk add (pin) failed with exit code 1: ERROR: unable to lock".to_owned(),
        })
    );
    assert_eq!(ports.install.calls(), vec![pin]);
}

/// Proves a kernel package that hostkeeper itself installs is pinned and not upgraded under the
/// security scope, which then upgrades nothing.
#[test]
fn an_own_kernel_is_pinned_and_not_upgraded() {
    let pin = ["--wait", "300", "add", "-q", "linux-lts=6.12.1-r0"];
    let (upgrader, ports) = installer(NO_OWN_LISTING, 300, &[&pin]);
    let own: BTreeSet<String> = ["linux-lts".to_owned()].into();
    let result = upgrader.upgrade(&Upgrade {
        scope: LinuxScope::Security,
        own_packages: &own,
    });
    assert_eq!(result, Ok(()));
    assert_eq!(ports.install.calls(), vec![args(&pin)]);
}
