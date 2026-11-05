//! The order a verdict ends (`order_id`, `order_try`): read when an updater sends them, read as
//! absent when they are bad, and never written when absent, so an updater that predates them
//! is read and written as before.

use crate::version::Version;
use crate::wire::{MAX_ORDER_ID, OrderId, Outcome, UpdateResult};

fn read(text: &str) -> UpdateResult {
    serde_json::from_str(text).expect("an update result")
}

/// Catches: the order of a verdict lost on reading or on writing back.
#[test]
fn the_order_of_a_verdict_is_read_and_written_back() {
    let text = r#"{"target":"2.0","outcome":"failed","order_id":"o-17.a_b","order_try":2}"#;
    let result = read(text);
    assert_eq!(
        result.order_id.as_ref().map(OrderId::as_str),
        Some("o-17.a_b")
    );
    assert_eq!(result.order_try, Some(2));
    assert_eq!(serde_json::to_string(&result).expect("json"), text);
}

/// Catches: a bad order id or try refusing the whole verdict, or being kept: each reads as
/// absent and the rest of the verdict stands.
#[test]
fn a_bad_order_id_or_try_reads_as_absent() {
    for text in [
        r#"{"target":"2.0","outcome":"failed","order_id":"","order_try":-1}"#,
        r#"{"target":"2.0","outcome":"failed","order_id":"a b","order_try":"2"}"#,
        r#"{"target":"2.0","outcome":"failed","order_id":7,"order_try":null}"#,
        r#"{"target":"2.0","outcome":"failed","order_id":null,"order_try":1.5}"#,
    ] {
        let result = read(text);
        assert_eq!(
            result,
            UpdateResult::new(Version::parse("2.0").expect("a version"), Outcome::Failed),
            "{text}"
        );
    }
}

/// Catches: an absent order written as `null`, which would change the bytes an updater that
/// predates it, and the ops file, hold.
#[test]
fn a_verdict_without_an_order_is_written_as_before() {
    let result = UpdateResult::new(Version::parse("2.0").expect("a version"), Outcome::Failed);
    assert_eq!(
        serde_json::to_string(&result).expect("json"),
        r#"{"target":"2.0","outcome":"failed"}"#
    );
}

/// Catches: the order id's rule loosened or tightened at its bounds.
#[test]
fn an_order_id_is_one_to_sixty_four_bytes_of_its_characters() {
    let longest = "a".repeat(MAX_ORDER_ID);
    assert_eq!(
        OrderId::parse(&longest).map(String::from),
        Some(longest.clone())
    );
    assert_eq!(OrderId::parse(&format!("{longest}a")), None);
    assert_eq!(OrderId::parse(""), None);
    for bad in ["a/b", "a b", "a+b", "a\nb", "\u{e9}"] {
        assert_eq!(OrderId::parse(bad), None, "{bad:?}");
    }
    assert_eq!(
        OrderId::try_from("a:b".to_owned()),
        Err("not an order id: [A-Za-z0-9._-], 1 to 64 bytes")
    );
    assert_eq!(
        OrderId::try_from("Z-9".to_owned()).map(String::from),
        Ok("Z-9".to_owned())
    );
}
