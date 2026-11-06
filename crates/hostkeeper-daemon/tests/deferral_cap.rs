//! The deferral's retry time through the daemon's heartbeat loop, on an injected unix clock: the
//! one decision of the daemon that reads the wall clock (`hostkeeper_daemon::unix_clock`) is
//! bounded, whatever the deferral file says.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::links::Links;
use hostkeeper_link::deferral::{DEFERRAL_FILE, MAX_RETRY_SECS};
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, DeferredReport, Feature, Order, OrderId,
    PackageDigest, Reason, Reply, StopFlag, Target, Verb,
};
use serde_json::Value;
use support::{Rig, d, name};

/// The unix second the scenario starts at.
const NOW: u64 = 2_000_000_000;

/// A retry time ten days ahead: far past the cap.
const FAR: u64 = NOW + 10 * 24 * 3600;

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// The coordinator's update of `web` to 2.0.0.
fn update() -> Order {
    Order {
        id: OrderId::new("update-web-2.0.0").expect("an id"),
        order_try: 1,
        verb: Verb::Update(Target {
            service: name("web"),
            version: version("2.0.0"),
            digest: PackageDigest::new(&d()).expect("a digest"),
            index_digest: None,
        }),
    }
}

/// A reply that advertises deferrals, with `order`.
fn deferring(order: Option<Order>) -> Reply {
    Reply {
        order,
        features: vec![Feature::DeferredV1],
        ..Reply::default()
    }
}

/// Catches a deferral read from the file as far as it says, which a wrong wall clock (or a bad
/// hint) could hold a host with for days: a retry time ten days ahead is reported as
/// `MAX_RETRY_SECS` from now, the capped time is written back to the file, the update it is
/// about is declined until then, and runs once the clock has passed it.
#[test]
fn a_retry_time_far_ahead_is_capped_at_an_hour_from_now() {
    let rig = Rig::new("deferral-cap");
    rig.running("web", "1.0.0");
    rig.stage("web", "2.0.0");
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the service's state directory");
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("its mode");
    let file = dir.join(DEFERRAL_FILE);
    fs::write(
        &file,
        format!(r#"{{"target":"2.0.0","reason":"drain_bound","retry_after_unix":{FAR}}}"#),
    )
    .expect("a deferral");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("its mode");

    let daemon = Arc::new(rig.daemon());
    let link = Arc::new(FakeLink::default());
    let clock = Arc::new(FakeClock::new(NOW, 7));
    let dispatch = Arc::new(ManualDispatch::default());
    let log = Arc::new(RecordingLog::default());
    let links = Links {
        enroller: Box::new(|| Err("no enrollment here".to_owned())),
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: clock.clone(),
        log: log.clone(),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: dispatch.clone(),
        stop: StopFlag::default(),
    };
    let mut heartbeater = links.heartbeater(&daemon);

    // The first reply advertises deferrals; the second carries the update.
    link.answer(Ok(deferring(None)));
    link.answer(Ok(deferring(Some(update()))));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let beats = link.beats();
    assert_eq!(
        beats[1].services[0].deferred,
        Some(DeferredReport {
            target: version("2.0.0"),
            reason: Reason::new("drain_bound").expect("a token"),
            retry_after_secs: MAX_RETRY_SECS,
        })
    );
    assert_eq!(dispatch.pending(), 0, "the deferred update is declined");
    assert_eq!(
        log.lines(),
        [format!(
            "order update-web-2.0.0 try 1 of web is not run: deferred until unix second {}",
            NOW + MAX_RETRY_SECS
        )]
    );

    // The next heartbeat writes the capped time back.
    link.answer(Ok(deferring(None)));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let written: Value =
        serde_json::from_slice(&fs::read(&file).expect("the deferral file")).expect("JSON");
    assert_eq!(written["retry_after_unix"], NOW + MAX_RETRY_SECS);

    // Past the capped time, the same update runs.
    clock.set_unix(NOW + MAX_RETRY_SECS);
    link.answer(Ok(deferring(Some(update()))));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(
        dispatch.pending(),
        1,
        "the update is started once the cap has passed"
    );
}
