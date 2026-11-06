//! The bound of a heartbeat: the service limit and the 16 KiB.

use crate::verdict;
use hostkeeper_link::beat::{DROP_ORDER, MAX_SERVICES_PER_HEARTBEAT};
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, DeferredReport, Fitted, Health, Heartbeat, LinkError, Liveness,
    MAX_HEARTBEAT_BYTES, Observed, OptionalFact, Outcome, Oversize, Platform, Reason,
    RecordedService, ServiceFacts, fit,
};

use crate::{Rig, recorded, service, update, version};

/// The facts that are never dropped, with a verdict whose detail is `detail_len` bytes.
fn required(name: &str, detail_len: usize) -> ServiceFacts {
    let mut result = verdict(&update("o-1", 1, name, "2.0"), Outcome::Failed);
    result.detail = Some("d".repeat(detail_len));
    ServiceFacts {
        name: service(name),
        version: version("1.0"),
        health: Health::Healthy,
        updating: Some(version("2.0")),
        last_update: Some(result),
        deferred: None,
        health_detail: None,
        observed: None,
    }
}

fn observed() -> Option<Observed> {
    Some(Observed {
        liveness: Liveness::Running,
        version: Some(version("1.0")),
    })
}

/// `facts` with every optional fact, the health detail `health_len` bytes.
fn with_optional(mut facts: ServiceFacts, health_len: usize) -> ServiceFacts {
    facts.health_detail = Some("h".repeat(health_len));
    facts.deferred = report();
    facts.observed = observed();
    facts
}

fn beat(services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services,
    }
}

/// Catches a fact dropped out of the documented order, a required fact (the service, its
/// version, health and running update, the verdict) dropped, a heartbeat that lost more than it
/// had to, and one sent over the bound. Three sizes: one that fits once the health detail is
/// gone, one that fits only once every optional fact is gone, and one that does not fit.
#[test]
fn heartbeat_over_16_kib_drops_optional_facts_first_in_order_and_refuses_if_still_too_big() {
    assert_eq!(MAX_HEARTBEAT_BYTES, 16 * 1024);
    assert_eq!(
        DROP_ORDER,
        [
            OptionalFact::HealthDetail,
            OptionalFact::DeferralDetail,
            OptionalFact::ObserverDetail,
        ]
    );

    // Within the bound: nothing is dropped.
    let small = beat(vec![with_optional(required("x", 10), 10)]);
    assert_eq!(
        fit(small.clone()),
        Ok(Fitted {
            beat: small,
            dropped: Vec::new(),
        })
    );

    // Over it by the health detail alone: only that is dropped.
    let whole = with_optional(required("x", 0), MAX_HEARTBEAT_BYTES);
    let mut kept = whole.clone();
    kept.health_detail = None;
    assert_eq!(
        fit(beat(vec![whole])),
        Ok(Fitted {
            beat: beat(vec![kept]),
            dropped: vec![OptionalFact::HealthDetail],
        })
    );

    // The bytes of the required facts, and what the observer's detail adds to them.
    let base = beat(vec![required("x", 0)]).encoded_len();
    let mut with_observed = required("x", 0);
    with_observed.observed = observed();
    let observer = beat(vec![with_observed]).encoded_len() - base;
    assert!(observer >= 2, "{observer}");

    // Required facts that fit only without the observer's detail: everything optional goes.
    let pad = MAX_HEARTBEAT_BYTES - base - observer / 2;
    let fitted = fit(beat(vec![with_optional(required("x", pad), 100)])).expect("it fits");
    assert_eq!(fitted.dropped, DROP_ORDER.to_vec());
    assert_eq!(fitted.beat, beat(vec![required("x", pad)]));
    assert_eq!(
        fitted.beat.encoded_len(),
        MAX_HEARTBEAT_BYTES - observer / 2
    );

    // Required facts one byte over the bound: refused, never cut.
    let pad = MAX_HEARTBEAT_BYTES - base + 1;
    assert_eq!(
        fit(beat(vec![with_optional(required("x", pad), 100)])),
        Err(Oversize::Bytes {
            len: MAX_HEARTBEAT_BYTES + 1
        })
    );
}

/// Catches a heartbeat over the bound sent anyway, or refused without a word: the loop makes no
/// call and logs why.
#[test]
fn a_heartbeat_still_too_big_is_refused_logged_and_not_sent() {
    let rig = Rig::new("size-refused");
    let mut kept = verdict(&update("o-1", 1, "x", "2.0"), Outcome::Failed);
    kept.detail = Some("d".repeat(MAX_HEARTBEAT_BYTES));
    rig.install(vec![RecordedService {
        last_result: Some(kept),
        ..recorded("x", "1.0")
    }]);
    let mut heartbeater = rig.heartbeater();
    let len = match heartbeater.beat_once() {
        BeatOutcome::NotSent(LinkError::TooLarge(Oversize::Bytes { len })) => len,
        other => panic!("{other:?}"),
    };
    assert!(len > MAX_HEARTBEAT_BYTES, "{len}");
    assert!(rig.world.link.calls().is_empty());
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "error: the heartbeat is not sent: it is {len} bytes with every optional fact \
             dropped, more than 16384"
        )]
    );
}

/// Catches a heartbeat that lists the first services and leaves the others out, or that
/// refuses one at the limit: one service over is refused with no call, the limit is sent whole.
#[test]
fn a_heartbeat_over_the_service_limit_is_refused_not_truncated() {
    assert_eq!(
        MAX_SERVICES_PER_HEARTBEAT,
        hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT
    );
    let rig = Rig::new("size-services");
    let names: Vec<String> = (0..=MAX_SERVICES_PER_HEARTBEAT)
        .map(|n| format!("s{n}"))
        .collect();
    rig.install(names.iter().map(|name| recorded(name, "1.0")).collect());
    let mut heartbeater = rig.heartbeater();
    assert_eq!(
        heartbeater.beat_once(),
        BeatOutcome::NotSent(LinkError::TooLarge(Oversize::Services { count: 17 }))
    );
    assert!(rig.world.link.calls().is_empty());
    assert_eq!(
        rig.world.log.lines(),
        ["error: the heartbeat is not sent: it lists 17 services, more than 16"]
    );

    rig.world.records.set(
        names[..MAX_SERVICES_PER_HEARTBEAT]
            .iter()
            .map(|name| recorded(name, "1.0"))
            .collect(),
    );
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(rig.world.link.beats()[0].services.len(), 16);
}

fn report() -> Option<DeferredReport> {
    Some(DeferredReport {
        target: version("2.0"),
        reason: Reason::new("drain_bound").expect("a reason"),
        retry_after_secs: 60,
    })
}

/// Catches a bound off by one (a heartbeat of exactly 16384 bytes is sent whole), a drop that
/// goes on after the heartbeat fits (the observer's detail is kept when dropping the deferral
/// detail is enough), bytes counted as characters (an escaped quote is two bytes, an `é` two),
/// and a group dropped from one service only.
#[test]
fn the_bound_is_exact_and_dropping_stops_as_soon_as_the_heartbeat_fits() {
    let base = beat(vec![required("x", 0)]).encoded_len();
    let exact = beat(vec![required("x", MAX_HEARTBEAT_BYTES - base)]);
    assert_eq!(exact.encoded_len(), MAX_HEARTBEAT_BYTES);
    assert_eq!(
        fit(exact.clone()),
        Ok(Fitted {
            beat: exact,
            dropped: Vec::new(),
        })
    );

    // Fits once the deferral detail is gone, with the observer's detail kept.
    let mut observed_only = required("x", 0);
    observed_only.observed = observed();
    let pad = MAX_HEARTBEAT_BYTES - beat(vec![observed_only]).encoded_len();
    let fitted = fit(beat(vec![with_optional(required("x", pad), 10)])).expect("it fits");
    assert_eq!(
        fitted.dropped,
        [OptionalFact::HealthDetail, OptionalFact::DeferralDetail]
    );
    let mut kept = required("x", pad);
    kept.observed = observed();
    assert_eq!(fitted.beat, beat(vec![kept]));
    assert_eq!(fitted.beat.encoded_len(), MAX_HEARTBEAT_BYTES);

    // Bytes of the JSON text: escapes and multibyte characters count as what they take.
    let sized = |detail: &str| {
        let mut facts = required("x", 0);
        facts.health_detail = Some(detail.to_owned());
        beat(vec![facts]).encoded_len()
    };
    assert_eq!(sized(&"\"".repeat(10)) - sized(""), 20);
    assert_eq!(sized(&"\u{e9}".repeat(10)) - sized(""), 20);
    assert_eq!(sized(&"a".repeat(10)) - sized(""), 10);

    // Several services: each group goes from every service at once.
    let names: Vec<String> = (0..16).map(|n| format!("s{n}")).collect();
    let many: Vec<ServiceFacts> = names
        .iter()
        .map(|name| with_optional(required(name, 0), MAX_HEARTBEAT_BYTES / 16))
        .collect();
    let fitted = fit(beat(many)).expect("it fits");
    assert_eq!(fitted.dropped, [OptionalFact::HealthDetail]);
    for facts in &fitted.beat.services {
        assert_eq!(facts.health_detail, None);
        assert_eq!(facts.deferred, report());
        assert_eq!(facts.observed, observed());
    }
}
