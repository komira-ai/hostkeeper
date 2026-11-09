//! `remove` and `retire`: the package released and purged whatever version is installed, the
//! purge tried again while the lock is held, the links taken away and the record dropped.

use hostkeeper_install::repository::{Class, Drift, Outcome, Reason};

use crate::harness::*;

use Class::Environment;
use Outcome::{Deferred, Failed};

fn installed_env(name: &str) -> Env {
    let env = Env::new(name);
    env.installed(&mdm_deb(V1), &RUN3);
    env
}

/// Proves a remove lifts the hold and then purges, takes away the links hostkeeper wrote,
/// reloads, and drops the record, in that order; it reports no drift for the recorded version.
#[test]
fn a_remove_unholds_purges_unlinks_and_clears() {
    let env = installed_env("apt-remove");
    let mark = env.event_log().len();
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert_eq!(removed.drift, None);
    let log: Vec<String> = env.event_log().into_iter().skip(mark).collect();
    assert_eq!(
        log,
        [
            "run dpkg-query",
            "run apt-mark unhold",
            "run dpkg --purge",
            "run systemctl daemon-reload",
            "clear",
        ]
    );
    assert!(env.host.installed(PACKAGE).is_none());
    assert!(env.records.get(SERVICE).is_none());
    for link in run3_links() {
        assert!(!at_root(&env, &link).exists() & at_root(&env, &link).symlink_metadata().is_err());
    }
    assert!(
        env.host.calls().iter().skip(13).all(|c| c.locked),
        "under the installer lock"
    );
}

/// Proves a remove purges whatever version is installed and reports the drift: the version found
/// and the one recorded.
#[test]
fn a_remove_purges_a_version_it_did_not_set_and_reports_it() {
    let env = installed_env("apt-remove-drift");
    env.host.set_installed(PACKAGE, "9.9-1", 'i', false);
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert_eq!(
        removed.drift,
        Some(Drift {
            found: Some("9.9-1".to_owned()),
            recorded: Some(V1.to_owned())
        })
    );
    assert!(env.host.installed(PACKAGE).is_none());
}

/// Proves a service with no record is a no-op: no tool runs, nothing is changed.
#[test]
fn a_remove_without_a_record_does_nothing() {
    let env = Env::new("apt-remove-nothing");
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert_eq!(removed.drift, None);
    assert!(env.host.calls().is_empty());
}

/// Proves a recorded package that is no longer installed is purged all the same, with no
/// `unhold`, and reported as drift with `found` none.
#[test]
fn a_remove_of_a_package_already_gone_skips_the_unhold() {
    let env = installed_env("apt-remove-gone");
    env.host.edit(|s| s.installed.clear());
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert_eq!(
        removed.drift,
        Some(Drift {
            found: None,
            recorded: Some(V1.to_owned())
        })
    );
    assert!(env.host.calls_of("apt-mark unhold").is_empty());
    assert_eq!(env.host.calls_of("dpkg --purge").len(), 1);
}

/// Proves the purge is tried again while the lock is held, within `remove_secs`: three busy
/// answers are three tries and three pauses of two seconds, then it succeeds; a `remove_secs`
/// of one is a single try.
#[test]
fn a_purge_is_tried_again_while_busy() {
    let env = installed_env("apt-remove-busy");
    env.host.edit(|s| {
        s.faults.busy.insert("purge", 3);
    });
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed in the end");
    assert_eq!(env.pause.calls(), [2, 2, 2]);
    assert_eq!(env.host.calls_of("dpkg --purge").len(), 4);
    assert!(env.host.installed(PACKAGE).is_none());
}

/// Proves a purge that stays busy past the tries `remove_secs` allows is `package_manager_busy`,
/// deferred, environment, with the record and the links still there.
#[test]
fn a_purge_busy_past_its_tries_defers() {
    let env = installed_env("apt-remove-busy-long");
    env.host.edit(|s| {
        s.faults.busy.insert("purge", 99);
    });
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("busy");
    assert_failure(&failure, Reason::PackageManagerBusy, Environment, Deferred);
    assert!(failure.detail().contains("10s"));
    assert_eq!(env.pause.calls(), [2, 2, 2, 2], "five tries, four pauses");
    assert_eq!(env.host.calls_of("dpkg --purge").len(), 5);
    assert!(env.records.get(SERVICE).is_some());
    assert!(env.host.installed(PACKAGE).is_some());
    for link in run3_links() {
        assert!(at_root(&env, &link).is_symlink());
    }

    let short = installed_env("apt-remove-busy-short");
    short.host.edit(|s| {
        s.faults.busy.insert("purge", 99);
    });
    let failure = short
        .installer()
        .remove(&service(SERVICE), 1)
        .expect_err("busy");
    assert_eq!(failure.reason(), Reason::PackageManagerBusy);
    assert!(
        short.pause.calls().is_empty(),
        "a bound of one second is one try"
    );
}

/// Proves a purge refused by a dependent package is `dependency_changes`, failed, environment,
/// and an interrupted dpkg is `dpkg_interrupted`; the record stays.
#[test]
fn a_purge_refused_or_interrupted_keeps_the_record() {
    let env = installed_env("apt-remove-refused");
    env.host.edit(|s| s.faults.purge_refused = true);
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("refused");
    assert_failure(&failure, Reason::DependencyChanges, Environment, Failed);
    assert!(env.records.get(SERVICE).is_some());
    env.host.edit(|s| {
        s.faults.purge_refused = false;
        s.faults.interrupted = true;
    });
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("interrupted");
    assert_failure(&failure, Reason::DpkgInterrupted, Environment, Failed);
    assert!(env.records.get(SERVICE).is_some());
}

/// Proves a remove takes away only what hostkeeper wrote: a link of the recorded set that is
/// gone already is no error, a recorded path that is no link any more stays, and a link that was
/// never recorded stays.
#[test]
fn a_remove_leaves_what_hostkeeper_did_not_write() {
    let env = Env::new("apt-remove-foreign");
    let wants = at_root(&env, "/etc/systemd/system/multi-user.target.wants");
    std::fs::create_dir_all(&wants).expect("a directory");
    std::os::unix::fs::symlink(
        "/usr/lib/systemd/system/mdm.service",
        wants.join("mdm.service"),
    )
    .expect("a foreign link");
    env.installed(&mdm_deb(V1), &RUN3);
    let timer = at_root(&env, "/etc/systemd/system/timers.target.wants/mdm.timer");
    std::fs::remove_file(&timer).expect("removed by hand");
    let worker = wants.join("mdm-worker.service");
    std::fs::remove_file(&worker).expect("removed");
    std::fs::write(&worker, "an operator's file").expect("a file");
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert!(
        wants.join("mdm.service").is_symlink(),
        "a link never recorded stays"
    );
    assert!(worker.is_file(), "a file that is no link stays");
    assert!(!timer.exists());
}

/// Proves a service manager that cannot reload fails the remove with `io_error` and keeps the
/// record.
#[test]
fn a_failed_reload_keeps_the_record() {
    use hostkeeper_install::repository::ToolOutput;
    let env = installed_env("apt-remove-reload");
    env.host
        .answer("systemctl daemon-reload", 0, Ok(ToolOutput::exit(1, "")));
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("fails");
    assert_eq!(failure.reason(), Reason::IoError);
    assert!(failure.detail().contains("daemon-reload failed"));
    assert!(env.records.get(SERVICE).is_some());
}

/// Proves a retire deletes the version directory, a read-only tree in it included, and its seal
/// beside it, runs no tool, and leaves the installed package alone; a directory already gone is
/// no error.
#[test]
fn a_retire_deletes_the_directory_and_its_seal_and_runs_no_tool() {
    use std::os::unix::fs::PermissionsExt;
    let env = installed_env("apt-retire");
    let calls = env.host.calls().len();
    let version_dir = env.scratch.path().join("versions-1.3.0");
    let inner = version_dir.join("tree/usr/bin");
    std::fs::create_dir_all(&inner).expect("a tree");
    std::fs::write(inner.join("mdm"), "x").expect("a file");
    std::fs::write(version_dir.join("tree/link-target"), "x").expect("a file");
    std::os::unix::fs::symlink("link-target", version_dir.join("tree/link")).expect("a link");
    for dir in [
        &inner,
        &version_dir.join("tree/usr"),
        &version_dir.join("tree"),
        &version_dir,
    ] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("read-only");
    }
    let seal = env.scratch.path().join("versions-1.3.0.seal");
    std::fs::write(&seal, "seal").expect("a seal");
    env.installer().retire(&version_dir).expect("retired");
    assert!(!version_dir.exists() & !seal.exists());
    assert_eq!(env.host.calls().len(), calls, "no tool ran");
    assert_eq!(env.host.installed(PACKAGE).expect("installed").version, V1);
    env.installer()
        .retire(&version_dir)
        .expect("a second retire is a no-op");
}

/// Proves a recorded link that cannot be inspected (its directory is a file now) fails the remove
/// with `io_error` and keeps the record.
#[test]
fn a_link_that_cannot_be_inspected_fails_the_remove() {
    let env = installed_env("apt-remove-link-error");
    let wants = at_root(&env, "/etc/systemd/system/multi-user.target.wants");
    std::fs::remove_dir_all(&wants).expect("removed");
    std::fs::write(&wants, "a file").expect("a file");
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("fails");
    assert_eq!(failure.reason(), Reason::IoError);
    assert!(failure.detail().starts_with("the units' enablement links"));
    assert!(env.records.get(SERVICE).is_some());
}
