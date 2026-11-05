//! What the heartbeat path asks of enrollment: who an enrolled host's certificate names, a
//! duplicate host ordered nothing, a denied host refused, a retiring host's slot freed when its
//! update is final, and a broken store file failing closed.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::{CoordinatorRefusal, RenewRequest};
use hostkeeper_wire::wire::{HEARTBEAT_PATH, ServiceReply};
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use super::support::{
    HostKey, PATIENCE, Rig, coordinator_over, enrolled, host_id, identity, watched,
};
use crate::enrollment::{Admission, RpcError, os_random};
use crate::fleet::{Fleet, ORDER_AGE};
use crate::http::{Peer, Surface, handle};
use crate::identity::{Identity, IdentityError};
use crate::service::Coordinator;
use crate::testutil::{Log, beat, id, role, secs, updating, version};

/// The order of the legacy service to 2.0.
fn order() -> ServiceReply {
    ServiceReply::Update {
        service: None,
        version: version("2.0"),
    }
}

/// A heartbeat of `body` through the heartbeat route, from the holder of `peer`.
async fn beat_json(coordinator: &Coordinator, peer: &Peer, body: &str) -> (StatusCode, Value) {
    let request = Request::post(HEARTBEAT_PATH)
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let response = handle(coordinator, Surface::Heartbeat, peer, request, PATIENCE).await;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

const UPDATING: &str =
    r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","updating":"2.0"}"#;
const DONE: &str = r#"{"platform":"linux-x86_64","version":"2.0","health":"healthy"}"#;

fn denied() -> (StatusCode, Value) {
    (StatusCode::FORBIDDEN, json!({"error": "denied"}))
}

fn peer_of(der: &[u8]) -> Peer {
    Some(Arc::from(der))
}

/// Catches a host two live daemons speak for being ordered, the duplicate going unalerted,
/// and the mark lost at a restart.
#[tokio::test]
async fn two_live_daemons_on_one_id_are_duplicate() {
    let enrollment = Rig::new("hb-duplicate");
    let (one, two) = (HostKey::new(), HostKey::new());
    let first = enrolled(&enrollment.enrollment, &one, "machine-1", "s1");
    let second = enrolled(&enrollment.enrollment, &two, "machine-2", "s2");
    let mut rig = coordinator_over("hb-duplicate-coordinator", &enrollment).await;
    let s1 = identity(&rig.coordinator, &first);
    let s2 = identity(&rig.coordinator, &second);
    let heard = |coordinator: &Coordinator, who: &Identity, machine: Option<&str>| {
        coordinator
            .heartbeat_from(who.clone(), beat("1.0"), machine)
            .expect("a heartbeat")
            .reply
    };
    assert_eq!(heard(&rig.coordinator, &s1, Some("machine-1")), order());

    let log = Log::default();
    let guard = log.install();
    let cloned = heard(&rig.coordinator, &s1, Some("machine-9"));
    drop(guard);
    assert_eq!(
        cloned,
        ServiceReply::Nothing,
        "a duplicate is ordered nothing"
    );
    assert!(
        log.text().contains("two live daemons report one host id"),
        "{}",
        log.text()
    );
    assert!(rig.coordinator.host_is_duplicate(&s1.host_id));

    rig.coordinator =
        crate::testutil::Rig::start(&rig.dir, &rig.clock).with_enrollment(enrollment.restart());
    rig.at(3_000);
    assert_eq!(heard(&rig.coordinator, &s1, None), ServiceReply::Nothing);
    assert_eq!(
        heard(&rig.coordinator, &s2, None),
        order(),
        "the others are ordered"
    );
}

/// Catches a deny that frees a retiring host's update slot at once, a final report that never
/// frees it, and a retired host's later heartbeat read before it is refused.
#[tokio::test]
async fn a_retired_host_mid_update_frees_its_slot_when_final() {
    let enrollment = Rig::new("hb-retiring");
    let (one, two) = (HostKey::new(), HostKey::new());
    let first = enrolled(&enrollment.enrollment, &one, "machine-1", "s1");
    let second = enrolled(&enrollment.enrollment, &two, "machine-2", "s2");
    let rig = coordinator_over("hb-retiring-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let (s1, s2) = (identity(c, &first), identity(c, &second));
    assert_eq!(
        c.heartbeat(s1.clone(), beat("1.0"))
            .expect("a heartbeat")
            .reply,
        order()
    );
    c.heartbeat(s1.clone(), updating(beat("1.0"), "2.0"))
        .expect("a heartbeat");

    enrollment
        .enrollment
        .store()
        .deny(&host_id("s1"))
        .expect("denied");
    let s2_told = || {
        c.heartbeat(s2.clone(), beat("1.0"))
            .expect("a heartbeat")
            .reply
    };
    assert_eq!(
        s2_told(),
        ServiceReply::Nothing,
        "a deny alone frees no slot"
    );
    let peer = peer_of(&first);
    assert_eq!(beat_json(c, &peer, UPDATING).await, denied());
    assert_eq!(
        s2_told(),
        ServiceReply::Nothing,
        "still updating: the slot is held"
    );
    assert!(
        enrollment
            .enrollment
            .store()
            .is_retiring(&host_id("s1"))
            .expect("read")
    );

    assert_eq!(beat_json(c, &peer, DONE).await, denied());
    assert_eq!(s2_told(), order(), "final: the slot is free");
    assert!(
        !enrollment
            .enrollment
            .store()
            .is_retiring(&host_id("s1"))
            .expect("read")
    );
    assert!(
        enrollment
            .enrollment
            .store()
            .is_denied(&host_id("s1"))
            .expect("read")
    );

    let (body, read) = watched();
    let request = Request::post(HEARTBEAT_PATH).body(body).expect("a request");
    let response = handle(c, Surface::Heartbeat, &peer, request, PATIENCE).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !read.load(Ordering::SeqCst),
        "a retired host's heartbeat was read"
    );
}

/// Catches a denied host answered at its heartbeat, or renewing, with a certificate that has
/// not expired.
#[tokio::test]
async fn a_denied_host_is_refused_at_handshake_and_cannot_renew() {
    let enrollment = Rig::new("hb-denied");
    let key = HostKey::new();
    let certificate = enrolled(&enrollment.enrollment, &key, "machine-1", "s1");
    let rig = coordinator_over("hb-denied-coordinator", &enrollment).await;
    let peer = peer_of(&certificate);
    assert_eq!(
        beat_json(&rig.coordinator, &peer, DONE).await,
        (StatusCode::OK, json!({"reply": "nothing"}))
    );

    enrollment
        .enrollment
        .store()
        .deny(&host_id("s1"))
        .expect("denied");
    assert_eq!(beat_json(&rig.coordinator, &peer, DONE).await, denied());
    let (body, read) = watched();
    let request = Request::post(HEARTBEAT_PATH).body(body).expect("a request");
    let response = handle(
        &rig.coordinator,
        Surface::Heartbeat,
        &peer,
        request,
        PATIENCE,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let text = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    assert_eq!(text, Bytes::from_static(br#"{"error":"denied"}"#));
    assert!(
        !read.load(Ordering::SeqCst),
        "a denied host's heartbeat was read"
    );

    assert_eq!(
        enrollment.enrollment.renew(
            Some(certificate.as_slice()),
            RenewRequest { csr_der: key.csr() }
        ),
        Err(RpcError::Refused(CoordinatorRefusal::Denied))
    );
}

/// Catches an enrolled certificate read as another host or role, one read without enrollment,
/// and a host file or deny row that cannot be read letting the host be identified, admitted or
/// ordered.
#[test]
fn an_enrolled_certificate_names_its_host_and_a_broken_store_file_fails_closed() {
    let enrollment = Rig::new("hb-identity");
    let key = HostKey::new();
    let certificate = enrolled(&enrollment.enrollment, &key, "machine-1", "s1");
    let rig = crate::testutil::Rig::new("hb-identity-coordinator");
    let plain = crate::testutil::Rig::start(&rig.dir, &rig.clock);
    let enrolled_one = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    let s1 = Identity {
        host_id: id("s1"),
        role: role("storage"),
    };
    assert_eq!(enrolled_one.identify(&certificate), Ok(s1.clone()));
    assert_eq!(enrolled_one.admission(&s1), Admission::Admitted);
    assert!(!enrolled_one.host_is_duplicate(&s1.host_id));
    assert!(matches!(
        enrolled_one.identify(b"not a certificate"),
        Err(IdentityError::NotACertificate(_))
    ));
    assert!(plain.identify(&certificate).is_err());
    assert!(plain.enrollment().is_none());
    assert_eq!(plain.admission(&s1), Admission::Admitted);
    assert!(!plain.host_is_duplicate(&s1.host_id));

    let newer = b"\x08\x02";
    std::fs::write(enrollment.files.store.join("hosts/s1.pb"), newer).expect("written");
    assert!(enrolled_one.identify(&certificate).is_err());
    assert!(
        enrolled_one.host_is_duplicate(&s1.host_id),
        "ordered nothing"
    );
    std::fs::write(enrollment.files.store.join("deny/s1.pb"), newer).expect("written");
    assert_eq!(enrolled_one.admission(&s1), Admission::Denied);
}

/// Catches a live report of a host the store never approved (a certificate of the farm CA)
/// stopping its heartbeat, and a final report that releases a host still updating, or does not
/// release one that is done.
#[tokio::test]
async fn a_farm_host_is_not_tracked_and_a_final_report_releases_only_a_final_host() {
    let enrollment = Rig::new("hb-farm-host");
    let rig = coordinator_over("hb-farm-host-coordinator", &enrollment).await;
    let farm_host = Identity {
        host_id: id("d1"),
        role: role("storage"),
    };
    let told = rig
        .coordinator
        .heartbeat_from(farm_host.clone(), beat("1.0"), Some("machine-1"))
        .expect("a heartbeat")
        .reply;
    assert_eq!(told, order());

    let plain = crate::testutil::Rig::new("hb-final-plain");
    plain.at(2_000);
    assert!(
        !plain
            .coordinator
            .final_report(farm_host.clone(), updating(beat("1.0"), "2.0"))
    );
    assert!(plain.coordinator.final_report(farm_host, beat("2.0")));
}

/// Catches a retiring host left holding its slot when its deny row cannot be made final, and
/// that failure going unlogged.
#[test]
fn a_deny_row_that_cannot_be_made_final_still_frees_the_slot() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("hb-retire-fault", fs.clone(), os_random());
    let key = HostKey::new();
    let certificate = enrolled(&enrollment.enrollment, &key, "machine-1", "s1");
    let rig = crate::testutil::Rig::new("hb-retire-fault-coordinator");
    let coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    enrollment
        .enrollment
        .store()
        .deny(&host_id("s1"))
        .expect("denied");
    let s1 = identity(&coordinator, &certificate);
    let log = Log::default();
    let guard = log.install();
    // Retiring takes the store's lock (1), then writes the deny row (2): that write fails.
    fs.fail_at(fs.count() + 2);
    assert!(coordinator.final_report(s1, beat("2.0")));
    drop(guard);
    assert!(
        log.text().contains("cannot mark a retired host final"),
        "{}",
        log.text()
    );
    assert!(
        enrollment
            .enrollment
            .store()
            .is_retiring(&host_id("s1"))
            .expect("read")
    );
}

/// Catches a host released with an update in flight (updating, or its promotion pending) or
/// an order outstanding, one kept once all have ended, and another host's state dropped with
/// it.
#[test]
fn a_host_is_released_only_when_no_update_or_order_of_it_is_in_flight() {
    let mut fleet = Fleet::new(secs(0));
    let (s1, s2) = (id("s1"), id("s2"));
    fleet.record_in("kbf", s2.clone(), role("storage"), beat("1.0"), secs(10));
    fleet.order("kbf", s2.clone(), version("2.0"), secs(10));
    fleet.record_in(
        "kbf",
        s1.clone(),
        role("storage"),
        updating(beat("1.0"), "2.0"),
        secs(10),
    );
    assert!(!fleet.release_if_final(&s1, secs(10)), "updating");
    let mut promoting = beat("2.0");
    promoting.promotion_pending = true;
    fleet.record_in("kbf", s1.clone(), role("storage"), promoting, secs(20));
    assert!(
        !fleet.release_if_final(&s1, secs(20)),
        "its promotion is pending"
    );
    fleet.record_in("kbf", s1.clone(), role("storage"), beat("2.0"), secs(30));
    fleet.order("other", s1.clone(), version("3.0"), secs(30));
    assert!(
        !fleet.release_if_final(&s1, secs(30)),
        "an order is outstanding"
    );
    assert!(
        fleet.release_if_final(&s1, secs(30) + ORDER_AGE),
        "the order aged out"
    );
    assert!(fleet.get_in("kbf", &s1).is_none());
    assert!(fleet.get_in("kbf", &s2).is_some(), "another host is kept");
    assert!(fleet.is_ordered("kbf", &s2, secs(10)), "with its order");
}
