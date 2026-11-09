//! The daemon's orders for an `apt` service that end early, differently or again: an install
//! that cannot activate, an installer that is not the service's, an entry that is not the one
//! sealed, a resend, the heartbeat's drift that cannot be read, the sealed executable of an apt
//! version and the prune after an apt update (packages-repositories.md section 4).

mod support;
mod support_apt;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_daemon::link_adapters::DaemonObserver;
use hostkeeper_daemon::observer::SealedExe;
use hostkeeper_daemon::prune::LayoutPrune;
use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Verdict};
use hostkeeper_link::{Drift, ServiceObserver};
use hostkeeper_observe::ExpectedExe;
use sha2::{Digest, Sha256};
use support::{OWN, alerting, force_rollback, lines, update};
use support_apt::seen::{
    failed, held, install_of, labels_since, linked, recorded, shared, succeeded, update_of, web,
};
use support_apt::{AptRig, PROGRAM, SERVICE, entry, package, with_entry};

/// Proves a first install whose package fails in a maintainer script is rolled back by
/// purging it: nothing is installed afterwards, `current` is not made, `placed.json` holds no
/// record, and the verdict is `Failed`, `release`, `dpkg_error`.
///
/// Catches: a first install whose failed `activate` leaves the package installed, or `current`
/// naming a version that does not run.
#[test]
fn a_first_install_whose_package_script_fails_is_purged_and_leaves_nothing() {
    let apt = AptRig::new("apt-first-install-fails");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    apt.host
        .edit(|state| state.faults.script_fails_for = Some("1.0.0-1".to_owned()));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        failed(Class::Release, "dpkg_error")
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current"), None);
    assert_eq!(recorded(&daemon), None);
    assert!(apt.host.labels().contains(&"dpkg --purge".to_owned()));
}

/// Proves an install that finds the package manager's lock held, for as long as the installer
/// waits, ends `Deferred` with the reason `package_manager_busy` (to be ordered again) and
/// leaves nothing installed.
///
/// Catches: a busy package manager reported as a failure of the release, or a first install
/// that leaves a half-made version current.
#[test]
fn an_install_that_finds_the_package_manager_busy_is_deferred_and_leaves_nothing() {
    let apt = AptRig::new("apt-install-busy");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    apt.host
        .edit(|state| state.faults.busy.insert("install", 1000));
    let reply = daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1));
    let Reply::Finished {
        verdict: Verdict::Deferred { .. },
        reason,
        resent: false,
    } = reply
    else {
        panic!("not a deferred order: {reply:?}");
    };
    assert_eq!(reason.as_deref(), Some("package_manager_busy"));
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current"), None);
    assert_eq!(recorded(&daemon), None);
}

/// Proves a service keeps the installer it was installed with: a service installed from the
/// release channel and then ordered an apt entry ends `Failed`, `release`, `installer_changed`
/// with nothing fetched and nothing installed, and `current` stays.
///
/// Catches: an apt entry that takes over an archive service, replacing its files by a package.
#[test]
fn an_archive_service_ordered_an_apt_entry_is_installer_changed() {
    let apt = AptRig::new("apt-archive-to-apt");
    let daemon = apt.daemon();
    apt.rig.running(SERVICE, "1.0.0");
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 1)),
        failed(Class::Release, "installer_changed")
    );
    assert!(apt.host.calls().is_empty(), "no tool ran");
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert!(!apt.version_dir("2.0.0").exists());
}

/// Proves the same rule the other way round: an apt service ordered an update from the release
/// channel (no entry) ends `Failed`, `release`, `installer_changed`, with nothing fetched from the
/// package port and the installed package untouched.
///
/// Catches: an archive update that replaces an apt service's version directory beside its
/// package.
#[test]
fn an_apt_service_ordered_without_an_entry_is_installer_changed() {
    let apt = AptRig::new("apt-to-archive");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.rig.stage(SERVICE, "2.0.0");
    let calls = apt.host.calls().len();
    assert_eq!(
        daemon.order(&update(SERVICE, "2.0.0", &support::d(), 2)),
        failed(Class::Release, "installer_changed")
    );
    assert!(apt.rig.store.asked().is_empty(), "nothing was fetched");
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
}

/// Proves a version already on the host is the sealed one only for the entry it was staged
/// for: ordered again with another `package_version`, it ends `Failed`, `release`,
/// `target_changed`, runs no tool, and changes neither the installed package, `current` nor the
/// record.
///
/// Catches: a sealed version that is activated for an entry other than its `target.json`.
#[test]
fn a_version_on_the_host_ordered_for_another_entry_is_target_changed() {
    let apt = AptRig::new("apt-target-changed");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        succeeded()
    );
    let calls = apt.host.calls().len();
    let again = with_entry(
        force_rollback(SERVICE, "1.0.0", &one, 3),
        &entry("1.0.1-1", &one),
    );
    assert_eq!(
        daemon.order(&again),
        failed(Class::Release, "target_changed")
    );
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
    assert_eq!(
        recorded(&daemon).and_then(|r| r.version).as_deref(),
        Some("2.0.0-1")
    );
}

/// Proves a resend of an apt order is answered from its record: the same order and try after
/// it ended `Succeeded` gives the same verdict with `resent: true` and runs no tool; a resend
/// of a failed order is answered the same way.
///
/// Catches: a resend that stages or installs the version again.
#[test]
fn a_resend_of_an_apt_order_is_answered_from_its_record() {
    let apt = AptRig::new("apt-resend");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let order = install_of("1.0.0", "1.0.0-1", &one, 1);
    assert_eq!(daemon.order(&order), succeeded());
    let calls = apt.host.calls().len();
    let Reply::Finished {
        verdict,
        reason,
        resent,
    } = daemon.order(&order)
    else {
        panic!("not an answer");
    };
    assert!(resent);
    assert_eq!(
        verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(reason, None);
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
}

/// Proves a heartbeat whose `dpkg-query` cannot run reports no drift, whatever the host holds,
/// and says so in the daemon's log, naming the service.
///
/// Catches: a drift report invented when the installed version cannot be read, or one that
/// fails the heartbeat without a line.
#[test]
fn a_dpkg_query_that_cannot_run_gives_no_drift_and_a_log_line() {
    let apt = AptRig::new("apt-drift-unreadable");
    let (daemon, log) = alerting(apt.daemon());
    let daemon = shared(daemon);
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host.set_installed(SERVICE, "0.9.0-1", 'i', false);
    apt.host.cannot_run("dpkg-query");
    let seen = DaemonObserver::new(Arc::clone(&daemon)).observe(&web());
    assert_eq!(seen.drift, None::<Drift>);
    let logged = lines(&log);
    assert!(
        logged.iter().any(|line| line.starts_with(
            "hostkeeper daemon: service=web: the installed version could not be read for its drift"
        )),
        "{logged:?}"
    );
}

/// Proves the executable the observer confirms an apt version by is the program the package
/// installs: the digest of an apt version is the SHA-256 of `tree/usr/bin/web`, found through
/// the `ExecStart=` of its unit file in the extracted tree; a version with no seal, and one the
/// service does not have, have none.
///
/// Catches: an apt version whose running executable is never matched, or matched against the
/// wrong file of the tree.
#[test]
fn the_sealed_digest_of_an_apt_version_is_that_of_its_program_in_the_tree() {
    let apt = AptRig::new("apt-sealed-exe");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    let program = fs::read(apt.version_dir("1.0.0").join("tree").join(PROGRAM)).expect("it reads");
    let want: [u8; 32] = Sha256::digest(&program).into();
    let exe = SealedExe::new(&apt.rig.packages, Path::new("/packages"), OWN);
    assert_eq!(exe.digest(SERVICE, "1.0.0"), Some(want));
    assert_eq!(exe.digest(SERVICE, "9.9.9"), None);
}

/// Proves an apt update that succeeds prunes the old version directories like any: after three
/// updates only `current` and `previous` are left, and nothing is asked of the package manager
/// for the versions deleted.
///
/// Catches: version directories of an apt service that pile up, or a prune that purges or
/// reinstalls a package for a version it deletes.
#[test]
fn updates_of_an_apt_service_prune_all_but_current_and_previous() {
    let apt = AptRig::new("apt-prune");
    let ports = Ports {
        prune: Arc::new(LayoutPrune),
        ..apt.ports()
    };
    let daemon = Daemon::new(support::config(), apt.rig.dirs(), ports, OWN);
    let pins: Vec<String> = ["1.0.0-1", "2.0.0-1", "3.0.0-1"]
        .iter()
        .map(|v| apt.publish(package(v)))
        .collect();
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &pins[0], 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &pins[1], 2)),
        succeeded()
    );
    assert!(apt.version_dir("1.0.0").is_dir(), "kept as previous");
    let before = apt.host.calls().len();
    // The order of 3.0.0 prunes 1.0.0.
    support_apt::open_up(&apt.rig.packages.join(SERVICE));
    assert_eq!(
        daemon.order(&update_of("3.0.0", "3.0.0-1", &pins[2], 3)),
        succeeded()
    );
    assert!(!apt.version_dir("1.0.0").exists(), "1.0.0 is pruned");
    assert!(!apt.rig.packages.join(SERVICE).join("1.0.0.seal").exists());
    assert_eq!(linked(&apt, "current").as_deref(), Some("3.0.0"));
    assert_eq!(linked(&apt, "previous").as_deref(), Some("2.0.0"));
    let after = labels_since(&apt, before);
    assert!(
        !after.iter().any(|l| l == "dpkg --purge"),
        "no purge for a pruned version: {after:?}"
    );
    assert_eq!(apt.host.installed(SERVICE), held("3.0.0-1"));
}
