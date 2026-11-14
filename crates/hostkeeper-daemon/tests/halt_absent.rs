//! The step record of a halted install of a service that is absent: a halt holds the orders of a
//! service until `clear_halt`, but an `install` of a service with no release installed has nothing
//! left to protect, so the record's halt no longer refuses it (the incident of a first install
//! whose rollback failed after the package was purged). A halt of a service that is installed, a
//! resend of the halted order itself and every other verb still stay halted.

mod support;
mod support_apt;
mod support_host;

use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::prune::LayoutPrune;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_placed::testing::FaultFs;
use hostkeeper_steps::record::State;
use hostkeeper_steps::{CrashPoint, RecordStore};
use support::{alerting, lines};
use support_host::NativeHost;

fn halted(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Finished {
            verdict: Verdict::Halt,
            ..
        }
    )
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// A first install of `web` 2.0 whose `prepare` exits 76: halted, with nothing installed.
fn halted_install(farm: &NativeHost) -> Daemon {
    let rig = farm.rig();
    farm.publish("web", "2.0");
    farm.publish("web", "2.1");
    let daemon = farm.daemon();
    rig.launcher.exits("web", Step::Prepare, &[76]);
    let reply = daemon.order(&farm.install("web", "2.0", 1));
    assert!(halted(&reply), "{reply:?}");
    daemon
}

/// Proves: the next install of a service whose halted install left nothing installed is run, not
/// refused `halted`: the install succeeds, the record then holds its success, the daemon says in
/// its log, once, that it lifted the halt of the absent service, no held halt generation is
/// written, and an update of the installed service follows.
///
/// Catches: an absent service held halted by the record of its failed install until an
/// operator's `clear_halt`, though the coordinator orders the install again.
#[test]
fn an_install_of_an_absent_service_runs_over_the_halt_of_its_failed_install() {
    let farm = NativeHost::new("halt-absent-install");
    let rig = farm.rig();
    let (daemon, log) = alerting(halted_install(&farm));
    assert_eq!(daemon.order(&farm.install("web", "2.0", 2)), succeeded());
    let held = daemon.order(&farm.update("web", "2.1", 3));
    assert_eq!(held, succeeded(), "installed, so not halted: {held:?}");
    let record = rig.record("web").expect("the record");
    assert!(matches!(record.state, State::Finished { .. }));
    let said = lines(&log);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("alert=halt_forgotten service=web"),
        "{said:?}"
    );
    assert!(!rig.state.join("web").join("held-halt").exists());
}

/// Proves: a resend of the halted install itself (the same package and try) is answered from the
/// record with its halt, as every resend is, though the service is absent; an update of the absent
/// service is not an install and stays refused `halted`.
///
/// Catches: a resend that runs the install again.
#[test]
fn a_resend_of_the_halted_install_is_answered_from_the_record() {
    let farm = NativeHost::new("halt-absent-stays");
    let daemon = halted_install(&farm);
    let resent = daemon.order(&farm.install("web", "2.0", 1));
    assert!(
        matches!(
            resent,
            Reply::Finished {
                verdict: Verdict::Halt,
                resent: true,
                ..
            }
        ),
        "{resent:?}"
    );
    // An update of an absent service is not an install: it stays refused by the halt.
    let update = daemon.order(&farm.update("web", "2.1", 2));
    assert_eq!(update.refusal(), Some(Refusal::Halted), "{update:?}");
}

/// Proves: a halt of a service that is installed stays: an update and an install of another
/// version are both refused `halted`, whatever the verb.
///
/// Catches: a halt of a running service that any install order lifts.
#[test]
fn a_halt_of_an_installed_service_stays_for_every_order() {
    let farm = NativeHost::new("halt-absent-installed");
    let rig = farm.rig();
    for version in ["1.9.0", "2.0.0", "2.1.0"] {
        farm.publish("web", version);
    }
    let daemon = farm.daemon();
    assert_eq!(daemon.order(&farm.install("web", "1.9.0", 1)), succeeded());
    rig.launcher.exits("web", Step::Prepare, &[76]);
    assert!(halted(&daemon.order(&farm.update("web", "2.0.0", 2))));
    for order in [
        farm.install("web", "2.1.0", 3),
        farm.update("web", "2.1.0", 3),
    ] {
        let held = daemon.order(&order);
        assert_eq!(held.refusal(), Some(Refusal::Halted), "{held:?}");
    }
}

/// Proves: when the host's record of placed services cannot be read, so that it is not known
/// whether the service is being uninstalled, the install is refused and the halt stands; and when
/// the record's halt cannot be cleared (a crash at the record's write) the install is refused
/// `store_failed` and the halt stands for the next order.
///
/// Catches: an install run over a halt whose service state could not be read, and a halt record
/// half cleared.
#[test]
fn a_halt_that_cannot_be_judged_or_cleared_stands() {
    let farm = NativeHost::new("halt-absent-faults");
    let rig = farm.rig();
    let daemon = halted_install(&farm);
    drop(daemon);

    let fs = FaultFs::new(rig.scratch.path(), rig.placed_log.clone()).failing_read(1, false);
    let blind = farm.daemon_on(Arc::new(fs), Arc::new(LayoutPrune));
    let refused = blind.order(&farm.install("web", "2.0", 2));
    assert!(
        refused.refusal().is_some_and(|r| r != Refusal::Halted),
        "{refused:?}"
    );
    assert!(matches!(
        rig.record("web").expect("the record").state,
        State::Finished {
            verdict: Verdict::Halt,
            ..
        }
    ));

    let dies = farm
        .daemon()
        .with_stores(Arc::new(|_: &ServiceName, store: RecordStore| {
            store.with_crash_gate(Arc::new(|point: CrashPoint| {
                matches!(point, CrashPoint::AfterWrite(_))
            }))
        }));
    let refused = dies.order(&farm.install("web", "2.0", 2));
    assert_eq!(refused.refusal(), Some(Refusal::StoreFailed), "{refused:?}");
}

/// Proves: the order's own package and try decide a resend, nothing else: the same package at
/// another try runs, and another package at the same try is not answered from the record (the
/// halt is lifted, then the try rules refuse the try the record already holds for the other
/// package, `try_conflict`); a `force_rollback` of the absent service stays refused `halted`.
///
/// Catches: a resend test that reads only the package or only the try, and a halt lifted for a
/// verb other than `install`.
#[test]
fn only_an_install_that_is_not_a_resend_runs_over_the_halt() {
    let farm = NativeHost::new("halt-absent-resend-keys");
    let daemon = halted_install(&farm);
    let rollback = daemon.order(&farm.force_rollback("web", "2.0", 2));
    assert_eq!(rollback.refusal(), Some(Refusal::Halted), "{rollback:?}");
    let other = daemon.order(&farm.install("web", "2.1", 1));
    assert_eq!(other.refusal(), Some(Refusal::TryConflict), "{other:?}");
    assert_eq!(daemon.order(&farm.install("web", "2.0", 2)), succeeded());
}

/// Proves: a halted install of a service with an uninstall under way (a tombstone in
/// `placed.json`) is not an absent service: the install is refused `halted` and the record still
/// holds the halt, which is not lifted for it.
///
/// Catches: a halt lifted over an uninstall that is half done.
#[test]
fn a_halt_of_a_tombstoned_service_stays() {
    let farm = NativeHost::new("halt-absent-tombstone");
    let rig = farm.rig();
    let daemon = halted_install(&farm);
    let placer = daemon.ctx().placer();
    let lock = placer.lock().expect("the lock");
    let mut placed = placer.store().load().expect("placed.json");
    placed
        .uninstalling
        .insert("web".to_owned(), hostkeeper_placed::Tombstone::default());
    placer.store().save(&placed, &lock).expect("saved");
    drop(lock);
    let held = daemon.order(&farm.install("web", "2.0", 2));
    assert_eq!(held.refusal(), Some(Refusal::Halted), "{held:?}");
    assert!(matches!(
        rig.record("web").expect("the record").state,
        State::Finished {
            verdict: Verdict::Halt,
            ..
        }
    ));
}
