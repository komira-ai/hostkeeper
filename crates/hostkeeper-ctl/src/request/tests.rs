//! The request built from flags, the request file and secrets, on the fixture protos.

use std::collections::HashMap;

use prost::Message as _;
use prost_reflect::{DynamicMessage, MapKey, Value};

use super::{BuiltRequest, build, read_input};
use crate::Failure;
use crate::testutil::{Broken, file, fixture, message, pool, scratch};
use crate::tree;

/// The request `ctl widgets <rpc> <args>` builds, with `stdin`.
fn built(rpc: &str, args: &[&str], stdin: &[u8]) -> Result<BuiltRequest, Failure> {
    let cli = tree::build(fixture::COMMANDS);
    let all: Vec<&str> = ["ctl", "widgets", rpc]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    let matches = cli.try_get_matches_from(all).expect("the arguments parse");
    let leaf = matches
        .subcommand_matches("widgets")
        .and_then(|w| w.subcommand_matches(rpc))
        .expect("the leaf");
    let command = fixture::COMMANDS
        .iter()
        .find(|c| c.method_command == rpc)
        .expect("the command");
    let mut input = stdin;
    build(command, leaf, &pool(), &mut input)
}

/// The usage error a request gives.
fn usage(rpc: &str, args: &[&str], stdin: &[u8]) -> String {
    match built(rpc, args, stdin) {
        Err(Failure::Usage(message)) => message,
        other => panic!("not a usage error: {other:?}"),
    }
}

/// Catches a flag sent with the wrong wire type, a repetition lost, an enum sent by name or as
/// another number, a map entry lost, and a number parsed as another width.
#[test]
fn scalar_repeated_enum_and_map_fields_become_flags_of_the_right_type() {
    let request = built(
        "make",
        &[
            "--name",
            "w",
            "--i32",
            "-1",
            "--i64",
            "-2",
            "--u32",
            "3",
            "--u64",
            "4",
            "--s32",
            "-5",
            "--s64",
            "-6",
            "--f32",
            "7",
            "--f64",
            "8",
            "--sf32",
            "-9",
            "--sf64",
            "-10",
            "--ratio",
            "1.5",
            "--weight",
            "2.5",
            "--flag",
            "true",
            "--maybe",
            "false",
            "--colour",
            "dark_blue",
            "--colours",
            "red",
            "--colours",
            "dark_blue",
            "--tags",
            "a",
            "--tags",
            "b",
            "--labels",
            "k=v=w",
            "--longs",
            "1",
            "--longs",
            "-2",
            "--node-id",
            "n1",
        ],
        b"",
    )
    .expect("builds");
    let desc = message("Widget");
    let mut expected = DynamicMessage::new(desc);
    let set = |m: &mut DynamicMessage, name: &str, value: Value| m.set_field_by_name(name, value);
    set(&mut expected, "name", Value::String("w".into()));
    set(&mut expected, "i32", Value::I32(-1));
    set(&mut expected, "i64", Value::I64(-2));
    set(&mut expected, "u32", Value::U32(3));
    set(&mut expected, "u64", Value::U64(4));
    set(&mut expected, "s32", Value::I32(-5));
    set(&mut expected, "s64", Value::I64(-6));
    set(&mut expected, "f32", Value::U32(7));
    set(&mut expected, "f64", Value::U64(8));
    set(&mut expected, "sf32", Value::I32(-9));
    set(&mut expected, "sf64", Value::I64(-10));
    set(&mut expected, "ratio", Value::F32(1.5));
    set(&mut expected, "weight", Value::F64(2.5));
    set(&mut expected, "flag", Value::Bool(true));
    set(&mut expected, "maybe", Value::Bool(false));
    set(&mut expected, "colour", Value::EnumNumber(2));
    set(
        &mut expected,
        "colours",
        Value::List(vec![Value::EnumNumber(1), Value::EnumNumber(2)]),
    );
    set(
        &mut expected,
        "tags",
        Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
    );
    set(
        &mut expected,
        "labels",
        Value::Map(HashMap::from([(
            MapKey::String("k".into()),
            Value::String("v=w".into()),
        )])),
    );
    set(
        &mut expected,
        "longs",
        Value::List(vec![Value::I64(1), Value::I64(-2)]),
    );
    set(&mut expected, "node_id", Value::String("n1".into()));
    assert_eq!(request.message(), &expected);
    assert_eq!(request.encode(), expected.encode_to_vec());
    // The enum is on the wire as its number: field 16, varint 2.
    assert!(request.encode().windows(3).any(|w| w == [0x80, 0x01, 0x02]));
}

/// Catches an absent `optional bool` sent as false, and an explicit false dropped.
#[test]
fn bool_flag_distinguishes_absent_from_false() {
    let desc = message("Widget");
    let maybe = desc.get_field_by_name("maybe").expect("maybe");
    let absent = built("make", &[], b"").expect("builds");
    assert!(!absent.message().has_field(&maybe));
    assert!(absent.encode().is_empty());
    let explicit = built("make", &["--maybe", "false"], b"").expect("builds");
    assert!(explicit.message().has_field(&maybe));
    // Field 15, varint 0.
    assert_eq!(explicit.encode().to_vec(), vec![0x78, 0x00]);
    let yes = built("make", &["--maybe", "true"], b"").expect("builds");
    assert_eq!(yes.encode().to_vec(), vec![0x78, 0x01]);
}

/// Catches `unspecified`, a full value name or an unknown name taken for an enum, and an error
/// that does not list the valid values.
#[test]
fn enum_flag_refuses_unspecified_and_unknown_names_and_lists_valid_ones() {
    let cli = tree::build(fixture::COMMANDS);
    for bad in ["unspecified", "purple", "COLOUR_RED", "Red"] {
        let refused = cli
            .clone()
            .try_get_matches_from(["ctl", "widgets", "make", "--colour", bad])
            .expect_err(bad);
        let text = refused.to_string();
        assert!(
            text.contains(&format!("invalid value '{bad}' for '--colour <VALUE>'")),
            "{text}"
        );
        assert!(text.contains("[possible values: red, dark_blue]"), "{text}");
    }
    let red = built("make", &["--colour", "red"], b"").expect("builds");
    assert_eq!(
        red.message()
            .get_field_by_name("colour")
            .expect("colour")
            .as_enum_number(),
        Some(1)
    );
}

/// Catches a number of the wrong width or sign taken, and an error that does not name the flag
/// and the kind it takes.
#[test]
fn a_flag_that_is_not_its_kind_is_refused() {
    let cases = [
        ("--i32", "2147483648", "a 32-bit signed integer"),
        ("--s32", "x", "a 32-bit signed integer"),
        ("--i64", "1.5", "a 64-bit signed integer"),
        ("--u32", "-1", "a 32-bit unsigned integer"),
        ("--u64", "18446744073709551616", "a 64-bit unsigned integer"),
        ("--ratio", "fast", "a number"),
        ("--weight", "1,5", "a number"),
    ];
    for (flag, value, kind) in cases {
        assert_eq!(
            usage("make", &[flag, value], b""),
            format!("{flag} takes {kind}, not {value:?}"),
            "{flag}"
        );
    }
    assert_eq!(
        usage("make", &["--longs", "1", "--longs", "x"], b""),
        "--longs takes a 64-bit signed integer, not \"x\""
    );
    assert_eq!(
        usage("make", &["--labels", "novalue"], b""),
        "--labels takes key=value, not \"novalue\""
    );
    assert_eq!(
        usage("make", &["--labels", "k=1", "--labels", "k=2"], b""),
        "--labels names the key \"k\" twice"
    );
    let empty_key = built("make", &["--labels", "=v"], b"").expect("an empty key is a key");
    assert!(empty_key.encode().len() > 2);
}

/// Catches a message, oneof, bytes or other map field given a flag, and the request file not
/// setting them.
#[test]
fn nested_message_and_oneof_fields_have_no_flag_and_come_through_request_file() {
    let cli = tree::build(fixture::COMMANDS);
    let make = cli
        .find_subcommand("widgets")
        .and_then(|w| w.find_subcommand("make"))
        .expect("make");
    let longs: Vec<&str> = make.get_arguments().filter_map(|a| a.get_long()).collect();
    for absent in [
        "inner",
        "round",
        "square",
        "blob",
        "counts",
        "inners",
        "by-name",
        "by-number",
        "by-truth",
        "by-long",
        "shape",
    ] {
        assert!(!longs.contains(&absent), "--{absent}");
    }
    let json = br#"{"inner": {"note": "x"}, "round": "r", "blob": "AQI=", "counts": {"a": "3"}}"#;
    let request = built("make", &["--request", "-", "--name", "n"], json).expect("builds");
    let expected = crate::testutil::from_json(
        "Widget",
        r#"{"name": "n", "inner": {"note": "x"}, "round": "r", "blob": "AQI=", "counts": {"a": "3"}}"#,
    );
    assert_eq!(request.message(), &expected);
    let dir = scratch("request-file");
    let path = file(&dir, "square.json", br#"{"square": {"count": "2"}}"#);
    let from_file = built("make", &["--request", &path], b"").expect("builds");
    assert_eq!(
        from_file.message(),
        &crate::testutil::from_json("Widget", r#"{"square": {"count": "2"}}"#)
    );
}

/// Catches the request file silently winning over a flag, or the flag over the file, by the
/// field's name or by its JSON name.
#[test]
fn a_flag_and_the_request_file_naming_the_same_field_is_a_usage_error() {
    assert_eq!(
        usage(
            "make",
            &["--request", "-", "--name", "b"],
            br#"{"name": "a"}"#
        ),
        "--name and the request file both set `name`"
    );
    assert_eq!(
        usage(
            "make",
            &["--request", "-", "--node-id", "b"],
            br#"{"nodeId": "a"}"#
        ),
        "--node-id and the request file both set `node_id`"
    );
    assert_eq!(
        usage(
            "make",
            &["--request", "-", "--node-id", "b"],
            br#"{"node_id": "a"}"#
        ),
        "--node-id and the request file both set `node_id`"
    );
    // Even a default in the file names the field.
    assert_eq!(
        usage(
            "make",
            &["--request", "-", "--flag", "true"],
            br#"{"flag": false}"#
        ),
        "--flag and the request file both set `flag`"
    );
    let dir = scratch("conflict");
    let secret = file(&dir, "password", b"p");
    assert_eq!(
        usage(
            "login",
            &["--request", "-", "--password-file", &secret],
            br#"{"password": "q"}"#
        ),
        "--password-file and the request file both set `password`"
    );
}

/// Catches a request file that is not canonical JSON of the request accepted, and an error that
/// shows what the file holds (it may hold a secret).
#[test]
fn a_bad_request_file_is_a_usage_error_that_shows_nothing_of_it() {
    let not_canonical = "the request file is not canonical JSON of hostkeeper.fixture.v1.LoginRequest (line 1, column ";
    let cases: &[(&[u8], &str)] = &[
        (b"[1]", "the request file is not a JSON object"),
        (
            b"{\"name\": \"s3cr3t\"",
            "the request file is not JSON (line 1, column ",
        ),
        (b"{\"password\": 12345}", not_canonical),
        (b"{\"s3cr3t\": 1}", not_canonical),
    ];
    for (json, error) in cases {
        let message = usage("login", &["--request", "-"], json);
        assert!(message.starts_with(error), "{message}");
        assert!(!message.contains("s3cr3t") && !message.contains("12345"));
    }
    let dir = scratch("unreadable");
    let gone = dir.path().join("gone.json");
    let gone = gone.to_str().expect("UTF-8");
    let message = usage("login", &["--request", gone], b"");
    assert!(
        message.starts_with(&format!("cannot read the request from {gone}: ")),
        "{message}"
    );
}

/// Catches a secret read with more than one trailing newline dropped (or none), a string secret
/// that is not UTF-8 accepted, and bytes secrets read as text.
#[test]
fn a_secret_is_read_from_its_file_or_stdin() {
    let dir = scratch("secrets");
    let password = file(&dir, "password", b"pass\n\n");
    let key = file(&dir, "key", b"\xff\x00\n");
    let request = built(
        "login",
        &["--password-file", &password, "--key-file", &key],
        b"",
    )
    .expect("builds");
    let m = request.message();
    assert_eq!(
        m.get_field_by_name("password").expect("p").as_str(),
        Some("pass\n")
    );
    assert_eq!(
        m.get_field_by_name("key")
            .expect("k")
            .as_bytes()
            .map(|b| b.to_vec()),
        Some(vec![0xff, 0x00])
    );
    let from_stdin = built("login", &["--key-stdin"], b"raw").expect("builds");
    assert_eq!(
        from_stdin
            .message()
            .get_field_by_name("key")
            .expect("k")
            .as_bytes()
            .map(|b| b.to_vec()),
        Some(b"raw".to_vec())
    );
    let bad = file(&dir, "bad", b"\xff");
    assert_eq!(
        usage("login", &["--password-file", &bad], b""),
        "`password` is not UTF-8 text"
    );
    let gone = dir.path().join("gone");
    let gone = gone.to_str().expect("UTF-8");
    let message = usage("login", &["--password-file", gone], b"");
    assert!(
        message.starts_with(&format!("cannot read `password` from {gone}: ")),
        "{message}"
    );
}

/// Catches a request's `Debug` that shows a secret, at the top or nested.
#[test]
fn the_debug_of_a_request_shows_no_secret() {
    let dir = scratch("debug");
    let password = file(&dir, "password", b"pa55w0rd-value");
    let request = built(
        "login",
        &["--user", "u", "--password-file", &password, "--key-stdin"],
        b"k3y-value",
    )
    .expect("builds");
    let shown = format!("{request:?}");
    assert!(
        !shown.contains("pa55w0rd") && !shown.contains("k3y"),
        "{shown}"
    );
    assert_eq!(
        shown,
        "BuiltRequest { type: \"hostkeeper.fixture.v1.LoginRequest\", json: \"{\\\"user\\\":\\\"u\\\"}\", \
         redacted: {\"hostkeeper.fixture.v1.LoginRequest.key\", \"hostkeeper.fixture.v1.LoginRequest.password\"} }"
    );
    let nested = built(
        "make",
        &["--request", "-"],
        br#"{"inner": {"token": "n3sted"}, "inners": [{"token": "l1st"}], "byName": {"k": {"token": "m4p"}}}"#,
    )
    .expect("builds");
    let shown = format!("{nested:?}");
    for secret in ["n3sted", "l1st", "m4p"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(
        shown.contains("redacted: {\"hostkeeper.fixture.v1.Inner.token\"}"),
        "{shown}"
    );
}

/// Catches a read error of stdin reported as an empty input.
#[test]
fn an_unreadable_stdin_is_a_usage_error() {
    match read_input(None, &mut Broken, "the request") {
        Err(Failure::Usage(message)) => assert_eq!(
            message,
            "cannot read the request from stdin: unreadable, for the test"
        ),
        other => panic!("{other:?}"),
    }
    let mut empty: &[u8] = b"\n";
    assert!(matches!(read_input(None, &mut empty, "x"), Ok(bytes) if bytes.is_empty()));
}
