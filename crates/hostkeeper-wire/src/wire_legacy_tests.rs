//! The legacy service: the name whose state is the top level of a heartbeat. It is a parameter
//! of `read_heartbeat`, never a constant of the wire.

use serde_json::{Value, json};

use crate::wire::{Heartbeat, read_heartbeat};

fn service(name: &str) -> Value {
    json!({"name": name, "version": "1.0", "health": "healthy"})
}

fn beat(extra: &[(&str, Value)], services: Vec<Value>) -> Vec<u8> {
    let mut wire = json!({
        "platform": "linux-x86_64",
        "version": "1.0",
        "health": "healthy",
        "updating": "2.0",
    });
    for (key, value) in extra {
        wire[key] = value.clone();
    }
    wire["services"] = json!(services);
    serde_json::to_vec(&wire).expect("json")
}

const WHY: &str = "kbf is the top level of a heartbeat, not a service";

/// Catches: the strict read reserving a name again: a list that names `kbf` is a list like any
/// other to `Heartbeat`, whose reader knows no legacy service.
#[test]
fn the_strict_read_reserves_no_name() {
    let read: Heartbeat =
        serde_json::from_slice(&beat(&[], vec![service("kbf"), service("db")])).expect("strict");
    let names: Vec<&str> = read
        .services
        .iter()
        .flatten()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["kbf", "db"]);
}

/// Catches: a list that names the legacy service kept, which would count its state twice, and
/// the rest of the heartbeat lost with it: the list is dropped, the reason is the fixed text
/// naming the legacy service, and the top level is read as sent.
#[test]
fn a_list_naming_the_legacy_service_is_dropped_with_its_reason() {
    let bytes = beat(&[], vec![service("db"), service("kbf")]);
    let read = read_heartbeat(&bytes, Some("kbf")).expect("read");
    assert_eq!(read.beat.services, None);
    assert_eq!(read.services_dropped.as_deref(), Some(WHY));
    assert_eq!(read.extras_dropped, None);
    assert_eq!(read.beat.version.as_str(), "1.0");
    assert_eq!(read.beat.updating.as_ref().map(|v| v.as_str()), Some("2.0"));
}

/// Catches: the reserved name hard-wired: with no legacy service nothing is reserved, and the
/// list that names `kbf` is kept as it is.
#[test]
fn without_a_legacy_service_no_name_is_reserved() {
    let bytes = beat(&[], vec![service("kbf"), service("db")]);
    let read = read_heartbeat(&bytes, None).expect("read");
    assert_eq!(read.services_dropped, None);
    assert_eq!(read.beat.services.as_ref().map(Vec::len), Some(2));
}

/// Catches: the reserved name being `kbf` whatever the parameter says: the reserved name is the
/// one given, and a list that does not name it is kept.
#[test]
fn the_reserved_name_is_the_one_given() {
    let bytes = beat(&[], vec![service("kbf"), service("db")]);
    let read = read_heartbeat(&bytes, Some("db")).expect("read");
    assert_eq!(read.beat.services, None);
    assert_eq!(
        read.services_dropped.as_deref(),
        Some("db is the top level of a heartbeat, not a service")
    );
    let kept = read_heartbeat(&bytes, Some("web")).expect("read");
    assert_eq!(kept.services_dropped, None);
    assert_eq!(kept.beat.services.as_ref().map(Vec::len), Some(2));
}

/// Catches: a heartbeat with no list read as if it had one, with or without a legacy service.
#[test]
fn a_heartbeat_with_no_list_drops_nothing() {
    let wire = json!({"platform": "linux-x86_64", "version": "1.0", "health": "healthy"});
    let bytes = serde_json::to_vec(&wire).expect("json");
    for legacy in [None, Some("kbf")] {
        let read = read_heartbeat(&bytes, legacy).expect("read");
        assert_eq!(read.beat.services, None);
        assert_eq!(read.services_dropped, None);
    }
}

/// Catches: the legacy check run only on the strict read: a heartbeat with a bad optional fact
/// goes through the loose read, and its list that names the legacy service is dropped there
/// too, beside the dropped fact, each with its own reason.
#[test]
fn the_loose_read_drops_a_list_naming_the_legacy_service_too() {
    let bytes = beat(&[("step", json!({"name": "verify"}))], vec![service("kbf")]);
    let read = read_heartbeat(&bytes, Some("kbf")).expect("read");
    assert_eq!(read.beat.services, None);
    assert_eq!(read.beat.step, None);
    assert_eq!(read.services_dropped.as_deref(), Some(WHY));
    assert_eq!(read.extras_dropped_facts, ["step"]);
    assert!(read.extras_dropped.is_some());
}

/// Catches: a list dropped for being bad being given a second reason: a list that is bad for
/// its own sake keeps that reason alone.
#[test]
fn a_list_bad_for_another_reason_keeps_that_reason() {
    let bytes = beat(&[], vec![service("kbf"), service("kbf")]);
    let read = read_heartbeat(&bytes, Some("kbf")).expect("read");
    assert_eq!(read.beat.services, None);
    assert_eq!(
        read.services_dropped.as_deref(),
        Some("a service is listed twice")
    );
}
