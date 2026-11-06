//! The host's clock and certificate facts of a heartbeat: `sent_unix`, `cert_not_after_unix`
//! and `last_renew_error`, three optional fields an agent that predates them leaves out.

use serde::Deserialize;

use crate::wire::{Heartbeat, MAX_RENEW_ERROR, clip_renew_error, read_heartbeat};

/// A heartbeat of an agent that predates the three fields.
const OLD_BEAT: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy"}"#;

/// The same heartbeat with the three fields.
const NEW_BEAT: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","sent_unix":2000000000,"cert_not_after_unix":2000600000,"last_renew_error":"renewal refused: unknown host"}"#;

/// The heartbeat of the release before the three fields, as its coordinator reads it: every
/// field it knows, and no more.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct PreviousHeartbeat {
    platform: String,
    version: String,
    health: String,
}

#[test]
fn a_heartbeat_with_the_three_fields_is_written_and_read_back_as_it_was() {
    let read: Heartbeat = serde_json::from_str(NEW_BEAT).expect("a heartbeat");
    assert_eq!(read.sent_unix, Some(2_000_000_000));
    assert_eq!(read.cert_not_after_unix, Some(2_000_600_000));
    assert_eq!(
        read.last_renew_error.as_deref(),
        Some("renewal refused: unknown host")
    );
    let written = serde_json::to_string(&read).expect("json");
    let again: Heartbeat = serde_json::from_str(&written).expect("a heartbeat");
    assert_eq!(again, read);
}

#[test]
fn a_heartbeat_without_them_reads_with_all_three_absent_and_is_written_without_them() {
    let read: Heartbeat = serde_json::from_str(OLD_BEAT).expect("a heartbeat");
    assert_eq!(read.sent_unix, None);
    assert_eq!(read.cert_not_after_unix, None);
    assert_eq!(read.last_renew_error, None);
    let written: serde_json::Value = serde_json::to_value(&read).expect("json");
    for key in ["sent_unix", "cert_not_after_unix", "last_renew_error"] {
        assert_eq!(
            written.get(key),
            None,
            "{key} is written only when it is there"
        );
    }
}

#[test]
fn the_previous_release_reads_a_heartbeat_with_the_three_fields() {
    let read: PreviousHeartbeat = serde_json::from_str(NEW_BEAT).expect("the old reader");
    assert_eq!(
        read,
        PreviousHeartbeat {
            platform: "linux-x86_64".to_owned(),
            version: "1.0".to_owned(),
            health: "healthy".to_owned(),
        }
    );
}

#[test]
fn a_bad_value_of_one_of_them_reads_as_absent_and_the_heartbeat_is_kept() {
    let beat = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","sent_unix":"soon","cert_not_after_unix":-1,"last_renew_error":42}"#;
    let read: Heartbeat = serde_json::from_str(beat).expect("a heartbeat");
    assert_eq!(read.sent_unix, None);
    assert_eq!(read.cert_not_after_unix, None);
    assert_eq!(read.last_renew_error, None);
    let null = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","sent_unix":null,"last_renew_error":""}"#;
    let read: Heartbeat = serde_json::from_str(null).expect("a heartbeat");
    assert_eq!(read.sent_unix, None);
    assert_eq!(read.last_renew_error, None, "an empty text is no error");
}

#[test]
fn a_long_renew_error_is_cut_to_the_bound_at_a_character_boundary() {
    // One byte, then two-byte characters: the bound falls inside one of them.
    let long = format!("a{}", "é".repeat(200));
    let clipped = clip_renew_error(&long);
    assert_eq!(clipped, format!("a{}", "é".repeat(127)));
    assert_eq!(clipped.len(), MAX_RENEW_ERROR - 1);
    let exact = "b".repeat(MAX_RENEW_ERROR);
    assert_eq!(
        clip_renew_error(&exact),
        exact,
        "a text at the bound is kept whole"
    );
    assert_eq!(clip_renew_error("short"), "short");

    let beat = serde_json::json!({
        "platform": "linux-x86_64", "version": "1.0", "health": "healthy",
        "last_renew_error": long,
    });
    let read: Heartbeat = serde_json::from_value(beat).expect("a heartbeat");
    assert_eq!(read.last_renew_error.as_deref(), Some(clipped));
}

#[test]
fn the_loose_read_of_a_heartbeat_with_a_bad_fact_keeps_the_three_fields() {
    let beat = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","step":7,"sent_unix":2000000000,"cert_not_after_unix":2000600000,"last_renew_error":"down"}"#;
    let read = read_heartbeat(beat.as_bytes(), None).expect("read loosely");
    assert_eq!(read.extras_dropped_facts, ["step"]);
    assert_eq!(read.beat.sent_unix, Some(2_000_000_000));
    assert_eq!(read.beat.cert_not_after_unix, Some(2_000_600_000));
    assert_eq!(read.beat.last_renew_error.as_deref(), Some("down"));
}
