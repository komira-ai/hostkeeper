//! An `install` order for a package the host's package manager already has (packages-lifecycle.md
//! section 6.1, "A package that is already installed is adopted"): the package was installed by
//! hand and `placed.json` knows nothing of it, so the coordinator orders a first install. The
//! daemon adopts it and handles the order as an update from the installed version, so a failed
//! update puts that version back and hostkeeper never purges a package it did not install.

mod support;
mod support_apt;

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::target;
use hostkeeper_hook::{Class, Verdict};
use hostkeeper_placed::InstalledEntry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::{Daemon, Placement, Ports};
use hostkeeper_install::repository::ToolOutput;
use hostkeeper_placed::testing::FaultFs;
use hostkeeper_steps::record::CrashPoint;
use support::{OWN, alerting, config, crash_at, roots};
use support_apt::seen::{
    deferred, failed, held, install_of, labels_since, linked, recorded, succeeded,
};
use support_apt::{AptRig, REPO, SERVICE, package};

/// The version of the package installed by hand, and the one the order installs.
const BY_HAND: &str = "1.0.0-1";
const ORDERED: &str = "2.0.0-1";

fn purges(apt: &AptRig) -> usize {
    apt.host.calls_of("dpkg --purge").len()
}

/// Proves an `install` order for a package installed by hand is an update from that version: the
/// order ends `Succeeded`, the ordered version is installed and held, `current` names it and
/// `previous` the adopted version, whose directory is sealed with the entry of its package
/// version, and `placed.json` records the package at the ordered version.
///
/// Catches: an install that treats a package already on the host as absent and leaves no version
/// to go back to.
#[test]
fn an_install_of_a_package_installed_by_hand_is_an_update_from_it() {
    let apt = AptRig::new("adopt-install");
    let daemon = apt.daemon();
    apt.publish(package(BY_HAND));
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held(ORDERED));
    assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
    assert_eq!(linked(&apt, "previous").as_deref(), Some(BY_HAND));
    let sealed = target::read(&apt.version_dir(BY_HAND))
        .expect("reads")
        .expect("a target.json");
    assert_eq!(sealed.package_version.as_deref(), Some(BY_HAND));
    assert_eq!(
        recorded(&daemon).and_then(|e| e.version).as_deref(),
        Some(ORDERED)
    );
    assert_eq!(purges(&apt), 0);
}

/// Proves an `install` order for a package installed by hand whose update fails restores the
/// installed version and does not purge: the verdict is the update's, the package is installed
/// and held at the version it had, `current` names it, `placed.json` records it, and no purge ran.
///
/// Catches: a rollback of a first install that purges a package hostkeeper did not install.
#[test]
fn a_failed_install_of_a_package_installed_by_hand_restores_it_and_purges_nothing() {
    let apt = AptRig::new("adopt-rollback");
    let daemon = apt.daemon();
    apt.publish(package(BY_HAND));
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    apt.host
        .edit(|state| state.faults.script_fails_for = Some(ORDERED.to_owned()));
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        failed(Class::Release, "dpkg_error")
    );
    assert_eq!(apt.host.installed(SERVICE), held(BY_HAND));
    assert_eq!(linked(&apt, "current").as_deref(), Some(BY_HAND));
    assert_eq!(
        recorded(&daemon).and_then(|e| e.version).as_deref(),
        Some(BY_HAND)
    );
    assert_eq!(purges(&apt), 0);
}

/// Proves an `install` order for the version that is installed adopts it: the order ends
/// `Succeeded`, the package is held and recorded, `current` names the version and nothing is
/// purged.
///
/// Catches: an install of the installed version that installs it again as if it were absent.
#[test]
fn an_install_of_the_installed_version_adopts_it() {
    let apt = AptRig::new("adopt-same");
    let daemon = apt.daemon();
    let one = apt.publish(package(BY_HAND));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    assert_eq!(
        daemon.order(&install_of("1.0.0", BY_HAND, &one, 1)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held(BY_HAND));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert_eq!(
        recorded(&daemon).and_then(|e| e.version).as_deref(),
        Some(BY_HAND)
    );
}

/// Proves an `install` order for the version that is installed rolls back to that version and
/// purges nothing: the package manager is busy for the order's install, the order ends `Deferred`,
/// and the package is still installed and held, `current` names it and `placed.json` records it.
///
/// Catches: an install of the installed version that purges the package when its step fails.
#[test]
fn a_deferred_install_of_the_installed_version_keeps_the_package() {
    let apt = AptRig::new("adopt-same-busy");
    let daemon = apt.daemon();
    let one = apt.publish(package(BY_HAND));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    apt.host
        .edit(|state| state.faults.busy.insert("install", 1));
    assert_eq!(
        daemon.order(&install_of("1.0.0", BY_HAND, &one, 1)),
        deferred("package_manager_busy")
    );
    assert_eq!(purges(&apt), 0);
    assert_eq!(apt.host.installed(SERVICE), held(BY_HAND));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert_eq!(
        recorded(&daemon).and_then(|e| e.version).as_deref(),
        Some(BY_HAND)
    );
}

/// Proves a package that is not installed is still a first install: it is installed, held and
/// recorded with no `previous`, and a first install that fails is purged as it always was.
///
/// Catches: an adoption that takes an absent package for an installed one.
#[test]
fn an_install_of_an_absent_package_is_still_a_first_install() {
    let apt = AptRig::new("adopt-absent");
    let daemon = apt.daemon();
    let two = apt.publish(package(ORDERED));
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held(ORDERED));
    assert_eq!(linked(&apt, "previous"), None);

    let failing = AptRig::new("adopt-absent-fails");
    let daemon = failing.daemon();
    let two = failing.publish(package(ORDERED));
    failing
        .host
        .edit(|state| state.faults.script_fails_for = Some(ORDERED.to_owned()));
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        failed(Class::Release, "dpkg_error")
    );
    assert_eq!(failing.host.installed(SERVICE), None);
    assert_eq!(purges(&failing), 1);
}

/// Proves a package installed half-configured is not adopted: the order is a first install, so
/// the install repairs it as it did before (`drift_repaired`, with no `previous`).
///
/// Catches: an adoption of a package whose installation did not finish.
#[test]
fn a_half_installed_package_is_not_adopted() {
    let apt = AptRig::new("adopt-incomplete");
    let daemon = apt.daemon();
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'F', false);
    let reply = daemon.order(&install_of("2.0.0", ORDERED, &two, 1));
    assert!(
        matches!(&reply, Reply::Finished { reason, .. } if reason.as_deref() == Some("drift_repaired")),
        "{reply:?}"
    );
    assert_eq!(linked(&apt, "previous"), None);
}

/// Proves an installed version the repository does not list cannot be adopted: the order ends
/// `Failed` with `not_in_repository`, the package is left installed and unheld, and nothing is
/// recorded, linked or purged.
///
/// Catches: an adoption that installs over a package it cannot put back.
#[test]
fn an_installed_version_the_repository_lacks_ends_the_order_and_touches_nothing() {
    let apt = AptRig::new("adopt-unlisted");
    let daemon = apt.daemon();
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    let from = apt.host.calls().len();
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        deferred("not_in_repository")
    );
    assert_eq!(
        apt.host
            .installed(SERVICE)
            .map(|found| (found.version, found.held)),
        Some((BY_HAND.to_owned(), false))
    );
    assert_eq!(recorded(&daemon), None);
    assert_eq!(linked(&apt, "current"), None);
    assert!(
        !labels_since(&apt, from)
            .iter()
            .any(|l| l == "dpkg --purge" || l == "apt-get install")
    );
}

/// Proves an installed version that is no version name of a service (an epoch) cannot be
/// adopted: the order ends `Failed` with `manifest_invalid` and the package is left alone.
///
/// Catches: an adoption that names a version directory after text the packages directory refuses.
#[test]
fn an_installed_version_that_is_no_service_version_ends_the_order() {
    let apt = AptRig::new("adopt-epoch");
    let daemon = apt.daemon();
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, "1:1.0.0-1", 'i', false);
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        failed(Class::Release, "manifest_invalid")
    );
    assert_eq!(
        apt.host
            .installed(SERVICE)
            .map(|found| found.version)
            .as_deref(),
        Some("1:1.0.0-1")
    );
    assert_eq!(purges(&apt), 0);
}

/// Proves a package `placed.json` records for the service is hostkeeper's own and is not adopted:
/// the install is a first install, whose failed rollback purges it.
///
/// Catches: an adoption of a package an earlier try of the same install left behind, which
/// hostkeeper must remove.
#[test]
fn a_package_the_record_names_is_not_adopted() {
    let apt = AptRig::new("adopt-recorded");
    let daemon = apt.daemon();
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    let placer = daemon.ctx().placer();
    let lock = placer.lock().expect("the placed lock");
    let mut record = placer.store().load().expect("reads");
    let mut entry = InstalledEntry::new("apt");
    entry.package = Some(SERVICE.to_owned());
    entry.version = Some(BY_HAND.to_owned());
    entry.repository = Some(REPO.to_owned());
    record.installed.insert(SERVICE.to_owned(), entry);
    placer.store().save(&record, &lock).expect("saved");
    drop(lock);
    apt.host
        .edit(|state| state.faults.script_fails_for = Some(ORDERED.to_owned()));
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        failed(Class::Release, "dpkg_error")
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(purges(&apt), 1);
}

/// A daemon on the rig whose reads of the host's files fail at the `n`-th, once.
fn daemon_failing_read(apt: &AptRig, n: u64) -> Daemon {
    let fs =
        FaultFs::new(apt.rig.scratch.path(), apt.rig.placed_log.clone()).failing_read(n, false);
    daemon_on(apt, fs)
}

/// A daemon on the rig whose writes of the host's files fail from the `n`-th on.
fn daemon_failing_write(apt: &AptRig, n: u64) -> Daemon {
    daemon_on(
        apt,
        FaultFs::failing_at(apt.rig.scratch.path(), n, apt.rig.placed_log.clone()),
    )
}

fn daemon_on(apt: &AptRig, fs: FaultFs) -> Daemon {
    let ports = Ports {
        placement: Placement {
            roots: roots(),
            fs: Arc::new(fs),
        },
        ..apt.ports()
    };
    alerting(Daemon::new(config(), apt.rig.dirs(), ports, OWN)).0
}

/// An installed package, as a rig of an adoption has it: `BY_HAND` installed, both versions
/// published; answers the pin of `ORDERED`.
fn installed_by_hand(apt: &AptRig) -> String {
    apt.publish(package(BY_HAND));
    let two = apt.publish(package(ORDERED));
    apt.host.set_installed(SERVICE, BY_HAND, 'i', false);
    two
}

/// Proves a read of `placed.json` that fails while the adoption looks for a record, and one that
/// fails while it records the package, each end the order `Failed`, `environment`, `io_error`
/// with the package left as it was (installed, not held, no record, no `current`).
///
/// Catches: an adoption that goes on, or purges, after the host's files failed.
#[test]
fn a_failed_read_of_the_record_ends_the_order_and_leaves_the_package_alone() {
    for read in [3, 4] {
        let apt = AptRig::new(&format!("adopt-read-{read}"));
        let two = installed_by_hand(&apt);
        let daemon = daemon_failing_read(&apt, read);
        assert_eq!(
            daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
            failed(Class::Environment, "io_error"),
            "read {read}"
        );
        assert_eq!(
            apt.host
                .installed(SERVICE)
                .map(|found| (found.version, found.held)),
            Some((BY_HAND.to_owned(), false))
        );
        assert_eq!(recorded(&daemon), None);
        assert_eq!(linked(&apt, "current"), None);
        assert_eq!(purges(&apt), 0);
    }
}

/// Proves an adoption that stops after the package was recorded and before `current` was made
/// (the link could not be written) is done again by the next order, which ends `Succeeded`: the
/// record an adoption writes names no repository, so it is not a package hostkeeper installed,
/// and the order replaces the adopted version as an update does.
///
/// Catches: an order retried after a crash that takes the adopted package for one hostkeeper
/// installed, and purges it.
#[test]
fn an_adoption_that_stopped_before_current_is_done_again() {
    let apt = AptRig::new("adopt-link-fails");
    let two = installed_by_hand(&apt);
    let stopped = daemon_failing_write(&apt, 5);
    let reply = stopped.order(&install_of("2.0.0", ORDERED, &two, 1));
    assert!(
        matches!(&reply, Reply::Refused { refusal, .. } if refusal.token() == "link_failed"),
        "{reply:?}"
    );
    assert_eq!(linked(&apt, "current"), None);
    assert_eq!(
        recorded(&stopped).map(|entry| (entry.version, entry.repository)),
        Some((Some(BY_HAND.to_owned()), Some(String::new())))
    );
    let daemon = apt.daemon();
    assert_eq!(
        daemon.order(&install_of("2.0.0", ORDERED, &two, 1)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held(ORDERED));
    assert_eq!(linked(&apt, "previous").as_deref(), Some(BY_HAND));
    assert_eq!(purges(&apt), 0);
}

/// Proves a package manager that cannot be asked ends the order and touches nothing: when the
/// query of the adoption's check cannot run, and when the query that records the adoption finds
/// the package gone (`dpkg_error`) or cannot run, the package is not held, recorded or linked.
///
/// Catches: an adoption that records a package the package manager no longer shows.
#[test]
fn a_package_manager_that_cannot_be_asked_ends_the_order_and_touches_nothing() {
    let gone = ToolOutput {
        code: Some(1),
        stdout: Vec::new(),
        stderr: b"dpkg-query: no packages found matching web".to_vec(),
        cut: false,
    };
    for (name, skip, answer, expected) in [
        ("first-cannot-run", 0, Err("no dpkg-query"), None),
        ("second-cannot-run", 1, Err("no dpkg-query"), None),
        (
            "second-gone",
            1,
            Ok(gone),
            Some(failed(Class::Release, "dpkg_error")),
        ),
    ] {
        let apt = AptRig::new(&format!("adopt-query-{name}"));
        let two = installed_by_hand(&apt);
        let daemon = apt.daemon();
        apt.host.answer(
            "dpkg-query",
            skip,
            answer.map_err(|why| hostkeeper_install::repository::tools::ToolError(why.to_owned())),
        );
        let reply = daemon.order(&install_of("2.0.0", ORDERED, &two, 1));
        if let Some(expected) = expected {
            assert_eq!(reply, expected, "{name}");
        } else {
            assert!(matches!(reply, Reply::Finished { .. }), "{name}: {reply:?}");
        }
        assert_eq!(recorded(&daemon), None, "{name}");
        assert_eq!(linked(&apt, "current"), None, "{name}");
        assert_eq!(purges(&apt), 0, "{name}");
    }
}

/// Proves an adopted install the daemon died in, after any write of its step record, is resumed
/// by the order sent again and ends `Succeeded` as an update from the adopted version: the ordered
/// version is installed and held, `previous` names the adopted version and nothing is purged.
///
/// Catches: a resumed adoption whose order has no previous release, which would purge the package
/// if it then rolled back.
#[test]
fn an_adopted_install_killed_after_any_write_of_its_record_is_resumed_as_an_update() {
    let mut writes = 0;
    for n in 1.. {
        let apt = AptRig::new(&format!("adopt-resume-{n}"));
        let two = installed_by_hand(&apt);
        let count = Arc::new(AtomicU64::new(0));
        let dying = apt.daemon().with_stores(crash_at(
            Some(CrashPoint::AfterWrite(n)),
            Arc::clone(&count),
        ));
        let order = install_of("2.0.0", ORDERED, &two, 1);
        let reply = dying.order(&order);
        drop(dying);
        if count.load(Ordering::SeqCst) < n {
            assert!(
                matches!(
                    reply,
                    Reply::Finished {
                        verdict: Verdict::Succeeded { .. },
                        ..
                    }
                ),
                "after {n} writes: {reply:?}"
            );
            writes = n - 1;
            break;
        }
        assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
        let daemon = apt.daemon();
        let resumed = daemon.order(&order);
        assert!(
            matches!(
                resumed,
                Reply::Finished {
                    verdict: Verdict::Succeeded { .. },
                    ..
                }
            ),
            "after write {n}: {resumed:?}"
        );
        assert_eq!(
            apt.host.installed(SERVICE),
            held(ORDERED),
            "after write {n}"
        );
        assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
        assert_eq!(linked(&apt, "previous").as_deref(), Some(BY_HAND));
        assert_eq!(purges(&apt), 0, "after write {n}");
    }
    assert!(writes >= 4, "the order wrote its record {writes} times");
}
