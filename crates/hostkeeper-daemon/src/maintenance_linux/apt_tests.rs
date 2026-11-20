//! Tests of the `apt` upgrader, over fakes of `apt-get`, `apt-mark` and `env` and the fixtures of
//! the apt collector.

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};

use super::*;
use crate::maintenance_linux::REASON_INSTALL_FAILED;

const UBUNTU: &str = include_str!("../../tests/fixtures/maintenance/apt/ubuntu_2404.txt");
const HELD: &str = include_str!("../../tests/fixtures/maintenance/apt/debian12_held.txt");
const NOTHING: &str = include_str!("../../tests/fixtures/maintenance/apt/nothing_to_do.txt");

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The four ports, each a fake of its own.
struct Host {
    refresh: FakeLaunchctl,
    simulate: FakeLaunchctl,
    mark: FakeLaunchctl,
    install: FakeLaunchctl,
    apt: AptUpgrade,
}

/// A host whose refresh and mark succeed and whose simulation writes `simulation`; the install
/// is scripted by each test.
fn host(simulation: &str, lock: u64) -> Host {
    let timeout = format!("DPkg::Lock::Timeout={lock}");
    let refresh = FakeLaunchctl::new();
    refresh.always(
        &args(&["-o", &timeout, "update", "-qq"]),
        Ok(LaunchctlOutput::success("")),
    );
    let simulate = FakeLaunchctl::new();
    simulate.always(
        &args(&["-o", &timeout, "--simulate", "-qq", "dist-upgrade"]),
        Ok(LaunchctlOutput::success(simulation)),
    );
    let mark = FakeLaunchctl::new();
    mark.always(
        &args(&["-o", &timeout, "hold", "hostkeeper"]),
        Ok(LaunchctlOutput::success("")),
    );
    let install = FakeLaunchctl::new();
    let apt = AptUpgrade::new(
        Box::new(refresh.clone()),
        Box::new(simulate.clone()),
        Box::new(mark.clone()),
        Box::new(install.clone()),
        lock,
    );
    Host {
        refresh,
        simulate,
        mark,
        install,
        apt,
    }
}

fn own() -> BTreeSet<String> {
    BTreeSet::from(["hostkeeper".to_owned()])
}

fn run(host: &Host, scope: LinuxScope) -> Result<(), KindError> {
    let own = own();
    host.apt.upgrade(&Upgrade {
        scope,
        own_packages: &own,
    })
}

/// The install's arguments up to its verb: the environment, `apt-get` and its options.
fn install_prefix(lock: u64) -> Vec<String> {
    let timeout = format!("DPkg::Lock::Timeout={lock}");
    let mut list: Vec<&str> = SCRIPTS_ENV.to_vec();
    list.extend([
        "/usr/bin/apt-get",
        "-o",
        &timeout,
        "-o",
        "Dpkg::Options::=--force-confdef",
        "-o",
        "Dpkg::Options::=--force-confold",
        "-qq",
        "-y",
    ]);
    args(&list)
}

fn ok_install(host: &Host, rest: &[&str], lock: u64) {
    let mut full = install_prefix(lock);
    full.extend(args(rest));
    host.install.always(&full, Ok(LaunchctlOutput::success("")));
}

/// Proves the security scope installs only the installed security and kernel packages of the
/// simulation, in its order, through `env` with the scripts' environment first, after holding the
/// own package the simulation would move; the own package and the non-security update are left
/// out.
#[test]
fn security_scope_installs_the_security_list_after_holding_the_own_package() {
    let host = host(UBUNTU, 300);
    ok_install(
        &host,
        &[
            "install",
            "--only-upgrade",
            "libc6",
            "libc-bin",
            "openssl",
            "linux-generic",
        ],
        300,
    );
    assert_eq!(run(&host, LinuxScope::Security), Ok(()));
    assert_eq!(
        host.refresh.calls(),
        vec![args(&["-o", "DPkg::Lock::Timeout=300", "update", "-qq"])]
    );
    assert_eq!(
        host.simulate.calls(),
        vec![args(&[
            "-o",
            "DPkg::Lock::Timeout=300",
            "--simulate",
            "-qq",
            "dist-upgrade"
        ])]
    );
    assert_eq!(
        host.mark.calls(),
        vec![args(&[
            "-o",
            "DPkg::Lock::Timeout=300",
            "hold",
            "hostkeeper"
        ])]
    );
    let mut expected = install_prefix(300);
    expected.extend(args(&[
        "install",
        "--only-upgrade",
        "libc6",
        "libc-bin",
        "openssl",
        "linux-generic",
    ]));
    assert_eq!(host.install.calls(), vec![expected]);
}

/// Proves the all scope runs `dist-upgrade` with exactly its arguments, after holding the own
/// package.
#[test]
fn all_scope_runs_dist_upgrade_after_the_hold() {
    let host = host(UBUNTU, 300);
    ok_install(&host, &["dist-upgrade"], 300);
    assert_eq!(run(&host, LinuxScope::All), Ok(()));
    assert_eq!(host.mark.calls().len(), 1);
    let mut expected = install_prefix(300);
    expected.push("dist-upgrade".to_owned());
    assert_eq!(host.install.calls(), vec![expected]);
}

/// Proves no `apt-mark` runs when the simulation moves no own package.
#[test]
fn no_hold_when_no_own_package_moves() {
    let host = host(HELD, 300);
    ok_install(&host, &["dist-upgrade"], 300);
    assert_eq!(run(&host, LinuxScope::All), Ok(()));
    assert!(host.mark.calls().is_empty());
}

/// Proves the security scope with nothing due runs no install and succeeds.
#[test]
fn security_scope_with_nothing_due_installs_nothing() {
    let host = host(NOTHING, 300);
    assert_eq!(run(&host, LinuxScope::Security), Ok(()));
    assert!(host.mark.calls().is_empty());
    assert!(host.install.calls().is_empty());
}

/// Proves a package of a foreign architecture is named as apt printed it, an own package is
/// matched without its architecture, and a package named twice is installed once.
#[test]
fn architecture_suffixes_and_repeats_are_handled() {
    let simulation = "\
Inst libc6:i386 [2.39-1] (2.39-2 Ubuntu:24.04/noble-security [i386])
Inst libc6:i386 [2.39-1] (2.39-2 Ubuntu:24.04/noble-security [i386])
Inst hostkeeper:amd64 [1.4.0-1] (1.5.0-1 hostkeeper:stable [amd64])
";
    let host = host(simulation, 300);
    ok_install(&host, &["install", "--only-upgrade", "libc6:i386"], 300);
    assert_eq!(run(&host, LinuxScope::Security), Ok(()));
    assert_eq!(
        host.mark.calls(),
        vec![args(&[
            "-o",
            "DPkg::Lock::Timeout=300",
            "hold",
            "hostkeeper"
        ])]
    );
    assert_eq!(host.install.calls().len(), 1);
}

/// Proves a failed install is reported as `install_failed`, naming `apt-get install`, its exit and
/// the first line of its standard error.
#[test]
fn a_failed_install_names_the_step_and_the_exit() {
    let host = host(UBUNTU, 300);
    let mut full = install_prefix(300);
    full.extend(args(&[
        "install",
        "--only-upgrade",
        "libc6",
        "libc-bin",
        "openssl",
        "linux-generic",
    ]));
    host.install.always(
        &full,
        Ok(LaunchctlOutput::exit(
            100,
            "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4242 (apt)\nE: second line\n",
        )),
    );
    assert_eq!(
        run(&host, LinuxScope::Security),
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apt-get install failed with exit code 100: E: Could not get lock \
                     /var/lib/dpkg/lock-frontend. It is held by process 4242 (apt)"
                .to_owned(),
        })
    );
}

/// Proves a failed index refresh fails the install naming `apt-get update`, and nothing after it
/// runs.
#[test]
fn a_failed_refresh_stops_before_everything_else() {
    let host = host(UBUNTU, 300);
    let refresh = FakeLaunchctl::new();
    refresh.always(
        &args(&["-o", "DPkg::Lock::Timeout=300", "update", "-qq"]),
        Ok(LaunchctlOutput::exit(100, "E: Failed to fetch")),
    );
    let apt = AptUpgrade::new(
        Box::new(refresh),
        Box::new(host.simulate.clone()),
        Box::new(host.mark.clone()),
        Box::new(host.install.clone()),
        300,
    );
    let own = own();
    let result = apt.upgrade(&Upgrade {
        scope: LinuxScope::All,
        own_packages: &own,
    });
    assert_eq!(
        result,
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apt-get update failed with exit code 100: E: Failed to fetch".to_owned(),
        })
    );
    assert!(host.simulate.calls().is_empty());
    assert!(host.mark.calls().is_empty());
    assert!(host.install.calls().is_empty());
}

/// Proves a failed hold fails the install naming `apt-mark hold`, and no install runs.
#[test]
fn a_failed_hold_runs_no_install() {
    let host = host(UBUNTU, 300);
    let mark = FakeLaunchctl::new();
    mark.always(
        &args(&["-o", "DPkg::Lock::Timeout=300", "hold", "hostkeeper"]),
        Ok(LaunchctlOutput::exit(100, "E: Unable to lock")),
    );
    let apt = AptUpgrade::new(
        Box::new(host.refresh.clone()),
        Box::new(host.simulate.clone()),
        Box::new(mark),
        Box::new(host.install.clone()),
        300,
    );
    let own = own();
    let result = apt.upgrade(&Upgrade {
        scope: LinuxScope::All,
        own_packages: &own,
    });
    assert_eq!(
        result,
        Err(KindError::Failed {
            reason: REASON_INSTALL_FAILED,
            detail: "apt-mark hold failed with exit code 100: E: Unable to lock".to_owned(),
        })
    );
    assert!(host.install.calls().is_empty());
}

/// Proves the lock wait reaches the arguments of every program.
#[test]
fn the_lock_wait_reaches_every_program() {
    let host = host(UBUNTU, 42);
    ok_install(&host, &["dist-upgrade"], 42);
    assert_eq!(run(&host, LinuxScope::All), Ok(()));
    for calls in [
        host.refresh.calls(),
        host.simulate.calls(),
        host.mark.calls(),
    ] {
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][..2], args(&["-o", "DPkg::Lock::Timeout=42"])[..]);
    }
    assert_eq!(host.install.calls(), {
        let mut expected = install_prefix(42);
        expected.push("dist-upgrade".to_owned());
        vec![expected]
    });
}

/// Proves the host's upgrader is named `apt` and runs `apt-mark` and `env` by their paths
/// (nothing is run).
#[test]
fn the_host_upgrader_is_apt() {
    assert_eq!(APT_MARK, "/usr/bin/apt-mark");
    assert_eq!(ENV, "/usr/bin/env");
    assert_eq!(AptUpgrade::at_host(300).name(), "apt");
    assert_eq!(AptUpgrade::at_host(u64::MAX).name(), "apt");
}

/// Proves the install's environment, the words `env` is given before `apt-get`, is a fixed `PATH`,
/// no question asked and needrestart suspended (listing only), and nothing else.
///
/// Catches: an install with no `PATH`, which `dpkg` refuses, or one whose needrestart restarts the
/// services of the host mid-run.
#[test]
fn the_install_environment_is_a_path_no_questions_and_needrestart_suspended() {
    assert_eq!(
        SCRIPTS_ENV,
        [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "DEBIAN_FRONTEND=noninteractive",
            "NEEDRESTART_SUSPEND=1",
            "NEEDRESTART_MODE=l",
        ]
    );
}
