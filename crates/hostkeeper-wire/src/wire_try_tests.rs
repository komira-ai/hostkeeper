//! The try a host's step record holds (`record_try`), read from a service entry and from the
//! top level, and the refusals of an order for its try alone ([`UpdateResult::is_try_refusal`]).

use serde_json::{Value, json};

use crate::names::Platform;
use crate::wire::{
    Class, Outcome, Phase, ServiceBeat, TRY_REFUSALS, Token, UpdateResult, read_heartbeat,
};

fn v(text: &str) -> crate::version::Version {
    crate::version::Version::parse(text).expect("a version")
}

fn beat_with(service: Value, extra: Value) -> Vec<u8> {
    let mut wire = json!({"platform": "linux-x86_64", "version": "1.0", "health": "healthy",
        "services": [service]});
    wire.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    serde_json::to_vec(&wire).expect("json")
}

fn web(record_try: Option<Value>) -> Value {
    let mut one = json!({"name": "web", "version": "1.0", "health": "healthy"});
    if let Some(value) = record_try {
        one["record_try"] = value;
    }
    one
}

/// Catches: a record try that is not read, not carried into the service's heartbeat, written
/// when absent, or that costs the host its services when it is no whole number: 4 reads 4 and
/// writes back as 4; absent, a text, a negative and a fraction read as none, drop nothing, and
/// leave the key out when written.
#[test]
fn a_services_record_try_is_read_leniently() {
    let cases = [
        (Some(json!(4)), Some(4)),
        (None, None),
        (Some(json!("four")), None),
        (Some(json!(-1)), None),
        (Some(json!(1.5)), None),
    ];
    for (value, want) in cases {
        let read = read_heartbeat(&beat_with(web(value.clone()), json!({})), None).expect("read");
        assert_eq!(read.services_dropped, None, "{value:?}");
        assert_eq!(read.extras_dropped, None, "{value:?}");
        let services = read.beat.services.expect("the services");
        assert_eq!(services[0].record_try, want, "{value:?}");
        assert_eq!(
            services[0].as_heartbeat(Platform::LinuxX86_64).record_try,
            want
        );
        let written = serde_json::to_value(&services[0]).expect("json");
        assert_eq!(written.get("record_try"), want.map(Value::from).as_ref());
        let back: ServiceBeat = serde_json::from_value(written).expect("back");
        assert_eq!(back, services[0]);
    }
}

/// Catches: a top-level record try lost when the heartbeat is read again without a bad
/// optional fact (the loose read), which a legacy service's state at the top level carries.
#[test]
fn a_top_level_record_try_survives_a_dropped_fact() {
    let strict = read_heartbeat(&beat_with(web(None), json!({"record_try": 7})), None)
        .expect("read")
        .beat;
    assert_eq!(strict.record_try, Some(7));
    let loose = read_heartbeat(
        &beat_with(web(None), json!({"record_try": 7, "commit": "maybe"})),
        None,
    )
    .expect("read");
    assert!(loose.extras_dropped.is_some(), "the bad fact is dropped");
    assert_eq!(loose.beat.record_try, Some(7));
}

fn result(outcome: Outcome, reason: Option<&str>) -> UpdateResult {
    let mut r = UpdateResult::new(v("2.0"), outcome);
    r.reason = reason.and_then(Token::parse);
    r
}

/// Catches: a refusal rule that passes over more than a refusal for the try. `try_conflict` and
/// `stale_try`, failed with no phase and no class, are; the same reasons with a phase, with a
/// class, or on a success are not, nor a phase-less failure for another reason or for none.
#[test]
fn only_a_phase_less_failure_for_the_try_is_a_try_refusal() {
    assert_eq!(TRY_REFUSALS, ["try_conflict", "stale_try"]);
    for reason in TRY_REFUSALS {
        assert!(
            result(Outcome::Failed, Some(reason)).is_try_refusal(),
            "{reason}"
        );
        let mut phased = result(Outcome::Failed, Some(reason));
        phased.phase = Some(Phase::BeforeCommit);
        assert!(!phased.is_try_refusal(), "{reason} in a phase");
        let mut classed = result(Outcome::Failed, Some(reason));
        classed.class = Some(Class::Release);
        assert!(!classed.is_try_refusal(), "{reason} with a class");
        assert!(
            !result(Outcome::Succeeded, Some(reason)).is_try_refusal(),
            "{reason} on a success"
        );
    }
    assert!(!result(Outcome::Failed, Some("busy")).is_try_refusal());
    assert!(!result(Outcome::Failed, None).is_try_refusal());
}
