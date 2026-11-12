//! The uninstall of an `apt` service (design section 4.7, packages-repositories.md section 4):
//! step 5 purges the package within the version's `remove_secs`, a hookless service needs no
//! `[hook.timeout_secs] uninstall`, and a purge that does not complete keeps the tombstone so a
//! later `uninstall` order finishes the work.

mod support;
mod support_apt;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Verdict};
use support::{install, uninstall};
use support_apt::seen::{failed, held, install_of, linked, recorded, succeeded};
use support_apt::{AptRig, PROGRAM, SERVICE, entry, open_up, package, with_entry};

/// Whether `placed.json` holds the tombstone of the service.
fn tombstoned(daemon: &hostkeeper_daemon::Daemon) -> bool {
    let record = daemon
        .ctx()
        .placer()
        .store()
        .load()
        .expect("placed.json reads");
    record.uninstalling.contains_key(SERVICE)
}

/// Proves a purge refused because another package depends on the service's package is the
/// uninstall's `Failed` verdict, `environment`, `dependency_changes`; the package stays
/// installed, the tombstone stays, `current` and the record stay, and a later `uninstall` order,
/// once the dependent is gone, completes the uninstall and leaves nothing.
///
/// Catches: an uninstall that drops the record or the version directories of a package that is
/// still installed, or one that cannot be completed after a refused purge.
#[test]
fn a_purge_refused_by_a_dependent_package_keeps_the_tombstone_until_a_later_uninstall() {
    let apt = AptRig::new("apt-uninstall-dependent");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host.edit(|state| state.faults.purge_refused = true);
    assert_eq!(
        daemon.order(&uninstall(SERVICE, 2)),
        failed(Class::Environment, "dependency_changes")
    );
    assert!(apt.host.installed(SERVICE).is_some(), "still installed");
    assert!(tombstoned(&daemon));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert!(recorded(&daemon).is_some());

    apt.host.edit(|state| state.faults.purge_refused = false);
    open_up(&apt.rig.packages.join(SERVICE));
    assert_eq!(daemon.order(&uninstall(SERVICE, 3)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert!(!tombstoned(&daemon));
    assert_eq!(recorded(&daemon), None);
    assert!(!apt.rig.packages.join(SERVICE).exists());
}

/// Proves a purge whose package manager lock stays held is tried for as long as
/// `--package-lock-wait-secs` says, not the version's `remove_secs`, which bounds each run (a
/// 20 second wait with a 10 second `remove_secs`: 11 tries, waiting on the daemon's clock between
/// them), and then ends the uninstall `Deferred`, `package_manager_busy`, with the tombstone and
/// the package kept; once the lock is free a later `uninstall` order completes it.
///
/// Catches: an uninstall that waits a bound other than the daemon's, gives up as a failure, or
/// forgets the half-done uninstall.
#[test]
fn a_purge_whose_lock_stays_held_is_deferred_after_the_daemons_lock_wait() {
    let apt = AptRig::new("apt-uninstall-busy");
    let daemon = apt.daemon_waiting(20);
    let one = apt.publish(package("1.0.0-1"));
    let mut bounded = entry("1.0.0-1", &one);
    bounded
        .manifest
        .as_mut()
        .and_then(|manifest| manifest.installer_bounds.as_mut())
        .expect("the entry has installer bounds")
        .remove_secs = 10;
    assert_eq!(
        daemon.order(&with_entry(install(SERVICE, "1.0.0", &one, 1), &bounded)),
        succeeded()
    );
    apt.host
        .edit(|state| state.faults.busy.insert("purge", 1000));
    let reply = daemon.order(&uninstall(SERVICE, 2));
    let Reply::Finished {
        verdict: Verdict::Deferred { .. },
        reason,
        resent: false,
    } = reply
    else {
        panic!("not deferred: {reply:?}");
    };
    assert_eq!(reason.as_deref(), Some("package_manager_busy"));
    assert_eq!(apt.host.calls_of("dpkg --purge").len(), 11);
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert!(tombstoned(&daemon));

    apt.host.edit(|state| state.faults.busy.clear());
    open_up(&apt.rig.packages.join(SERVICE));
    assert_eq!(daemon.order(&uninstall(SERVICE, 3)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert!(!tombstoned(&daemon));
}

/// Proves an uninstall of a hookless apt service checks the tree of `current` against its
/// seal before it changes anything: a program of the extracted tree changed after the seal is
/// refused `release_unreadable`, and no tombstone is written, no package purged and nothing
/// removed.
///
/// Catches: an uninstall that runs from a version directory that is not the one verified.
#[test]
fn an_uninstall_of_a_tree_that_is_not_its_seal_is_refused_before_anything_changes() {
    let apt = AptRig::new("apt-uninstall-tree");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    open_up(&apt.rig.packages.join(SERVICE));
    let program = apt.version_dir("1.0.0").join("tree").join(PROGRAM);
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("its mode");
    fs::write(&program, "#!/bin/sh\necho changed\n").expect("the program changes");
    let reply = daemon.order(&uninstall(SERVICE, 2));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::ReleaseUnreadable),
        "{reply:?}"
    );
    assert!(!tombstoned(&daemon));
    assert!(apt.host.calls_of("dpkg --purge").is_empty());
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
}
