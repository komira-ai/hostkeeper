//! The failure and boundary cases of hostkeeper's own package (docs/design/packages-native.md,
//! sections 10.4 and 10.6) that its main flow does not meet: an `activate` that repairs drift or
//! fails in the host's environment, a pending `activate` whose version or directory cannot be
//! read, a record that cannot be written, the service's lock held, and a host whose records
//! disagree with its packages directory.

mod support;
mod support_apt;
mod support_native;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::locks::Holder;
use hostkeeper_daemon::own_package::{
    OwnActivate, REASON_ACTIVATE_PENDING, REASON_RELEASE_UNREADABLE,
};
use hostkeeper_daemon::refusal::Refusal;
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_placed::InstalledEntry;
use hostkeeper_steps::record::{Activated, PendingActivate, State};
use hostkeeper_steps::{CrashPoint, Settled};

use hostkeeper_placed::testing::{FaultFs, Log as PlacedLog};
use support::selfupdate::{HK, awaiting, put_record, record_of};
use support::{alerting, crash_at, d, lines, name, update};
use support_apt::seen::{held, web};
use support_apt::{AptRig, SERVICE};
use support_native::{NativeRig, V1, V2, install_hk, pv};

/// A host that runs [`V2`] after its hand-over from [`V1`], adopted, whose new binary confirmed
/// itself and has its package still to activate.
fn confirmed(name: &str) -> NativeRig {
    let nr = NativeRig::new(name);
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    drop(nr.start(V1));
    let pin = nr.publish(V2);
    assert_eq!(
        nr.daemon_at(V1).order(&install_hk(V2, &pin, 1)),
        awaiting(false)
    );
    assert_eq!(
        nr.daemon_at(V2).settle_self_update().unwrap(),
        Settled::Confirmed
    );
    assert!(nr.record().unwrap().progress.activate.is_some());
    nr
}

/// hostkeeper's step record of an update to `version` whose `activate` waits.
fn pending_record(nr: &NativeRig, version: &str) {
    let mut rec = record_of(
        HK,
        State::Done {
            code: Some(0),
            last_line: None,
        },
    );
    rec.target_version = version.to_owned();
    rec.step = Step::Cleanup;
    rec.progress.activate = Some(PendingActivate {
        version: version.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
    });
    put_record(&nr.rig, HK, &rec);
}

/// Proves an `activate` that finds the package at a version hostkeeper did not set installs the
/// one it should over it, and the update is `Succeeded` with the reason `drift_repaired`.
#[test]
fn an_activate_that_finds_drift_repairs_it_and_says_so() {
    let nr = confirmed("own-drift");
    nr.host.set_installed(HK, "0.9.0-1", 'i', true);
    let daemon = nr.daemon_at(V2);
    assert_eq!(
        daemon.activate_own_package(),
        Some(OwnActivate::Ended(Activated::Succeeded {
            reason: Some("drift_repaired".to_owned())
        }))
    );
    assert_eq!(nr.host.installed(HK), held(&pv(V2)));
    assert_eq!(
        nr.record().unwrap().state,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: Some("drift_repaired".to_owned()),
        }
    );
}

/// Proves an `activate` that fails in the host's environment (dpkg interrupted) ends the update
/// `Failed` after the commit, of class environment, with its reason.
#[test]
fn an_activate_that_fails_in_the_environment_is_failed_of_that_class() {
    let nr = confirmed("own-interrupted");
    nr.host.edit(|s| s.faults.interrupted = true);
    let daemon = nr.daemon_at(V2);
    assert_eq!(
        daemon.activate_own_package(),
        Some(OwnActivate::Ended(Activated::Failed {
            class: Class::Environment,
            reason: "dpkg_interrupted".to_owned()
        }))
    );
    assert_eq!(
        nr.record().unwrap().state,
        State::Finished {
            verdict: Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Environment,
            },
            reason: Some("dpkg_interrupted".to_owned()),
        }
    );
}

/// Proves a pending `activate` whose version cannot be read ends instead of being tried for
/// ever: a version that is not a version, a version directory gone (its `target.json` cannot
/// be read), a `target.json` that does not read, and one of an installer this daemon has no
/// steps of each end the update `Failed` after the commit, of class release,
/// `release_unreadable`, with an alert naming the version, no tool run, and the record no longer
/// waiting, so that the next order of hostkeeper runs.
#[test]
fn a_pending_activate_whose_version_cannot_be_read_ends_failed() {
    let cases = [
        ("not-a-version", "1.1.0 x"),
        ("gone", V2),
        ("bad-target", V2),
        ("no-steps", V2),
    ];
    for (case, version) in cases {
        let nr = NativeRig::new(&format!("own-unreadable-{case}"));
        pending_record(&nr, version);
        let target = nr.version_dir(V2).join("target.json");
        match case {
            "bad-target" => {
                fs::create_dir_all(nr.version_dir(V2)).unwrap();
                fs::write(&target, "{").unwrap();
            }
            "no-steps" => {
                fs::create_dir_all(nr.version_dir(V2)).unwrap();
                fs::write(&target, r#"{"installer":"oci-image"}"#).unwrap();
            }
            _ => {}
        }
        let (daemon, log) = alerting(nr.daemon_at(V2));
        assert_eq!(
            daemon.activate_own_package(),
            Some(OwnActivate::Ended(Activated::Failed {
                class: Class::Release,
                reason: REASON_RELEASE_UNREADABLE.to_owned()
            })),
            "{case}"
        );
        let rec = nr.record().unwrap();
        assert_eq!(rec.progress.activate, None, "{case}");
        assert_eq!(
            rec.state,
            State::Finished {
                verdict: Verdict::Failed {
                    phase: Phase::AfterCommit,
                    class: Class::Release,
                },
                reason: Some(REASON_RELEASE_UNREADABLE.to_owned()),
            },
            "{case}"
        );
        assert!(
            lines(&log)
                .iter()
                .any(|line| line.contains("alert=own_package") & line.contains(version)),
            "{case}: {:?}",
            lines(&log)
        );
        assert!(nr.host.calls().is_empty(), "{case}: no tool ran");
    }
}

/// Proves an `activate` whose end cannot be written leaves the record waiting and says so: the
/// alert names it, and the order for the version is answered `Deferred` `activate_pending`, since
/// the record holds no verdict to answer with.
#[test]
fn an_activate_whose_end_cannot_be_written_leaves_the_record_waiting() {
    let nr = confirmed("own-unwritten");
    let seam = crash_at(
        Some(CrashPoint::BeforeWrite(1)),
        Arc::new(AtomicU64::new(0)),
    );
    let (daemon, log) = alerting(nr.daemon_at(V2).with_stores(seam));
    let pin = nr.publish(V2);
    assert_eq!(
        daemon.order(&install_hk(V2, &pin, 2)),
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(60),
            },
            reason: Some(REASON_ACTIVATE_PENDING.to_owned()),
            resent: false,
        }
    );
    assert!(nr.record().unwrap().progress.activate.is_some());
    assert!(
        lines(&log)
            .iter()
            .any(|line| line.contains("the end of the activate was not recorded")),
        "{:?}",
        lines(&log)
    );
}

/// Proves the service's lock keeps an order and the retry apart: while an order holds it the
/// retry runs nothing and defers `busy`; while the retry holds it an order is `busy`, naming it.
#[test]
fn the_retry_and_an_order_never_run_at_once() {
    let nr = confirmed("own-locked");
    let daemon = nr.daemon_at(V2);
    let held = daemon
        .ctx()
        .locks
        .take_for(&name(HK), Holder::ClearHalt)
        .unwrap();
    let ended = daemon.activate_own_package();
    assert!(
        matches!(ended, Some(OwnActivate::Deferred { ref reason, .. }) if reason == "busy"),
        "{ended:?}"
    );
    drop(held);
    assert_eq!(nr.host.installed(HK), held_v1());

    let retry = daemon
        .ctx()
        .locks
        .take_for(&name(HK), Holder::OwnActivate)
        .unwrap();
    let reply = daemon.order(&update(HK, V2, &d(), 2));
    match reply {
        Reply::Refused {
            refusal: Refusal::Busy,
            detail,
        } => assert!(
            detail.contains("the activate of its own package"),
            "{detail}"
        ),
        other => panic!("not busy: {other:?}"),
    }
    drop(retry);
}

fn held_v1() -> Option<support_apt::Inst> {
    held(&pv(V1))
}

/// Proves a settlement that cannot read the confirmed version's `target.json` is an error, with
/// the record still waiting for the restart: the start alerts and the next start settles again.
#[test]
fn a_settlement_that_cannot_read_the_version_waits() {
    let nr = confirmed("own-settle-unreadable");
    let mut rec = nr.record().unwrap();
    rec.progress.activate = None;
    rec.progress.awaiting_restart = Some(hostkeeper_steps::record::AwaitingRestart {
        version: V2.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
        next: Some(V2.to_owned()),
        starts: Some(1),
    });
    put_record(&nr.rig, HK, &rec);
    let target = nr.version_dir(V2).join("target.json");
    support_apt::open_up(&nr.version_dir(V2));
    fs::remove_file(&target).unwrap();
    fs::write(&target, "{").unwrap();
    assert!(nr.daemon_at(V2).settle_self_update().is_err());
    assert!(nr.record().unwrap().progress.awaiting_restart.is_some());
}

/// Proves a service whose `placed.json` names apt while its packages directory holds no version
/// the installer staged (a version directory with no `target.json`) is not ordered through: an
/// `update` of it, whose `current` holds no `target.json`, is refused `release_unreadable`, and an
/// `install` of it over a version directory with no seal is refused `artifact_changed`, with
/// nothing fetched, installed or purged and the record as it was.
#[test]
fn a_service_recorded_with_apt_and_no_native_version_is_not_switched() {
    for current in [true, false] {
        let apt = AptRig::new(&format!("own-damaged-{current}"));
        let pin = apt.publish(support_apt::package("2.0.0"));
        let entry = support_apt::entry("2.0.0-1", &pin);
        let dir = match current {
            true => apt.rig.running(SERVICE, "1.0.0"),
            false => apt.rig.stage(SERVICE, "2.0.0"),
        };
        fs::remove_file(dir.join("target.json")).expect("a version the installer did not stage");
        let mut record = apt.rig.placed_record();
        record
            .installed
            .insert(SERVICE.to_owned(), InstalledEntry::new("apt"));
        apt.rig.save_placed(&record);
        let daemon = apt.daemon();
        let order = match current {
            true => update(SERVICE, "2.0.0", &pin, 1),
            false => support::install(SERVICE, "2.0.0", &pin, 1),
        };
        let reply = daemon.order(&support_apt::with_entry(order, &entry));
        let refused = match current {
            true => Refusal::ReleaseUnreadable,
            false => Refusal::ArtifactChanged,
        };
        assert_eq!(
            reply.refusal(),
            Some(refused),
            "current={current}: {reply:?}"
        );
        assert!(
            apt.host.calls().is_empty(),
            "current={current}: nothing fetched, installed or purged"
        );
        assert_eq!(
            apt.rig.placed_record().installer_of(web().as_str()),
            Some("apt")
        );
    }
}

/// Proves where the new binary of each kind of version is: `tree/usr/bin/hostkeeper` of a `.deb`,
/// `tree/usr/local/bin/hostkeeper` of a `.pkg`.
#[test]
fn the_new_binary_is_the_package_s_own_path_in_the_version_s_tree() {
    use hostkeeper_daemon::selfupdate::release_binary;
    use hostkeeper_proto::hostkeeper::release::v1::Target;
    let of = |installer: &str| Target {
        installer: installer.to_owned(),
        ..Target::default()
    };
    assert_eq!(release_binary(&of("apt")), "tree/usr/bin/hostkeeper");
    assert_eq!(
        release_binary(&of("macos-pkg")),
        "tree/usr/local/bin/hostkeeper"
    );
}

/// Proves an order of hostkeeper on a host whose `placed.json` cannot be read is refused
/// `store_failed` before anything is staged: the rollback table cannot be judged without it.
#[test]
fn an_order_of_hostkeeper_with_placed_json_unreadable_is_refused_before_staging() {
    let nr = NativeRig::new("own-placed-unreadable");
    let placed = nr.rig.scratch.path().join("placed/placed.json");
    fs::write(&placed, "{").unwrap();
    let pin = nr.publish(V2);
    let reply = nr.daemon_at(V1).order(&install_hk(V2, &pin, 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(nr.host.calls().is_empty());
    assert!(!nr.version_dir(V2).exists());
}

/// A filesystem over the host's root whose `n`-th write and every later one fail.
fn failing_at(nr: &NativeRig, n: u64) -> Arc<FaultFs> {
    Arc::new(FaultFs::failing_at(&nr.root(), n, PlacedLog::new()))
}

/// Proves a failure of the host's files during the `activate` is tried again, not ended, with
/// nothing recorded as done: the switch of `current` after the package was installed (the 9th
/// write of an `activate`: the record's two saves come first, four writes each) defers
/// `io_error`, the record still waiting.
#[test]
fn a_failure_of_the_host_s_files_in_the_activate_is_tried_again() {
    let nr = confirmed("own-io-current");
    let ended = nr
        .daemon_with_fs(V2, failing_at(&nr, 9))
        .activate_own_package();
    assert!(
        matches!(ended, Some(OwnActivate::Deferred { ref reason, .. }) if reason == "io_error"),
        "{ended:?}"
    );
    assert!(nr.record().unwrap().progress.activate.is_some());
    assert_eq!(nr.linked("current"), None);
}

/// Proves a failed `activate` whose record cannot be written whole again is still ended
/// `Failed`, with an alert that the drift's record was not written: the 5th write, the second
/// save, is the one that fails.
#[test]
fn a_failed_activate_whose_record_cannot_be_written_again_says_so() {
    let nr = confirmed("own-unpend-fails");
    nr.host.edit(|s| s.faults.noop = true);
    let (daemon, log) = alerting(nr.daemon_with_fs(V2, failing_at(&nr, 5)));
    let ended = daemon.activate_own_package();
    assert!(
        matches!(ended, Some(OwnActivate::Ended(Activated::Failed { ref reason, .. })) if reason == "dpkg_error"),
        "{ended:?}"
    );
    assert!(
        lines(&log)
            .iter()
            .any(|line| line.contains("the record of the failed activate")),
        "{:?}",
        lines(&log)
    );
    assert_eq!(
        nr.record().unwrap().state,
        support_native::activate_failed("dpkg_error")
    );
}

/// Proves an adoption the host refuses is no adoption: an installer lock that cannot be taken,
/// and a record that cannot be written, each make the start's adoption an error that names it,
/// and nothing is recorded.
#[test]
fn an_adoption_the_host_refuses_records_nothing() {
    let nr = NativeRig::new("own-adopt-lock");
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    fs::create_dir_all(nr.rig.scratch.path().join("installer.lock")).unwrap();
    let adopted = nr.start(V1).boot_report().adopted.clone();
    assert!(
        matches!(adopted, Some(Err(ref e)) if e.contains("the installer lock")),
        "{adopted:?}"
    );
    assert_eq!(nr.installed(), None);

    let nr = NativeRig::new("own-adopt-write");
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    fs::create_dir_all(&nr.rig.state).unwrap();
    let started = hostkeeper_daemon::start::startup(
        &nr.rig.settings(),
        nr.ports_with_fs(failing_at(&nr, 1)),
        support::selfupdate::own(V1),
    )
    .expect("the start goes on");
    assert!(
        matches!(started.boot_report().adopted, Some(Err(_))),
        "{:?}",
        started.boot_report().adopted
    );
    assert_eq!(nr.installed(), None);
}
