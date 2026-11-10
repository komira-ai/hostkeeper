//! A force that meets a fault in its own files or in the version it resumes: what it answers, and
//! that it changes nothing of the host before it does.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::{CrashPoint, Finished, Layout};
use support::force::{UNIT, WEB, failed, link_of};
use support::{crash_at, name};
use support_host::NativeHost;

/// A daemon of `rig` whose step records fail to write from the `nth` write on.
fn failing_from(rig: &NativeHost, nth: u64) -> Daemon {
    let seam = crash_at(
        Some(CrashPoint::BeforeWrite(nth)),
        Arc::new(AtomicU64::new(0)),
    );
    rig.daemon().with_stores(seam)
}

/// `web` at 1.0.0 whose update of 1.0.0 lost its `prepare` (the launcher lost the step, so the
/// record holds it running), with 3.0.0 beside it.
fn lost_update(label: &str) -> NativeHost {
    let rig = NativeHost::new(label);
    rig.add(WEB, "1.0.0");
    rig.pin(WEB, "3.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose(WEB, Step::Prepare);
    let lost = daemon.order(&rig.update(WEB, "1.0.0", 1));
    assert_eq!(
        lost.refusal(),
        Some(Refusal::LauncherFailed),
        "{label}: {lost:?}"
    );
    rig
}

/// A force over an update that was lost mid-step writes its records twice before it switches: it
/// flags the update, then closes it `forced_over`. When either write fails the force is refused
/// `store_failed` and `current` is not moved.
#[test]
fn a_force_over_a_lost_update_that_cannot_write_its_records_is_refused() {
    for nth in [1, 2] {
        let rig = lost_update(&format!("force-faults-write-{nth}"));
        let reply = failing_from(&rig, nth).order(&rig.force(WEB, "3.0.0", 2, 0));
        assert_eq!(
            reply.refusal(),
            Some(Refusal::StoreFailed),
            "write {nth}: {reply:?}"
        );
        assert_eq!(
            link_of(&rig, WEB, "current").as_deref(),
            Some("0.0.0"),
            "write {nth}"
        );
    }
}

/// A force that lifts a standing halt clears the halt record: when that write fails the force is
/// refused `store_failed` and `current` is not moved.
#[test]
fn a_force_whose_halt_lift_cannot_clear_the_halt_record_is_refused() {
    let rig = NativeHost::new("force-faults-halt");
    rig.add(WEB, "1.0.0");
    rig.pin(WEB, "3.0.0");
    rig.launcher.exits(WEB, Step::Prepare, &[76]);
    let (daemon, _) = rig.booted();
    let halted = daemon.order(&rig.update(WEB, "1.0.0", 1));
    assert!(
        matches!(
            halted,
            Reply::Finished {
                verdict: Verdict::Halt,
                ..
            }
        ),
        "{halted:?}"
    );
    daemon
        .ctx()
        .halts
        .raise(&name(WEB), 2)
        .expect("hold the service at 2");
    let reply = failing_from(&rig, 1).order(&rig.force(WEB, "3.0.0", 2, 3));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("0.0.0"));
}

/// The unfinished force of web to 2.0.0 that a crash after the first write of its record leaves,
/// with 1.0.0 forced before it.
fn crashed_force(label: &str) -> NativeHost {
    let rig = NativeHost::new(label);
    rig.manifest(WEB, "1.0.0", &support_host::runs(WEB, "1.0.0", &[UNIT]));
    rig.manifest(WEB, "2.0.0", &support_host::runs(WEB, "2.0.0", &[UNIT]));
    rig.forced(&rig.daemon(), "1.0.0", 1);
    let seam = crash_at(Some(CrashPoint::AfterWrite(1)), Arc::new(AtomicU64::new(0)));
    let first = rig
        .daemon()
        .with_stores(seam)
        .order(&rig.force(WEB, "2.0.0", 2, 0));
    assert_eq!(first.refusal(), Some(Refusal::StoreFailed), "{first:?}");
    rig
}

fn release_unreadable() -> Finished {
    let Reply::Finished {
        verdict, reason, ..
    } = failed(
        Phase::BeforeCommit,
        Class::Environment,
        "release_unreadable",
    )
    else {
        unreachable!("failed() answers a finished reply")
    };
    Finished { verdict, reason }
}

/// The start pass resumes a force from its record alone, with no entry to stage from: when the
/// version it names is no longer whole on the host (its seal is gone) the force ends `Failed`
/// before the commit, `release_unreadable`, and `current` is not moved.
#[test]
fn a_resumed_force_whose_version_is_gone_fails_release_unreadable() {
    let rig = crashed_force("force-faults-gone");
    let seal = Layout::package(&rig.packages.join(WEB)).seal_path("2.0.0");
    fs::remove_file(seal).expect("the seal of 2.0.0");
    let (_, report) = rig.booted();
    assert_eq!(report.services[WEB].verdict(), Some(release_unreadable()));
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
}

/// The start pass of a daemon older than the version's `min_hostkeeper` cannot read the
/// manifest of the force it resumes: the force ends `Failed` before the commit, as the
/// environment's fault (`daemon_too_old`), and `current` is not moved.
#[test]
fn a_resumed_force_the_daemon_is_too_old_for_fails_daemon_too_old() {
    let rig = crashed_force("force-faults-old");
    let old = HostkeeperVersion {
        major: 0,
        minor: 0,
        patch: 9,
    };
    let daemon = Daemon::new(support::config(), rig.dirs(), rig.ports(), old);
    let report = daemon.boot_pass().expect("the start pass");
    assert_eq!(
        report.services[WEB].verdict(),
        Some(Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Environment,
            },
            reason: Some("daemon_too_old".to_owned()),
        })
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
}
