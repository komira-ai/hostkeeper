//! The detail of a verdict (`detail`): what the hook said, read cut to `MAX_DETAIL_BYTES` at a
//! character boundary with the mark `…`, read as absent when it is not text, never written when absent, and no
//! part of a verdict's identity.

use crate::wire::{CUT_MARK, MAX_DETAIL_BYTES, Outcome, UpdateResult, cut_marked, cut_text};

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

/// Catches a detail kept whole however long it is, one cut inside a character, one of exactly
/// the bound being cut, and a cut a reader cannot tell from a whole text: a detail of
/// `MAX_DETAIL_BYTES` is kept, one a byte over is cut to the bound ending in the mark `…`, and one
/// whose last character kept would cross the room left for the mark is cut before that character.
#[test]
fn a_long_detail_is_cut_to_the_bound_at_a_character_boundary_with_a_mark() {
    let of = |detail: &str| {
        let text = serde_json::json!({"target": "2.0", "outcome": "failed", "detail": detail});
        read(&text.to_string()).detail.expect("a detail")
    };
    assert_eq!(CUT_MARK, "\u{2026}");
    assert_eq!(CUT_MARK.len(), 3);
    let room = MAX_DETAIL_BYTES - CUT_MARK.len();
    let exact = "x".repeat(MAX_DETAIL_BYTES);
    assert_eq!(of(&exact), exact);
    let over = of(&format!("{exact}y"));
    assert_eq!(over, format!("{}\u{2026}", "x".repeat(room)));
    assert_eq!(over.len(), MAX_DETAIL_BYTES);
    let straddling = format!("{}é{}", "x".repeat(room - 1), "y".repeat(10));
    assert_eq!(of(&straddling), format!("{}\u{2026}", "x".repeat(room - 1)));
    assert_eq!(of(""), "");
    assert_eq!(cut_text("é", 1), "");
    assert_eq!(cut_text("ab", 5), "ab");
    assert_eq!(cut_marked("abcdef", 6), "abcdef");
    assert_eq!(cut_marked("abcdefg", 6), "abc\u{2026}");
    assert_eq!(cut_marked("abcd", 3), "\u{2026}");
    assert_eq!(cut_marked("abcd", 2), "");
    assert_eq!(cut_marked("abc", 3), "abc");
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

/// Catches the deferrals of a running `verify` lost on reading or writing back, a bad field of
/// them costing the step, and an agent that predates them read as deferring: a step with
/// deferrals is read and written back byte for byte, a reason that is no token, a detail that is
/// not text and a time that is not a whole number each read as absent, deferrals that are not an
/// object read as none, a long detail is cut with the mark as a verdict's is, and a step with no
/// deferrals writes no key.
#[test]
fn the_deferrals_of_a_step_are_read_and_written_back() {
    use crate::wire::{StepDeferrals, StepRun};
    let step = |text: &str| serde_json::from_str::<StepRun>(text).expect("a step");
    let text = r#"{"name":"verify","attempt":3,"deferrals":{"count":2,"reason":"provider_busy","detail":"read refused","since_unix":1790000000}}"#;
    let read = step(text);
    assert_eq!(
        read.deferrals,
        Some(StepDeferrals {
            count: 2,
            reason: crate::wire::Token::parse("provider_busy"),
            detail: Some("read refused".to_owned()),
            since_unix: Some(1_790_000_000),
        })
    );
    assert_eq!(serde_json::to_string(&read).expect("json"), text);
    let bad = step(
        r#"{"name":"verify","attempt":3,"deferrals":{"count":2,"reason":"Not A Token","detail":7,"since_unix":"soon"}}"#,
    );
    assert_eq!(
        bad.deferrals,
        Some(StepDeferrals {
            count: 2,
            reason: None,
            detail: None,
            since_unix: None,
        })
    );
    assert_eq!(
        step(r#"{"name":"verify","attempt":3,"deferrals":"many"}"#).deferrals,
        None
    );
    let long = serde_json::json!({
        "name": "verify", "attempt": 1,
        "deferrals": {"count": 1, "detail": "z".repeat(MAX_DETAIL_BYTES + 1)},
    });
    let cut = step(&long.to_string()).deferrals.and_then(|d| d.detail);
    assert_eq!(
        cut,
        Some(cut_marked(
            &"z".repeat(MAX_DETAIL_BYTES + 1),
            MAX_DETAIL_BYTES
        ))
    );
    let older = r#"{"name":"verify","attempt":3}"#;
    assert_eq!(step(older).deferrals, None);
    assert_eq!(serde_json::to_string(&step(older)).expect("json"), older);
}
