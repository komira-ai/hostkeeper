//! The canonical JSON writer, on the fixture's message of every kind.

use std::collections::HashMap;

use prost_reflect::{DynamicMessage, Kind, MapKey, Value};

use super::{base64, compact, pretty, scalar_text, write_single, write_string};
use crate::testutil::{from_json, message};

/// Catches a kind written in another form than canonical JSON: 64-bit integers as numbers,
/// 32-bit ones as strings, enums as numbers, bytes in another encoding, floats wrongly, a
/// default written, or fields out of their declared order.
#[test]
fn every_kind_is_written_canonically() {
    let widget = from_json(
        "Widget",
        r#"{"name": "n", "i32": -1, "i64": "-2", "u32": 3, "u64": "18446744073709551615",
            "s32": -5, "s64": "-6", "f32": 7, "f64": "8", "sf32": -9, "sf64": "-10",
            "ratio": 1.5, "weight": 0.1, "flag": true, "maybe": false, "colour": "COLOUR_RED",
            "colours": ["COLOUR_RED", "COLOUR_DARK_BLUE"], "tags": ["a"], "labels": {"b": "2", "a": "1"},
            "inner": {}, "round": "r", "blob": "AAEC/w==", "counts": {"x": "9"},
            "inners": [{"note": "i"}, {}], "byNumber": {"10": "ten", "9": "nine"},
            "byTruth": {"true": "t", "false": "f"}, "byLong": {"-1": "m"}, "longs": ["1", "-1"],
            "nodeId": "id"}"#,
    );
    assert_eq!(
        compact(&widget),
        "{\"name\":\"n\",\"i32\":-1,\"i64\":\"-2\",\"u32\":3,\"u64\":\"18446744073709551615\",\
         \"s32\":-5,\"s64\":\"-6\",\"f32\":7,\"f64\":\"8\",\"sf32\":-9,\"sf64\":\"-10\",\
         \"ratio\":1.5,\"weight\":0.1,\"flag\":true,\"maybe\":false,\"colour\":\"COLOUR_RED\",\
         \"colours\":[\"COLOUR_RED\",\"COLOUR_DARK_BLUE\"],\"tags\":[\"a\"],\
         \"labels\":{\"a\":\"1\",\"b\":\"2\"},\"inner\":{},\"round\":\"r\",\"blob\":\"AAEC/w==\",\
         \"counts\":{\"x\":\"9\"},\"inners\":[{\"note\":\"i\"},{}],\
         \"byNumber\":{\"9\":\"nine\",\"10\":\"ten\"},\"byTruth\":{\"false\":\"f\",\"true\":\"t\"},\
         \"byLong\":{\"-1\":\"m\"},\"longs\":[\"1\",\"-1\"],\"nodeId\":\"id\"}"
    );
}

/// Catches pretty output that is not the compact output indented: a wrong indent, a trailing
/// comma, an empty object or list broken over lines.
#[test]
fn pretty_output_is_indented_by_two_spaces_a_level() {
    let m = from_json(
        "Widget",
        r#"{"name": "n", "inner": {"note": "x"}, "inners": [{}], "labels": {"k": "v"}}"#,
    );
    assert_eq!(
        pretty(&m, 0),
        "{\n  \"name\": \"n\",\n  \"labels\": {\n    \"k\": \"v\"\n  },\n  \"inner\": {\n    \
         \"note\": \"x\"\n  },\n  \"inners\": [\n    {}\n  ]\n}"
    );
    assert_eq!(pretty(&DynamicMessage::new(message("Empty")), 3), "{}");
    assert_eq!(compact(&DynamicMessage::new(message("Widget"))), "{}");
}

/// Catches a float that is not finite written as a bare word, which is not JSON, and an enum
/// number the reader does not know written as a name or dropped.
#[test]
fn non_finite_floats_and_unknown_enum_numbers() {
    let mut m = DynamicMessage::new(message("Widget"));
    let cases = [
        (
            Value::F32(f32::NAN),
            Value::F64(f64::INFINITY),
            "\"NaN\"",
            "\"Infinity\"",
        ),
        (
            Value::F32(f32::NEG_INFINITY),
            Value::F64(f64::NAN),
            "\"-Infinity\"",
            "\"NaN\"",
        ),
        (
            Value::F32(f32::INFINITY),
            Value::F64(-f64::INFINITY),
            "\"Infinity\"",
            "\"-Infinity\"",
        ),
    ];
    for (ratio, weight, r, w) in cases {
        m.set_field_by_name("ratio", ratio);
        m.set_field_by_name("weight", weight);
        assert_eq!(compact(&m), format!("{{\"ratio\":{r},\"weight\":{w}}}"));
    }
    let mut e = DynamicMessage::new(message("Widget"));
    e.set_field_by_name("colour", Value::EnumNumber(42));
    assert_eq!(compact(&e), "{\"colour\":42}");
}

/// Catches a string that breaks out of its quotes or carries a control character to a terminal.
#[test]
fn strings_are_escaped() {
    let mut out = String::new();
    write_string(&mut out, "a\"b\\c\nd\re\tf\u{1}g\u{7f}h\u{9b}i\u{e9}");
    assert_eq!(
        out,
        "\"a\\\"b\\\\c\\nd\\re\\tf\\u0001g\\u007fh\\u009bi\u{e9}\""
    );
}

/// Catches base64 without padding, with the URL alphabet, or with a wrong last group.
#[test]
fn bytes_are_standard_base64_with_padding() {
    let cases: [(&[u8], &str); 6] = [
        (b"", ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (&[0xfb, 0xff, 0xbf], "+/+/"),
    ];
    for (bytes, text) in cases {
        assert_eq!(base64(bytes), text, "{bytes:?}");
    }
}

/// Catches the text of a scalar keeping its quotes, or losing its escapes.
#[test]
fn scalar_text_drops_only_the_quotes() {
    assert_eq!(
        scalar_text(&Value::String("a\nb".into()), &Kind::String),
        "a\\nb"
    );
    assert_eq!(scalar_text(&Value::U64(7), &Kind::Uint64), "7");
    assert_eq!(scalar_text(&Value::I32(-7), &Kind::Int32), "-7");
    assert_eq!(scalar_text(&Value::Bool(false), &Kind::Bool), "false");
    assert_eq!(
        scalar_text(&Value::String("\"".into()), &Kind::String),
        "\\\""
    );
}

/// Catches a list or map written where a single value is, which `write_field` never asks for;
/// it is written as `null`, never as a value of another field.
#[test]
fn a_list_or_map_as_a_single_value_is_null() {
    for value in [
        Value::List(vec![Value::I32(1)]),
        Value::Map(HashMap::from([(MapKey::I32(1), Value::I32(1))])),
    ] {
        let mut out = String::new();
        write_single(&mut out, &value, &Kind::Int32, None);
        assert_eq!(out, "null");
    }
}
