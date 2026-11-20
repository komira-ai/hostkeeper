//! The tests of `maintenance_linux::dnf`: the arguments of each `dnf` call by scope, the kernel
//! step of the security scope over a fixture listing, and the failures, asserted by equality.

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};

use super::*;
use crate::maintenance_linux::REASON_INSTALL_FAILED;

const CHECK_FIXTURE: &str = include_str!("../../tests/fixtures/maintenance/dnf/check_update.txt");
const NO_KERNEL: &str = "openssl.x86_64     1:3.2.2-6.el9_5     baseos\n";

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn check_args() -> Vec<String> {
    args(&["check-update", "-q"])
}

fn security_args() -> Vec<String> {
    args(&["-y", "-q", "upgrade", "--security"])
}

fn listed(code: i32, text: &str) -> LaunchctlOutput {
    LaunchctlOutput {
        code: Some(code),
        stdout: text.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

/// An installer over two fakes: the listing's and the upgrade's.
fn installer() -> (DnfUpgrade, FakeLaunchctl, FakeLaunchctl) {
    let list = FakeLaunchctl::new();
    let install = FakeLaunchctl::new();
    let upgrader = DnfUpgrade::new(Box::new(list.clone()), Box::new(install.clone()));
    (upgrader, list, install)
}

fn run(upgrader: &DnfUpgrade, scope: LinuxScope, own: &[&str]) -> Result<(), KindError> {
    let own = names(own);
    upgrader.upgrade(&Upgrade {
        scope,
        own_packages: &own,
    })
}

fn failed(detail: &str) -> KindError {
    KindError::Failed {
        reason: REASON_INSTALL_FAILED,
        detail: detail.to_owned(),
    }
}

/// Proves: the installer is named `dnf`.
#[test]
fn name_is_dnf() {
    let (upgrader, _, _) = installer();
    assert_eq!(upgrader.name(), "dnf");
}

/// Proves: scope `all` runs one upgrade that excludes each own package in sorted order and never
/// lists the updates.
#[test]
fn scope_all_runs_one_upgrade_with_excludes() {
    let (upgrader, list, install) = installer();
    let all = args(&[
        "-y",
        "-q",
        "upgrade",
        "--exclude=hostkeeper",
        "--exclude=tierstore",
    ]);
    install.always(&all, Ok(LaunchctlOutput::success("")));
    assert_eq!(
        run(&upgrader, LinuxScope::All, &["tierstore", "hostkeeper"]),
        Ok(())
    );
    assert_eq!(install.calls(), vec![all]);
    assert!(list.calls().is_empty());
}

/// Proves: scope `security` upgrades the security updates, lists the updates left and upgrades
/// the kernel packages it lists by name, after the excludes.
#[test]
fn scope_security_upgrades_listed_kernels() {
    let (upgrader, list, install) = installer();
    let security = args(&["-y", "-q", "upgrade", "--security", "--exclude=hostkeeper"]);
    let kernel = args(&[
        "-y",
        "-q",
        "upgrade",
        "--exclude=hostkeeper",
        "kernel",
        "kernel-core",
    ]);
    install.always(&security, Ok(LaunchctlOutput::success("")));
    install.always(&kernel, Ok(LaunchctlOutput::success("")));
    list.always(&check_args(), Ok(listed(100, CHECK_FIXTURE)));
    assert_eq!(
        run(&upgrader, LinuxScope::Security, &["hostkeeper"]),
        Ok(())
    );
    assert_eq!(install.calls(), vec![security, kernel]);
    assert_eq!(list.calls(), vec![check_args()]);
}

/// Proves: scope `security` with no kernel among the listed updates, or nothing listed, runs the
/// security upgrade alone.
#[test]
fn scope_security_without_kernel_upgrades_security_only() {
    for (code, text) in [(100, NO_KERNEL), (0, "")] {
        let (upgrader, list, install) = installer();
        install.always(&security_args(), Ok(LaunchctlOutput::success("")));
        list.always(&check_args(), Ok(listed(code, text)));
        assert_eq!(run(&upgrader, LinuxScope::Security, &[]), Ok(()));
        assert_eq!(install.calls(), vec![security_args()]);
    }
}

/// Proves: a kernel package that hostkeeper installs is excluded and left out of the kernel
/// upgrade.
#[test]
fn own_kernel_package_is_excluded_not_upgraded() {
    let (upgrader, list, install) = installer();
    let security = args(&["-y", "-q", "upgrade", "--security", "--exclude=kernel-core"]);
    let kernel = args(&["-y", "-q", "upgrade", "--exclude=kernel-core", "kernel"]);
    install.always(&security, Ok(LaunchctlOutput::success("")));
    install.always(&kernel, Ok(LaunchctlOutput::success("")));
    list.always(&check_args(), Ok(listed(100, CHECK_FIXTURE)));
    assert_eq!(
        run(&upgrader, LinuxScope::Security, &["kernel-core"]),
        Ok(())
    );
    assert_eq!(install.calls(), vec![security, kernel]);
}

/// Proves: a kernel listed twice is upgraded once.
#[test]
fn repeated_kernel_is_named_once() {
    let (upgrader, list, install) = installer();
    let kernel = args(&["-y", "-q", "upgrade", "kernel"]);
    install.always(&security_args(), Ok(LaunchctlOutput::success("")));
    install.always(&kernel, Ok(LaunchctlOutput::success("")));
    let text = "kernel.x86_64   5.14.0-503.14.1.el9_5   baseos\n\
                kernel.noarch   5.14.0-503.14.1.el9_5   baseos\n";
    list.always(&check_args(), Ok(listed(100, text)));
    assert_eq!(run(&upgrader, LinuxScope::Security, &[]), Ok(()));
    assert_eq!(install.calls(), vec![security_args(), kernel]);
}

/// Proves: a failed security upgrade fails with the step, its exit and the first line of its
/// standard error, and neither the listing nor the kernel upgrade runs.
/// Catches: going on to the kernel after the security upgrade failed.
#[test]
fn failed_security_upgrade_stops_the_install() {
    let (upgrader, list, install) = installer();
    install.always(
        &security_args(),
        Ok(LaunchctlOutput::exit(
            1,
            "Error: Failed to download metadata for repo\nmore\n",
        )),
    );
    assert_eq!(
        run(&upgrader, LinuxScope::Security, &[]),
        Err(failed(
            "dnf upgrade --security failed with exit code 1: Error: Failed to download metadata for repo"
        ))
    );
    assert_eq!(install.calls().len(), 1);
    assert!(list.calls().is_empty());
}

/// Proves: a `dnf check-update` that exits 1 fails the install naming the listing, and no kernel
/// upgrade runs.
#[test]
fn failed_listing_stops_before_the_kernel_upgrade() {
    let (upgrader, list, install) = installer();
    install.always(&security_args(), Ok(LaunchctlOutput::success("")));
    list.always(&check_args(), Ok(LaunchctlOutput::exit(1, "")));
    assert_eq!(
        run(&upgrader, LinuxScope::Security, &[]),
        Err(failed("dnf check-update failed with exit code 1"))
    );
    assert_eq!(install.calls(), vec![security_args()]);
}

/// Proves: a failed kernel upgrade fails the install naming the kernel step.
#[test]
fn failed_kernel_upgrade_is_reported() {
    let (upgrader, list, install) = installer();
    install.always(&security_args(), Ok(LaunchctlOutput::success("")));
    install.always(
        &args(&["-y", "-q", "upgrade", "kernel", "kernel-core"]),
        Ok(LaunchctlOutput::exit(1, "boom")),
    );
    list.always(&check_args(), Ok(listed(100, CHECK_FIXTURE)));
    assert_eq!(
        run(&upgrader, LinuxScope::Security, &[]),
        Err(failed("dnf upgrade kernel failed with exit code 1: boom"))
    );
}

/// Proves: the installer of the host builds without running anything.
#[test]
fn at_host_builds_both_ports() {
    assert_eq!(DnfUpgrade::at_host(u64::MAX).name(), "dnf");
}
