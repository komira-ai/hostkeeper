//! `cutover`, `verify`, `rollback`, `cleanup` and `uninstall`: what each does to the boot
//! directory and asks of `systemctl`, run again, and at its boundaries.

use std::fs;

use hostkeeper_hook::{Class, Outcome, Step};

use super::kit::{FakeSystemctl, Setup, plain};

/// Cutover points `current` at the target with a relative link, restarts the unit, and run
/// again does the same; a later version replaces the link.
///
/// Catches a link to an absolute path, which the sealed `--dir` of the unit would not survive a
/// move of the directory by, and a cutover that restarts nothing.
#[test]
fn cutover_points_current_at_the_target_and_restarts_the_unit() {
    let setup = Setup::new("cutover");
    setup.versions(&["1.0.0", "1.2.3"]);
    let systemctl = FakeSystemctl::default();
    let params = setup.params(Some("1.2.3"), Some("1.0.0"), "");
    for _ in 0..2 {
        plain(&setup, Step::Cutover, &params, &systemctl).succeeded();
        assert_eq!(setup.current().as_deref(), Some("1.2.3"));
    }
    assert_eq!(
        systemctl.called(),
        ["restart pxe.service", "restart pxe.service"]
    );
    let params = setup.params(Some("1.0.0"), None, "");
    plain(&setup, Step::Cutover, &params, &systemctl).succeeded();
    assert_eq!(setup.current().as_deref(), Some("1.0.0"));
    assert_eq!(setup.listing(), ["1.0.0", "1.2.3", "current"]);
}

/// A version that was not prepared is refused and nothing is changed or restarted; a restart
/// that fails or cannot run is `not_started`.
///
/// Catches a cutover that points the served directory at a boot set that is not there.
#[test]
fn cutover_refuses_what_was_not_prepared_and_a_restart_that_fails() {
    let setup = Setup::new("cutover-refused");
    setup.versions(&["1.0.0"]);
    let systemctl = FakeSystemctl::default();
    let params = setup.params(Some("1.2.3"), None, "");
    let ended = plain(&setup, Step::Cutover, &params, &systemctl);
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_prepared");
    assert_eq!(
        detail,
        format!(
            "{} is not a directory: the version was not prepared",
            setup.boot.join("1.2.3").display()
        )
    );
    assert!(systemctl.called().is_empty());
    assert_eq!(setup.current(), None);

    setup.versions(&["1.2.3"]);
    let failing = FakeSystemctl::answering(&[1]);
    let ended = plain(&setup, Step::Cutover, &params, &failing);
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_started");
    assert_eq!(detail, "systemctl restart pxe.service exited 1");
    let ended = plain(&setup, Step::Cutover, &params, &FakeSystemctl::missing());
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_started");
    assert_eq!(
        detail,
        "systemctl restart pxe.service could not run: no systemctl"
    );
}

/// A `current` that is a directory cannot be replaced by the link: the step fails and names
/// the path.
///
/// Catches an `io_failed` that is swallowed, so the unit would be restarted on the old set.
#[test]
fn cutover_names_a_current_it_cannot_replace() {
    let setup = Setup::new("cutover-io");
    setup.versions(&["1.2.3"]);
    fs::create_dir_all(setup.boot.join("current/inside")).expect("a directory");
    let systemctl = FakeSystemctl::default();
    let params = setup.params(Some("1.2.3"), None, "");
    let ended = plain(&setup, Step::Cutover, &params, &systemctl);
    let detail = ended.refused(Outcome::Failure, Class::Environment, "io_failed");
    assert!(
        detail.starts_with(&format!("{}: ", setup.boot.join("current").display())),
        "{detail}"
    );
    assert!(systemctl.called().is_empty());
}

/// Verify succeeds when the unit is active, and otherwise is deferred (exit 75) with the retry
/// wait and the reason `not_active`, whether the unit is inactive or `systemctl` cannot run.
///
/// Catches a verify that fails the update for a unit still starting, and one that passes for
/// a unit that is down.
#[test]
fn verify_passes_an_active_unit_and_defers_otherwise() {
    let setup = Setup::new("verify");
    let params = setup.params(None, None, "");
    let systemctl = FakeSystemctl::default();
    plain(&setup, Step::Verify, &params, &systemctl).succeeded();
    assert_eq!(systemctl.called(), ["is-active --quiet pxe.service"]);
    for systemctl in [FakeSystemctl::answering(&[3]), FakeSystemctl::missing()] {
        let ended = plain(&setup, Step::Verify, &params, &systemctl);
        let detail = ended.refused(Outcome::Deferred, Class::Environment, "not_active");
        assert_eq!(detail, "pxe.service is not active");
        assert_eq!(ended.report().retry_after_secs, Some(2));
        assert_eq!(ended.code, 75);
    }
}

/// With a previous boot set, rollback points `current` back at it, restarts the unit, removes
/// the target's directory, and does the same again.
///
/// Catches a rollback that leaves the unit serving the failed version.
#[test]
fn rollback_restores_the_previous_boot_set() {
    let setup = Setup::new("rollback");
    setup.versions(&["1.0.0", "1.2.3"]);
    let systemctl = FakeSystemctl::default();
    let params = setup.params(Some("1.2.3"), Some("1.0.0"), "");
    for _ in 0..2 {
        plain(&setup, Step::Rollback, &params, &systemctl).succeeded();
        assert_eq!(setup.current().as_deref(), Some("1.0.0"));
        assert_eq!(setup.listing(), ["1.0.0", "current"]);
    }
    assert_eq!(
        systemctl.called(),
        ["restart pxe.service", "restart pxe.service"]
    );
}

/// A rollback to the version that is also the target keeps its directory.
///
/// Catches a rollback that deletes the boot set it has just put back.
#[test]
fn rollback_keeps_a_target_that_is_the_previous() {
    let setup = Setup::new("rollback-same");
    setup.versions(&["1.2.3"]);
    let params = setup.params(Some("1.2.3"), Some("1.2.3"), "");
    plain(&setup, Step::Rollback, &params, &FakeSystemctl::default()).succeeded();
    assert_eq!(setup.listing(), ["1.2.3", "current"]);
}

/// A first install's rollback (no previous version, or none whose directory is there) stops
/// the unit, accepting exit 5 (not loaded), removes `current` and the target, and may be run
/// again.
///
/// Catches a first-install rollback that fails because the daemon unlinked the unit first.
#[test]
fn a_first_install_rolls_back_by_stopping_the_unit() {
    let setup = Setup::new("rollback-first");
    setup.versions(&["1.2.3"]);
    std::os::unix::fs::symlink("1.2.3", setup.boot.join("current")).expect("a link");
    for (previous, code) in [(None, 5), (Some("0.9.0"), 0), (None, 5)] {
        let systemctl = FakeSystemctl::answering(&[code]);
        let params = setup.params(Some("1.2.3"), previous, "");
        plain(&setup, Step::Rollback, &params, &systemctl).succeeded();
        assert_eq!(systemctl.called(), ["stop pxe.service"]);
        assert_eq!(setup.listing(), Vec::<String>::new());
    }
}

/// A stop that fails with another code, one that cannot run, a restart that fails, and a
/// `current` that cannot be removed each fail the rollback with the reason that names it.
///
/// Catches a rollback reported done while the unit still serves.
#[test]
fn rollback_refuses_a_stop_or_restart_that_fails() {
    let setup = Setup::new("rollback-refused");
    setup.versions(&["1.0.0", "1.2.3"]);
    let first = setup.params(Some("1.2.3"), None, "");
    let ended = plain(
        &setup,
        Step::Rollback,
        &first,
        &FakeSystemctl::answering(&[1]),
    );
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_stopped");
    assert_eq!(detail, "systemctl stop pxe.service exited 1");
    let ended = plain(&setup, Step::Rollback, &first, &FakeSystemctl::missing());
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_stopped");
    assert_eq!(
        detail,
        "systemctl stop pxe.service could not run: no systemctl"
    );
    let update = setup.params(Some("1.2.3"), Some("1.0.0"), "");
    let ended = plain(
        &setup,
        Step::Rollback,
        &update,
        &FakeSystemctl::answering(&[4]),
    );
    let detail = ended.refused(Outcome::Failure, Class::Release, "not_restarted");
    assert_eq!(detail, "systemctl restart pxe.service exited 4");
    assert_eq!(setup.listing(), ["1.0.0", "1.2.3", "current"]);

    fs::remove_file(setup.boot.join("current")).expect("unlink");
    fs::create_dir_all(setup.boot.join("current/inside")).expect("a directory");
    let ended = plain(&setup, Step::Rollback, &first, &FakeSystemctl::default());
    ended.refused(Outcome::Failure, Class::Environment, "io_failed");
    assert!(setup.boot.join("1.2.3").exists());
}

/// Cleanup keeps `current` and the target and removes every other entry, directories with
/// what they hold and plain files alike; run again it changes nothing.
///
/// Catches a cleanup that removes the set being served, or leaves old boot sets (several GiB
/// each) to fill the disk.
#[test]
fn cleanup_keeps_only_current_and_the_target() {
    let setup = Setup::new("cleanup");
    setup.versions(&["0.9.0", "1.0.0", "1.2.3"]);
    fs::write(setup.boot.join("stray"), "x").expect("a file");
    std::os::unix::fs::symlink("1.0.0", setup.boot.join("current")).expect("a link");
    std::os::unix::fs::symlink("1.0.0", setup.boot.join("other")).expect("a link");
    let params = setup.params(Some("1.2.3"), Some("1.0.0"), "");
    let systemctl = FakeSystemctl::default();
    for _ in 0..2 {
        plain(&setup, Step::Cleanup, &params, &systemctl).succeeded();
        assert_eq!(setup.listing(), ["1.2.3", "current"]);
    }
    assert!(systemctl.called().is_empty());
    assert_eq!(setup.current().as_deref(), Some("1.0.0"));
}

/// Cleanup of a boot directory that is not there makes it empty.
///
/// Catches a cleanup that fails on a directory the update never filled.
#[test]
fn cleanup_of_an_absent_boot_directory_leaves_it_empty() {
    let setup = Setup::new("cleanup-absent");
    let params = setup.params(Some("1.2.3"), None, "");
    plain(&setup, Step::Cleanup, &params, &FakeSystemctl::default()).succeeded();
    assert_eq!(setup.listing(), Vec::<String>::new());
}

/// Uninstall removes the boot directory whole, with its links and boot sets, calls nothing of
/// the service manager, and an absent directory is done.
///
/// Catches an uninstall that leaves gigabytes behind, and one that fails the second time.
#[test]
fn uninstall_removes_the_boot_directory_whole() {
    let setup = Setup::new("uninstall");
    setup.versions(&["1.2.3"]);
    std::os::unix::fs::symlink("1.2.3", setup.boot.join("current")).expect("a link");
    let systemctl = FakeSystemctl::default();
    let params = setup.params(None, None, "");
    for _ in 0..2 {
        plain(&setup, Step::Uninstall, &params, &systemctl).succeeded();
        assert!(!setup.boot.exists());
    }
    assert!(systemctl.called().is_empty());
    assert!(setup.settings.exists());
}
