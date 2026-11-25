//! The detail of a verdict (`detail`): what the hook said, read cut to `MAX_DETAIL_BYTES` at a
//! character boundary, read as absent when it is not text, never written when absent, and no
//! part of a verdict's identity.

use crate::wire::{MAX_DETAIL_BYTES, Outcome, UpdateResult, cut_text};

fn read(text: &str) -> UpdateResult {
    serde_json::from_str(text).expect("an update result")
}

/// Catches the detail lost on reading or on writing back, and a key written for none: a verdict
/// with a detail is written back byte for byte, and one without writes no `detail`.
#[test]
fn the_detail_of_a_verdict_is_read_and_written_back() {
    let text = r#"{"target":"2.0","outcome":"failed","phase":"halt","reason":"state_invalid","detail":"state_root: owned by uid 501"}"#;
    let result = read(text);
    assert_eq!(
        result.detail.as_deref(),
        Some("state_root: owned by uid 501")
    );
    assert_eq!(serde_json::to_string(&result).expect("json"), text);
    let none = r#"{"target":"2.0","outcome":"failed"}"#;
    assert_eq!(read(none).detail, None);
    assert_eq!(serde_json::to_string(&read(none)).expect("json"), none);
}

/// Catches a detail kept whole however long it is, one cut inside a character, and one of
/// exactly the bound being cut: a detail of `MAX_DETAIL_BYTES` is kept, one a byte over is cut to
/// the bound, and one whose last character would cross the bound is cut before that character.
#[test]
fn a_long_detail_is_cut_to_the_bound_at_a_character_boundary() {
    let of = |detail: &str| {
        let text = serde_json::json!({"target": "2.0", "outcome": "failed", "detail": detail});
        read(&text.to_string()).detail.expect("a detail")
    };
    let exact = "x".repeat(MAX_DETAIL_BYTES);
    assert_eq!(of(&exact), exact);
    assert_eq!(of(&format!("{exact}y")), exact);
    let straddling = format!("{}é", "x".repeat(MAX_DETAIL_BYTES - 1));
    assert_eq!(of(&straddling), "x".repeat(MAX_DETAIL_BYTES - 1));
    assert_eq!(of(""), "");
    assert_eq!(cut_text("é", 1), "");
    assert_eq!(cut_text("ab", 5), "ab");
}

/// Catches a detail that is not text refusing the whole verdict, or being kept: each reads as
/// absent and the rest of the verdict stands.
#[test]
fn a_detail_that_is_not_text_reads_as_absent() {
    for text in [
        r#"{"target":"2.0","outcome":"failed","reason":"bad_unit","detail":7}"#,
        r#"{"target":"2.0","outcome":"failed","reason":"bad_unit","detail":null}"#,
        r#"{"target":"2.0","outcome":"failed","reason":"bad_unit","detail":["a"]}"#,
    ] {
        let result = read(text);
        assert_eq!(result.detail, None, "{text}");
        assert_eq!(result.outcome, Outcome::Failed, "{text}");
        assert_eq!(
            result.reason.as_ref().map(|r| r.as_str()),
            Some("bad_unit"),
            "{text}"
        );
    }
}

/// Catches a halt or an acknowledgement that no longer matches its verdict because one copy
/// holds the detail and the other not (one stored before the field existed): two verdicts that
/// differ only in their detail are equal, and two that differ in any other field are not.
#[test]
fn two_verdicts_that_differ_only_in_their_detail_are_equal() {
    let stored = read(r#"{"target":"2.0","outcome":"failed","reason":"state_invalid"}"#);
    let said =
        read(r#"{"target":"2.0","outcome":"failed","reason":"state_invalid","detail":"why"}"#);
    assert_eq!(stored, said);
    for other in [
        r#"{"target":"2.1","outcome":"failed","reason":"state_invalid","detail":"why"}"#,
        r#"{"target":"2.0","outcome":"succeeded","reason":"state_invalid","detail":"why"}"#,
        r#"{"target":"2.0","outcome":"failed","phase":"halt","reason":"state_invalid"}"#,
        r#"{"target":"2.0","outcome":"failed","class":"release","reason":"state_invalid"}"#,
        r#"{"target":"2.0","outcome":"failed","reason":"bad_unit","detail":"why"}"#,
        r#"{"target":"2.0","outcome":"failed","reason":"state_invalid","order_id":"o-1"}"#,
        r#"{"target":"2.0","outcome":"failed","reason":"state_invalid","order_try":2}"#,
    ] {
        assert_ne!(stored, read(other), "{other}");
    }
}
