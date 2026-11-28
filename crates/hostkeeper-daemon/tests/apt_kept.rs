//! A config package's first install over a file an operator placed by hand at one of its
//! payload paths (packages-lifecycle.md section 6.5, "A file nobody owns is kept at a first
//! install"): the daemon, with the fake of apt and dpkg writing the package's files on the host,
//! keeps the file at the first install and puts it back when that install is removed, by a
//! failed first install's rollback or by an uninstall; a file another package owns is not kept;
//! a copy that cannot be put back is an alert naming the path, and the removal completes.

mod support;
mod support_apt;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use support::{OWN, Tap, alerting, config, install, lines, uninstall, update};
use support_apt::apt_fake::Deb;
use support_apt::seen::{failed, held, linked, recorded, succeeded};
use support_apt::{AptRig, HOOK_PROGRAM, SERVICE, hooked_entry, with_entry};

/// The config package's file, which an operator placed by hand when the node was set up.
const FILE: &str = "/etc/web/flags";
/// What the operator wrote there.
const HAND: &str = "--node-pool=gpu\n";

/// The config package at `version`: its file under `/etc/web/` and its hook program.
fn config_package(version: &str) -> Deb {
    Deb::new(SERVICE, version, "amd64")
        .file(
            FILE.trim_start_matches('/'),
            0o644,
            &format!("version = \"{version}\"\n"),
        )
        .file(HOOK_PROGRAM, 0o755, "#!/bin/sh\nexit 0\n")
}

/// A rig whose installs write the package's files on its host, with the operator's file at
/// [`FILE`], mode 0640.
fn rig(name: &str) -> AptRig {
    let apt = AptRig::new(name);
    apt.host.place_files(apt.rig.scratch.path());
    let at = apt.rig.host(FILE);
    fs::create_dir_all(at.parent().expect("a parent")).expect("/etc/web");
    fs::write(&at, HAND).expect("the operator's file");
    fs::set_permissions(&at, fs::Permissions::from_mode(0o640)).expect("its mode");
    apt
}

/// The bytes, mode, owner and group at [`FILE`].
fn seen(apt: &AptRig) -> (String, u32, u32, u32) {
    let at = apt.rig.host(FILE);
    let meta = fs::symlink_metadata(&at).expect("a file at the path");
    (
        fs::read_to_string(&at).expect("its bytes"),
        meta.mode() & 0o7777,
        meta.uid(),
        meta.gid(),
    )
}

/// The kept files' set of the service.
fn set_dir(apt: &AptRig) -> PathBuf {
    apt.rig.scratch.path().join("kept").join(SERVICE)
}

/// The order of the config package at `package_version`, `version` of the service: a hook
/// command, no `run` unit.
fn config_order(
    order: hostkeeper_proto::hostkeeper::agent::v1::Order,
    package_version: &str,
    pin: &str,
) -> hostkeeper_proto::hostkeeper::agent::v1::Order {
    let command = [&format!("/{HOOK_PROGRAM}")[..], "update-hook"];
    let mut target = hooked_entry(package_version, pin, &command);
    target.manifest.as_mut().expect("a manifest").run = Vec::new();
    with_entry(order, &target)
}

/// A daemon whose hook's `rollback` does what jobscheduler's does: it writes the original file
/// back, then the daemon removes the package.
fn daemon_whose_rollback_restores(apt: &AptRig) -> Daemon {
    let at = apt.rig.host(FILE);
    let ports = Ports {
        launcher: Tap::before(
            apt.rig.launcher.clone(),
            apt.rig.placed_log.clone(),
            move |step| {
                if step == Step::Rollback {
                    fs::write(&at, HAND).expect("the hook's rollback writes the file back");
                }
            },
        ),
        ..apt.ports()
    };
    Daemon::new(config(), apt.rig.dirs(), ports, OWN)
}

/// Proves (a), the case reported: a first install whose `verify` fails runs the hook's
/// `rollback` (which writes the original file back), then purges the package; the operator's
/// file is at its path after, with the same bytes, mode, owner and group, and the kept copy is
/// gone.
///
/// Catches: the purge after the hook's `rollback` deleting the file the hook put back.
#[test]
fn a_hand_placed_file_survives_a_failed_first_install_with_its_bytes_mode_and_owner() {
    let apt = rig("apt-kept-failed-first");
    let before = seen(&apt);
    let daemon = daemon_whose_rollback_restores(&apt);
    apt.rig
        .launcher
        .replies(SERVICE, Step::Verify, &[(1, r#"{"reason":"not_ready"}"#)]);
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    let Reply::Finished { verdict, .. } = reply else {
        panic!("not finished: {reply:?}");
    };
    assert!(matches!(verdict, Verdict::Failed { .. }), "{verdict:?}");
    assert_eq!(apt.host.installed(SERVICE), None, "the package is purged");
    assert_eq!(recorded(&daemon), None);
    assert_eq!(seen(&apt), before);
    assert!(!set_dir(&apt).exists(), "the kept copy is dropped");
}

/// Proves (a) for a first install that fails in dpkg itself (a maintainer script): the package,
/// unpacked over the file, is purged and the operator's file is put back.
#[test]
fn a_hand_placed_file_survives_a_first_install_whose_package_script_fails() {
    let apt = rig("apt-kept-script-fails");
    let before = seen(&apt);
    let daemon = apt.daemon();
    apt.host
        .edit(|s| s.faults.script_fails_for = Some("1.0.0-1".to_owned()));
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, failed(Class::Release, "dpkg_error"));
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(seen(&apt), before);
    assert!(!set_dir(&apt).exists());
}

/// Proves (b) and (e): an install over the file replaces it with the package's, keeps a copy in
/// a set only root can read (0700, each file 0600), an update keeps the set as it is, and the
/// uninstall puts the operator's file back with its bytes, mode, owner and group, then drops the
/// set.
#[test]
fn a_hand_placed_file_survives_an_install_an_update_and_the_uninstall() {
    let apt = rig("apt-kept-uninstall");
    let before = seen(&apt);
    let daemon = apt.daemon();
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, succeeded());
    assert_eq!(seen(&apt).0, "version = \"1.0.0-1\"\n");
    let set = set_dir(&apt);
    let mode = |path: PathBuf| fs::symlink_metadata(path).expect("there").mode() & 0o7777;
    assert_eq!(mode(set.parent().expect("the store").to_path_buf()), 0o700);
    assert_eq!(mode(set.clone()), 0o700);
    assert_eq!(mode(set.join("0")), 0o600);
    assert_eq!(mode(set.join("kept.toml")), 0o600);
    assert_eq!(fs::read_to_string(set.join("0")).expect("the copy"), HAND);
    let index = fs::read_to_string(set.join("kept.toml")).expect("the index");

    let two = apt.publish(config_package("2.0.0-1"));
    let reply = daemon.order(&config_order(update(SERVICE, "2.0.0", &two, 1), "2.0.0-1", &two));
    assert_eq!(reply, succeeded());
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(seen(&apt).0, "version = \"2.0.0-1\"\n");
    assert_eq!(
        fs::read_to_string(set.join("kept.toml")).expect("the index"),
        index,
        "the update keeps the set"
    );

    assert_eq!(daemon.order(&uninstall(SERVICE, 1)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(seen(&apt), before);
    assert!(!set.exists());
    assert!(!apt.rig.host(format!("/{HOOK_PROGRAM}")).exists());
    assert_eq!(linked(&apt, "current"), None);
}

/// Proves (c): a file another installed package owns is not kept, and the first install is
/// refused as dpkg refuses it today (`dpkg_error`): the file is the other package's as before,
/// and no copy is made.
#[test]
fn a_file_another_package_owns_is_not_kept_and_the_first_install_is_refused() {
    let apt = rig("apt-kept-owned");
    apt.host.edit(|s| {
        s.owners.insert(FILE.to_owned(), "web-legacy".to_owned());
    });
    let before = seen(&apt);
    let daemon = apt.daemon();
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, failed(Class::Release, "dpkg_error"));
    assert_eq!(seen(&apt), before);
    assert!(!apt.rig.scratch.path().join("kept").exists());
}

/// Proves (d): with no file at the path before the install, the path is empty after the
/// uninstall and no alert is raised, as before this store.
#[test]
fn a_path_with_no_file_before_the_install_is_empty_after_the_uninstall() {
    let apt = AptRig::new("apt-kept-nothing");
    apt.host.place_files(apt.rig.scratch.path());
    let (daemon, log) = alerting(apt.daemon());
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, succeeded());
    assert!(apt.rig.host(FILE).is_file());
    assert_eq!(daemon.order(&uninstall(SERVICE, 1)), succeeded());
    assert!(!apt.rig.host(FILE).exists());
    assert!(!apt.rig.scratch.path().join("kept").exists());
    assert_eq!(lost_lines(&log), Vec::<String>::new());
}

/// Proves a kept copy missing at the uninstall is what the design says it is on purpose: the
/// uninstall completes (package purged, record and versions gone), the path stays empty, and an
/// alert at error level names the path, exactly.
#[test]
fn a_kept_copy_missing_at_the_uninstall_leaves_the_path_empty_and_raises_an_alert() {
    let apt = rig("apt-kept-lost");
    let (daemon, log) = alerting(apt.daemon());
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, succeeded());
    fs::remove_file(set_dir(&apt).join("0")).expect("the copy goes");

    assert_eq!(daemon.order(&uninstall(SERVICE, 1)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(recorded(&daemon), None);
    assert!(!apt.rig.host(FILE).exists(), "the path stays empty");
    assert!(!set_dir(&apt).exists());
    assert_eq!(
        lost_lines(&log),
        [format!(
            "hostkeeper daemon: level=error alert=kept_file_lost service={SERVICE} path={FILE}: \
             the file kept at its first install was not put back: the kept copy: No such file \
             or directory (os error 2); no copy of it is left"
        )]
    );
}

/// Proves a kept copy whose bytes changed is not written to the path: the uninstall completes,
/// the path stays empty, the copy is set aside where the alert says it is, with its bytes, so
/// the operator can put the file back by hand.
#[test]
fn a_kept_copy_that_changed_is_set_aside_and_the_alert_names_where() {
    let apt = rig("apt-kept-aside");
    let (daemon, log) = alerting(apt.daemon());
    let one = apt.publish(config_package("1.0.0-1"));
    let reply = daemon.order(&config_order(install(SERVICE, "1.0.0", &one, 1), "1.0.0-1", &one));
    assert_eq!(reply, succeeded());
    fs::write(set_dir(&apt).join("0"), "changed in the store\n").expect("the copy changes");

    assert_eq!(daemon.order(&uninstall(SERVICE, 1)), succeeded());
    assert!(!apt.rig.host(FILE).exists(), "the path stays empty");
    let aside = apt.rig.scratch.path().join("kept").join(format!(".lost-{SERVICE}"));
    assert_eq!(
        fs::read_to_string(aside.join("0")).expect("the copy set aside"),
        "changed in the store\n"
    );
    let lines = lost_lines(&log);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with(&format!("; its copy is at {}", aside.join("0").display())),
        "{lines:?}"
    );
    assert!(lines[0].contains(&format!("path={FILE}: ")), "{lines:?}");
}

/// The log's lines of the alert of a kept file not put back.
fn lost_lines(log: &hostkeeper_daemon::testing::Log) -> Vec<String> {
    lines(log)
        .into_iter()
        .filter(|line| line.contains("alert=kept_file_lost"))
        .collect()
}
