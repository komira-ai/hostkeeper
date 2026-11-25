//! The halt a host holds of a service of its own (`local_halt`), read from a service entry and
//! from the top level, and the refusal an order gets while it is held
//! ([`UpdateResult::is_halted_refusal`]).

use serde_json::{Value, json};

use crate::names::Platform;
use crate::wire::{
    Class, HALTED_REFUSAL, LocalHalt, Outcome, Phase, ServiceBeat, Token, UpdateResult,
    read_heartbeat,
};

fn beat_with(service: Value, extra: Value) -> Vec<u8> {
    let mut wire = json!({"platform": "linux-x86_64", "version": "1.0", "health": "healthy",
        "services": [service]});
    wire.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    serde_json::to_vec(&wire).expect("json")
}

fn web(local_halt: Option<Value>) -> Value {
    let mut one = json!({"name": "web", "version": "1.0", "health": "healthy"});
    if let Some(value) = local_halt {
        one["local_halt"] = value;
    }
    one
}

fn halt(reason: Option<&str>, generation: u64, age_secs: Option<u64>) -> LocalHalt {
    LocalHalt {
        reason: reason.and_then(Token::parse),
        generation,
        age_secs,
    }
}

fn read_service(local_halt: Value) -> ServiceBeat {
    let read = read_heartbeat(&beat_with(web(Some(local_halt)), json!({})), None).expect("read");
    assert_eq!(read.services_dropped, None);
    assert_eq!(read.extras_dropped, None);
    read.beat
        .services
        .expect("the services")
        .into_iter()
        .next()
        .expect("one service")
}

/// Proves: a full local halt in a service entry is read with its reason, generation and age,
/// copied into the service's heartbeat, and written back as it was read.
#[test]
fn a_services_full_local_halt_is_read_and_carried() {
    let one = read_service(json!({"reason": "disk_bad", "generation": 3, "age_secs": 90}));
    let want = halt(Some("disk_bad"), 3, Some(90));
    assert_eq!(one.local_halt, Some(want.clone()));
    assert_eq!(
        one.as_heartbeat(Platform::LinuxX86_64).local_halt,
        Some(want)
    );
    let written = serde_json::to_value(&one).expect("json");
    assert_eq!(
        written["local_halt"],
        json!({"reason": "disk_bad", "generation": 3, "age_secs": 90})
    );
    let back: ServiceBeat = serde_json::from_value(written).expect("back");
    assert_eq!(back, one);
}

/// Proves: a local halt whose reason is absent, null or no token is still a halt: the reason
/// reads as none, the generation and age are kept.
#[test]
fn a_local_halt_without_a_usable_reason_keeps_the_halt() {
    let cases = [
        json!({"generation": 2, "age_secs": 5}),
        json!({"reason": null, "generation": 2, "age_secs": 5}),
        json!({"reason": "Disk Bad", "generation": 2, "age_secs": 5}),
        json!({"reason": 7, "generation": 2, "age_secs": 5}),
    ];
    for case in cases {
        let one = read_service(case.clone());
        assert_eq!(one.local_halt, Some(halt(None, 2, Some(5))), "{case}");
    }
}

/// Proves: an age that is absent or no whole number reads as none while the halt and its
/// generation are kept.
#[test]
fn a_local_halt_without_a_usable_age_keeps_the_halt() {
    for case in [
        json!({"generation": 4}),
        json!({"generation": 4, "age_secs": -1}),
        json!({"generation": 4, "age_secs": "soon"}),
    ] {
        let one = read_service(case.clone());
        assert_eq!(one.local_halt, Some(halt(None, 4, None)), "{case}");
    }
}

/// Proves: a local halt that is no object, or whose generation is a text, a negative or
/// missing, reads as none, and the beat is still read with its other facts and drops nothing.
#[test]
fn a_bad_local_halt_reads_as_none_and_costs_nothing() {
    let cases = [
        json!("halted"),
        json!(7),
        json!([1]),
        json!({"generation": "3"}),
        json!({"generation": -3}),
        json!({"reason": "disk_bad"}),
    ];
    for case in cases {
        let mut one = web(Some(case.clone()));
        one["record_try"] = json!(6);
        let read =
            read_heartbeat(&beat_with(one, json!({"local_halt": case})), None).expect("read");
        assert_eq!(read.services_dropped, None, "{case}");
        assert_eq!(read.extras_dropped, None, "{case}");
        assert_eq!(read.beat.local_halt, None, "{case}");
        let services = read.beat.services.expect("the services");
        assert_eq!(services.len(), 1, "{case}");
        assert_eq!(services[0].name.as_str(), "web", "{case}");
        assert_eq!(services[0].local_halt, None, "{case}");
        assert_eq!(services[0].record_try, Some(6), "{case}");
    }
}

/// Proves: a service with no local halt is written without the key, byte for byte.
#[test]
fn no_local_halt_writes_no_key() {
    let read = read_heartbeat(&beat_with(web(None), json!({})), None).expect("read");
    let services = read.beat.services.expect("the services");
    assert_eq!(services[0].local_halt, None);
    assert_eq!(
        serde_json::to_string(&services[0]).expect("json"),
        r#"{"name":"web","version":"1.0","health":"healthy","last_update":null,"updating":null}"#
    );
    assert_eq!(
        serde_json::to_string(&services[0].as_heartbeat(Platform::LinuxX86_64)).expect("json"),
        r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","last_update":null,"updating":null}"#
    );
}

/// Proves: a top-level local halt is kept when the heartbeat is read again without a bad
/// optional fact (the loose read), as it is on the strict read.
#[test]
fn a_top_level_local_halt_survives_a_dropped_fact() {
    let top = json!({"reason": "disk_bad", "generation": 9, "age_secs": 1});
    let strict = read_heartbeat(
        &beat_with(web(None), json!({"local_halt": top.clone()})),
        None,
    )
    .expect("read");
    assert_eq!(
        strict.beat.local_halt,
        Some(halt(Some("disk_bad"), 9, Some(1)))
    );
    let loose = read_heartbeat(
        &beat_with(web(None), json!({"local_halt": top, "commit": "maybe"})),
        None,
    )
    .expect("read");
    assert!(loose.extras_dropped.is_some(), "the bad fact is dropped");
    assert_eq!(
        loose.beat.local_halt,
        Some(halt(Some("disk_bad"), 9, Some(1)))
    );
}

fn result(outcome: Outcome, reason: Option<&str>) -> UpdateResult {
    let mut r = UpdateResult::new(
        crate::version::Version::parse("2.0").expect("a version"),
        outcome,
    );
    r.reason = reason.and_then(Token::parse);
    r
}

/// Proves: only a failure with no phase, no class and the reason `halted` is the halted
/// refusal; a success, a phase, a class, another reason or none is not.
#[test]
fn only_a_phase_less_classless_halted_failure_is_a_halted_refusal() {
    assert_eq!(HALTED_REFUSAL, "halted");
    assert!(result(Outcome::Failed, Some("halted")).is_halted_refusal());
    assert!(!result(Outcome::Succeeded, Some("halted")).is_halted_refusal());
    let mut phased = result(Outcome::Failed, Some("halted"));
    phased.phase = Some(Phase::Halt);
    assert!(!phased.is_halted_refusal());
    let mut classed = result(Outcome::Failed, Some("halted"));
    classed.class = Some(Class::Release);
    assert!(!classed.is_halted_refusal());
    assert!(!result(Outcome::Failed, Some("busy")).is_halted_refusal());
    assert!(!result(Outcome::Failed, None).is_halted_refusal());
}
