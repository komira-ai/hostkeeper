//! The maintenance report a heartbeat carries (`maintenance`): a text the wire keeps as it is,
//! read by the strict and the loose reader.

use serde_json::{Value, json};

use crate::wire::{Heartbeat, read_heartbeat};

const TEXT: &str = "CgR0ZXN0";

fn beat_with(extra: Value) -> Vec<u8> {
    let mut wire = json!({"platform": "linux-x86_64", "version": "1.0", "health": "healthy"});
    wire.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    serde_json::to_vec(&wire).expect("json")
}

/// Proves: a heartbeat with no `maintenance` reads it as none and writes no such key.
#[test]
fn an_absent_maintenance_reads_as_none_and_is_not_written() {
    let read = read_heartbeat(&beat_with(json!({})), None).expect("read");
    assert_eq!(read.extras_dropped, None);
    assert_eq!(read.beat.maintenance, None);
    let written = serde_json::to_value(&read.beat).expect("json");
    assert!(written.get("maintenance").is_none());
}

/// Proves: a maintenance text is kept as it is and written back unchanged, and the strict
/// reader reads it back equal.
#[test]
fn a_maintenance_text_is_kept_and_written_back_unchanged() {
    let read = read_heartbeat(&beat_with(json!({"maintenance": TEXT})), None).expect("read");
    assert_eq!(read.extras_dropped, None);
    assert_eq!(read.beat.maintenance.as_deref(), Some(TEXT));
    let written = serde_json::to_value(&read.beat).expect("json");
    assert_eq!(written["maintenance"], json!(TEXT));
    let back: Heartbeat = serde_json::from_value(written).expect("back");
    assert_eq!(back, read.beat);
}

/// Proves: a `maintenance` that is no text reads as none and the rest of the beat is read,
/// dropping nothing.
#[test]
fn a_maintenance_that_is_no_text_reads_as_none_and_costs_nothing() {
    for case in [
        json!(7),
        json!([1]),
        json!({"a": 1}),
        json!(true),
        Value::Null,
    ] {
        let read = read_heartbeat(
            &beat_with(json!({"maintenance": case, "record_try": 4})),
            None,
        )
        .expect("read");
        assert_eq!(read.services_dropped, None, "{case}");
        assert_eq!(read.extras_dropped, None, "{case}");
        assert_eq!(read.beat.maintenance, None, "{case}");
        assert_eq!(read.beat.record_try, Some(4), "{case}");
    }
}

/// Proves: a beat whose services list is dropped by the loose reader keeps its maintenance
/// text.
#[test]
fn a_beat_with_its_services_dropped_keeps_its_maintenance() {
    let services = json!([
        {"name": "web", "version": "1.0", "health": "healthy"},
        {"name": "web", "version": "1.0", "health": "healthy"}
    ]);
    let read = read_heartbeat(
        &beat_with(json!({"services": services, "maintenance": TEXT})),
        None,
    )
    .expect("read");
    assert!(read.services_dropped.is_some());
    assert_eq!(read.beat.services, None);
    assert_eq!(read.beat.maintenance.as_deref(), Some(TEXT));
}
