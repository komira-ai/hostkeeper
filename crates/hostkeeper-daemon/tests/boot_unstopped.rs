//! A step the dead-man cannot stop during the start pass (design section 6.3): the daemon keeps
//! serving, keeps the service locked and its record `running`, and a later run stops the unit
//! first.

mod support;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::thread;

use hostkeeper_daemon::admin::service_record;
use hostkeeper_daemon::boot::Outcome;
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::testing::{FakeClock, Log};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::Step;
use hostkeeper_steps::CrashPoint;
use hostkeeper_steps::record::State;
use support::{Rig, crash_at, d, name, ok, update};

/// Catches a daemon that exits, lets go of the service, or rewrites the record when a step of
/// the start pass cannot be stopped: `alpha`'s resumed `prepare` runs past its bound and the
/// dead-man's stop fails. The start pass ends with `alpha` interrupted `unstopped`; the status
/// still answers, for `beta` too, and reports `alpha` under way with the reason
/// `unstoppable_unit`; `alpha`'s record is still `running`; its lock is held (a second take
/// fails); another order of it is `busy`, the same one is answered as running, and `beta`'s runs.
/// Once stops work again, the next start stops the unit first and finishes the update.
#[test]
fn unstopped_keeps_serving_holds_the_lock_and_leaves_the_step_running() {
    let rig = Rig::new("boot-unstopped");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    rig.add("beta", "2.0.0");
    let (first, _) = rig.booted();
    drop(first);
    // The daemon dies right after the first record of `alpha`'s update: `prepare` is pending.
    let dying = rig.daemon().with_stores(crash_at(
        Some(CrashPoint::AfterWrite(1)),
        Arc::new(AtomicU64::new(0)),
    ));
    let reply = dying.order(&update("alpha", "2.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert_eq!(rig.record("alpha").expect("a record").state, State::Pending);
    drop(dying);

    rig.launcher.fail_stops("alpha");
    let stops = rig.launcher.stops_seen();
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let clock = FakeClock::frozen();
    let mut ports = rig.ports();
    ports.clock = clock.clone();
    ports.deadman_poll_secs = Some(1);
    let daemon = Arc::new(Daemon::new(
        support::config(),
        rig.dirs(),
        ports,
        support::OWN,
    ));
    let pass = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.boot_pass().expect("the start pass"))
    };
    gate.entered();
    // Past the bound of `prepare` (600 seconds): the dead-man tries to stop the step, and fails.
    clock.advance(601);
    assert_eq!(stops.recv().expect("a stop"), "updater-alpha-prepare-1");
    gate.release();
    let report = pass.join().expect("the start pass");
    let Outcome::Interrupted { refusal, detail } = &report.services["alpha"].outcome else {
        panic!("{:?}", report.services["alpha"])
    };
    assert_eq!(*refusal, Refusal::Unstopped);
    assert!(detail.starts_with("service=alpha: "), "{detail}");

    let alpha = name("alpha");
    assert!(daemon.ctx().locks.holder(&alpha).expect("held").stuck);
    assert!(matches!(
        rig.record("alpha").expect("a record").state,
        State::Running { .. }
    ));
    let status = daemon.status().expect("the status");
    let names: Vec<String> = status
        .services
        .iter()
        .map(|s| s.service.to_string())
        .collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert!(status.services[0].stuck & status.services[0].step_running());
    let record = service_record(&status.services[0]);
    assert_eq!(
        record.current.expect("under way").reason,
        "unstoppable_unit"
    );
    assert!(service_record(&status.services[1]).current.is_none());
    let package = Package {
        verb: HookedVerb::Update,
        version: hostkeeper_config::names::Version::new("2.0.0").expect("version"),
        digest: PackageDigest::new(&d()).expect("digest"),
    };
    assert!(daemon.ctx().locks.take(&alpha, package).is_err());
    let other = daemon.order(&update("alpha", "3.0.0", &d(), 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy));
    assert_eq!(
        daemon.order(&update("alpha", "2.0.0", &d(), 1)),
        Reply::Running
    );
    assert_eq!(
        daemon.order(&update("beta", "2.0.0", &d(), 1)),
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: false
        }
    );
    drop(daemon);

    rig.launcher.allow_stops("alpha");
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let (_, report) = rig.booted();
    assert_eq!(report.services["alpha"].verdict(), Some(ok()));
    let calls = log.lock().expect("the log").clone();
    assert_eq!(
        calls[..2],
        [
            "stop updater-alpha-prepare-1",
            "run updater-alpha-prepare-1"
        ]
    );
}
