//! An `apt` service whose sealed version directory was damaged on the host: a tree that is not
//! its seal, a `target.json` that does not read, one that is gone.

mod support;
mod support_apt;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_daemon::target::TARGET_FILE;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::Class;
use support::{force, uninstall};
use support_apt::seen::{failed, held, install_of, labels_since, linked, succeeded, update_of};
use support_apt::{AptRig, PROGRAM, SERVICE, entry, open_up, package, with_entry};

/// Overwrites `file` of the sealed version `version` with `text`.
fn damage(apt: &AptRig, version: &str, file: &str, text: &str) {
    open_up(&apt.rig.packages.join(SERVICE));
    let path = apt.version_dir(version).join(file);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("its mode");
    fs::write(&path, text).expect("the file is damaged");
}

/// Proves a version on the host whose tree is no longer the one its seal names is not used: the
/// update that orders it again is refused with the unpack's refusal, nothing is installed or
/// run, and a `force` of it ends `Failed` before the commit, `environment`, with the refusal's
/// token.
///
/// Catches: a damaged version directory activated as the sealed one, or a force that reports a
/// refusal of its stage as a success or a refusal.
#[test]
fn a_sealed_version_whose_tree_changed_is_not_activated_by_an_update_or_a_force() {
    let apt = AptRig::new("apt-tree-changed");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host
        .edit(|state| state.faults.script_fails_for = Some("2.0.0-1".to_owned()));
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        failed(Class::Release, "dpkg_error")
    );
    apt.host.edit(|state| state.faults.script_fails_for = None);
    damage(
        &apt,
        "2.0.0",
        &format!("tree/{PROGRAM}"),
        "#!/bin/sh\necho other\n",
    );
    let calls = apt.host.calls().len();
    let reply = daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 3));
    let refusal = reply.refusal().expect("a refusal");
    let forced = daemon.order(&with_entry(
        force(SERVICE, "2.0.0", &two, 4, 0),
        &entry("2.0.0-1", &two),
    ));
    assert_eq!(forced, failed(Class::Environment, refusal.token()));
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
}

/// Proves the installer a service runs is read from the `target.json` of `current`, and a
/// `target.json` that does not read stops the order before anything is fetched or run: an update
/// is refused `release_unreadable`.
///
/// Catches: an update of a service whose installer is unknown, treated as an archive service.
#[test]
fn an_update_of_a_service_whose_target_json_does_not_read_is_release_unreadable() {
    let apt = AptRig::new("apt-target-unreadable");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    damage(&apt, "1.0.0", TARGET_FILE, "{");
    let calls = apt.host.calls().len();
    let reply = daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::ReleaseUnreadable),
        "{reply:?}"
    );
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
}

/// Proves an uninstall resumed from its tombstone reads the entry from the `target.json` of
/// `current` and does not go on when it cannot: a `target.json` that does not read interrupts
/// the uninstall with `link_failed`, the package stays installed and the tombstone stays.
///
/// Catches: an uninstall resumed on a damaged version that purges with the wrong bound or
/// installer.
#[test]
fn a_resumed_uninstall_whose_target_json_does_not_read_is_interrupted() {
    let apt = AptRig::new("apt-resume-unreadable");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host.edit(|state| state.faults.purge_refused = true);
    let first = daemon.order(&uninstall(SERVICE, 2));
    assert_eq!(first, failed(Class::Environment, "dependency_changes"));
    apt.host.edit(|state| state.faults.purge_refused = false);
    damage(&apt, "1.0.0", TARGET_FILE, "{");
    let reply = daemon.order(&uninstall(SERVICE, 3));
    let Reply::Interrupted { refusal, .. } = reply else {
        panic!("not interrupted: {reply:?}");
    };
    assert_eq!(refusal, Refusal::LinkFailed);
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
}

/// Proves an uninstall resumed with the `target.json` of `current` gone still purges the
/// package, by the installer the record names and the bound the installer falls back to
/// (120 seconds: 60 tries when the lock is held), and removes the service.
///
/// Catches: a resumed uninstall that cannot finish because the entry it was staged for is gone.
#[test]
fn a_resumed_uninstall_without_target_json_purges_by_the_recorded_installer() {
    let apt = AptRig::new("apt-resume-no-target");
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
    apt.host.edit(|state| {
        state.faults.purge_refused = false;
        state.faults.busy.insert("purge", 1000);
    });
    open_up(&apt.rig.packages.join(SERVICE));
    fs::remove_file(apt.version_dir("1.0.0").join(TARGET_FILE)).expect("target.json goes");
    let before = apt.host.calls_of("dpkg --purge").len();
    let reply = daemon.order(&uninstall(SERVICE, 3));
    assert!(
        matches!(&reply, Reply::Finished { reason: Some(r), .. } if r == "package_manager_busy"),
        "{reply:?}"
    );
    assert_eq!(apt.host.calls_of("dpkg --purge").len() - before, 60);
    apt.host.edit(|state| state.faults.busy.clear());
    assert_eq!(daemon.order(&uninstall(SERVICE, 4)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert!(!apt.rig.packages.join(SERVICE).exists());
}
