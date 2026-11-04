use serde::de::Expected;

use super::*;

/// Proves a document whose objects all have distinct keys, holding every kind of JSON value
/// (booleans, null, positive and negative integers, a float, strings with and without escapes,
/// arrays, nested objects and an empty object and array), has no repeated key. Catches a walk
/// that fails on a value kind it does not handle and so reports a key that is not there.
#[test]
fn a_document_of_distinct_keys_has_no_repeated_key() {
    let text = br#"{"a":true,"b":false,"c":null,"d":7,"e":-7,"f":1.5,"g":"plain","h":"es\"caped\n",
        "i":[1,"x",[null,{"a":1}],{"a":2,"b":[]}],"j":{"a":{"a":{"a":{}}}},"k":{},"l":[]}"#;
    assert_eq!(repeated_key(text), None);
    assert_eq!(repeated_key(b"[]"), None);
    assert_eq!(repeated_key(b"\"just text\""), None);
    assert_eq!(repeated_key(b"3"), None);
}

/// Proves the same key in different objects (siblings, parent and child, array elements) is not
/// a repeat. Catches a walk that shares one set of keys across objects.
#[test]
fn the_same_key_in_different_objects_is_not_a_repeat() {
    assert_eq!(
        repeated_key(br#"{"a":{"a":1},"b":[{"a":1},{"a":2}],"c":{"a":{"a":1}}}"#),
        None
    );
}

/// Proves a key twice in the top-level object is named, whatever the two values are. Catches a
/// walk that keeps the last value and so cannot see the first.
#[test]
fn a_key_twice_at_the_top_level_is_named() {
    assert_eq!(
        repeated_key(br#"{"schema":2,"schema":1}"#).as_deref(),
        Some("schema")
    );
    assert_eq!(
        repeated_key(br#"{"a":1,"b":2,"a":2}"#).as_deref(),
        Some("a")
    );
}

/// Proves a repeat inside an object nested in an object, and one inside an object in an array,
/// is found, and that the first repeat in document order is the one named. Catches a walk that
/// looks at the top level only.
#[test]
fn a_key_twice_in_a_nested_object_or_an_array_element_is_named() {
    assert_eq!(
        repeated_key(br#"{"x":{"y":{"k":1,"k":2}}}"#).as_deref(),
        Some("k")
    );
    assert_eq!(
        repeated_key(br#"{"x":[1,{"k":1},{"m":1,"m":2}]}"#).as_deref(),
        Some("m")
    );
    assert_eq!(
        repeated_key(br#"[{"p":1,"p":2},{"q":1,"q":2}]"#).as_deref(),
        Some("p")
    );
}

/// Proves two keys that are the same text written differently (an escape for a plain letter)
/// are one key. Catches a walk that compares the raw source text instead of the decoded key.
#[test]
fn keys_are_compared_as_decoded_text() {
    assert_eq!(repeated_key(br#"{"a":1,"\u0061":2}"#).as_deref(), Some("a"));
}

/// Proves a text that is not JSON gives no repeated key, wherever the syntax error is (after a
/// key, in a value, in an array element), and that a repeat before the error is still named.
/// Catches a walk that panics on bad syntax or reports a key it never finished reading.
#[test]
fn a_text_that_is_not_json_has_no_repeated_key_unless_it_repeats_first() {
    for bytes in [
        &b""[..],
        &b"{"[..],
        &b"{\"a\" 1}"[..],
        &b"{\"a\":tru}"[..],
        &b"[tru]"[..],
        &b"[1,"[..],
        &b"\xff"[..],
    ] {
        assert_eq!(repeated_key(bytes), None, "{bytes:?}");
    }
    assert_eq!(repeated_key(b"{\"a\":1,\"a\":tru").as_deref(), Some("a"));
}

/// Proves the visitor describes itself as any JSON value, which serde prints in a type error.
/// Catches an `expecting` that is empty or names something else.
#[test]
fn the_walk_describes_itself_as_any_json_value() {
    let found = Cell::new(None);
    let walk = Walk { found: &found };
    let expected: &dyn Expected = &walk;
    assert_eq!(expected.to_string(), "any JSON value");
}
