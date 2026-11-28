//! What the host waits for while its update's `verify` defers, reported in every heartbeat's
//! service facts.

use hostkeeper_link::testing::START_UNIX;
use hostkeeper_link::{Reason, RecordedDeferrals, RecordedService, VerifyDeferrals};

use crate::{Rig, recorded, service, version};

/// The deferrals the beat `at` reports for `name`.
fn reported(rig: &Rig, at: usize, name: &str) -> Option<VerifyDeferrals> {
    rig.world.link.beats()[at]
        .services
        .iter()
        .find(|facts| facts.name == service(name))
        .expect("the service is listed")
        .deferring
        .clone()
}

/// Proves a service whose record says its `verify` is deferring is reported with the run, the
/// count, the last reason and detail and the time of the first deferral, by the heartbeat's own
/// clock, and a service beside it with none.
///
/// Catches: what a host waits for left out of the facts, dated from the wrong clock, or reported
/// for every service once one defers.
#[test]
fn a_deferring_verify_is_reported_with_since_when() {
    let rig = Rig::new("deferring-reported");
    let waiting = RecordedService {
        updating: Some(version("0.3.212")),
        deferring: Some(RecordedDeferrals {
            attempt: 3,
            count: 2,
            reason: Some(Reason::new("provider_busy").expect("a reason")),
            detail: Some("read of /Volumes/build: Operation not permitted".to_owned()),
            age_secs: Some(240),
        }),
        ..recorded("tierstore", "0.3.211")
    };
    rig.install(vec![waiting, recorded("web", "1.0")]);
    rig.heartbeater().beat_once();
    assert_eq!(
        reported(&rig, 0, "tierstore"),
        Some(VerifyDeferrals {
            attempt: 3,
            count: 2,
            reason: Some(Reason::new("provider_busy").expect("a reason")),
            detail: Some("read of /Volumes/build: Operation not permitted".to_owned()),
            since_unix: Some(START_UNIX - 240),
        })
    );
    assert_eq!(reported(&rig, 0, "web"), None);
}
