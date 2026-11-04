//! The client certificate: read at every heartbeat, and no call without one.

use hostkeeper_link::testing::identity;
use hostkeeper_link::{BeatOutcome, CertState, ClientIdentity};

use crate::{Rig, recorded};

/// Catches a daemon with no certificate that logs at every period, calls with an empty pair,
/// or keeps waiting once the pair is there: N periods give one line and no call, and the first
/// call is at the period after the pair appears. A change of what it waits for, or a pair that
/// goes away again, is said once more.
#[test]
fn no_certificate_says_once_that_it_is_not_enrolled_and_makes_no_call() {
    let rig = Rig::new("certs-none");
    rig.install(vec![recorded("x", "1.0")]);
    let current = rig.state().join("pki").join("current");
    rig.world.certs.set(CertState::NotEnrolled {
        waiting_for: current.clone(),
    });
    let mut heartbeater = rig.heartbeater();
    for _ in 0..5 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    }
    assert!(rig.world.link.calls().is_empty());
    let waiting = format!("not enrolled: waiting for {}", current.display());
    assert_eq!(rig.world.log.lines(), [waiting.clone()]);

    rig.world.certs.set(CertState::Enrolled(identity("a")));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(rig.world.link.calls()[0].0, identity("a"));

    let other = rig.state().join("pki").join("gen-2");
    rig.world.certs.set(CertState::NotEnrolled {
        waiting_for: current.clone(),
    });
    heartbeater.beat_once();
    rig.world.certs.set(CertState::NotEnrolled {
        waiting_for: other.clone(),
    });
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.link.calls().len(), 1);
    assert_eq!(
        rig.world.log.lines(),
        [
            waiting.clone(),
            waiting,
            format!("not enrolled: waiting for {}", other.display()),
        ]
    );
}

/// Catches a loop that keeps the pair it read first: each call is made with the pair the source
/// gives at that heartbeat.
#[test]
fn each_call_uses_the_pair_the_source_gives_at_that_heartbeat() {
    let rig = Rig::new("certs-pair");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    rig.world.certs.set(CertState::Enrolled(identity("b")));
    heartbeater.beat_once();
    let used: Vec<ClientIdentity> = rig
        .world
        .link
        .calls()
        .into_iter()
        .map(|(pair, _)| pair)
        .collect();
    assert_eq!(used, [identity("a"), identity("b")]);
}

/// Catches a private key that shows in a log line or a panic message through `Debug`.
#[test]
fn a_client_identity_never_shows_its_key() {
    let pair = ClientIdentity::new(b"-----CERT-----".to_vec(), b"secret key bytes".to_vec());
    assert_eq!(
        format!("{pair:?}"),
        "ClientIdentity { certificate: 14 bytes, key: redacted }"
    );
    assert_eq!(pair.certificate(), b"-----CERT-----");
    assert_eq!(pair.key(), b"secret key bytes");
}
