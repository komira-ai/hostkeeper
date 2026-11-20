//! The parts of `hk` that are functions of their input: how text becomes JSON, how a request
//! file is scanned, what the call's deadline is, and what an endpoint, a refusal and a reason
//! are. The behaviour of whole command lines is in tests/cli.

use hostkeeper_proto::ctl::{COMMANDS, EnumValue, Field, FieldKind, Input, Label};

use crate::body::scan_object;
use crate::call::{is_endpoint, is_reason, percent, refusal, total_secs};
use crate::encode::{base64, encode, json_text, seconds, trim, visible, without_newline};
use crate::table::{
    EXIT_CODES, MEANINGS, enc_of, exit_of, field_flags, is_hold, json_name, shape, value_name,
};

/// A field of the tests: a string flag named `name`.
const fn field(name: &'static str, flag: &'static str, kind: FieldKind) -> Field {
    Field {
        name,
        flag,
        kind,
        type_name: "",
        label: Label::Singular,
        input: Input::Flag,
        secret: false,
        enum_values: &[],
        doc: "A field.",
    }
}

/// `encode` of `text` for `field` as text, or the usage error's message.
fn encoded(field: &Field, text: &str) -> Result<String, String> {
    encode(field, text.as_bytes())
        .map(|json| String::from_utf8_lossy(&json).into_owned())
        .map_err(|stop| match stop {
            crate::Stop::Usage(message) => String::from_utf8_lossy(&message).into_owned(),
            other => format!("{other:?}"),
        })
}

/// Proves: the deadline of a call is `--timeout-secs` plus the hold when the hold is a whole
/// number of seconds from 1 to 999999, with at most one pair of quotes around it (a 64-bit value
/// is written as a string), and `--timeout-secs` alone for anything else (zero, a negative, more
/// than 999999, a fraction, text, nothing).
#[test]
fn the_deadline_is_the_timeout_plus_a_hold_that_is_a_whole_number_of_seconds() {
    for (timeout, hold, total) in [
        (30, "", 30),
        (30, "1800", 1830),
        (5, "1800", 1805),
        (30, "\"60\"", 90),
        (30, "\"60", 90),
        (30, "60\"", 90),
        (30, "\"\"60\"\"", 30),
        (30, "999999", 1_000_029),
        (30, "1000000", 30),
        (30, "0", 30),
        (30, "007", 30),
        (30, "-1", 30),
        (30, "1.5", 30),
        (30, "1e3", 30),
        (30, " 5", 30),
        (30, "1,2", 30),
        (30, "x", 30),
        (999_999, "999999", 1_999_998),
    ] {
        assert_eq!(
            total_secs(timeout, hold.as_bytes()),
            total,
            "{timeout} {hold}"
        );
    }
}

/// Proves: an endpoint is `https://` and a host name or address of letters, digits, `.` and `-`
/// or an IPv6 address in brackets, then at most a port of 1 to 5 digits and one `/`; anything with
/// a path, a user, another scheme, an empty host, a port that is no number or a bracket left
/// open is not.
#[test]
fn an_endpoint_is_https_host_and_port_only() {
    for good in [
        "https://coordinator.example.com",
        "https://coordinator.example.com/",
        "https://127.0.0.1:8443",
        "https://127.0.0.1:1/",
        "https://[::1]",
        "https://[fe80::1]:443/",
        "https://a.example.com",
        "https://A-b.example.com:65535",
    ] {
        assert!(is_endpoint(good.as_bytes()), "{good}");
    }
    for bad in [
        "",
        "http://coordinator.example.com",
        "https://",
        "https:///",
        "https://coordinator.example.com//",
        "https://coordinator.example.com/path",
        "https://user@coordinator.example.com",
        "https://coordinator.example.com:",
        "https://coordinator.example.com:123456",
        "https://coordinator.example.com:80a",
        "https://coordinator_example.com",
        "https://[::1",
        "https://[]",
        "https://[::g]",
        "https://[::1]x",
        "https://[::1]:",
        " https://a.example.com",
        "https://a.example.com b",
    ] {
        assert!(!is_endpoint(bad.as_bytes()), "{bad:?}");
    }
}

/// Proves: the gateway's error body is read as `{"code":"<CAPITALS_>","reason":"<text>"}` with
/// those keys in that order and nothing after the brace, the reason as written, and any other
/// body is not one.
#[test]
fn a_refusal_is_the_gateways_error_body_and_nothing_else() {
    let some =
        |code: &'static str, reason: &'static str| Some((code.as_bytes(), reason.as_bytes()));
    assert_eq!(
        refusal(br#"{"code":"NOT_FOUND","reason":"x_1"}"#),
        some("NOT_FOUND", "x_1")
    );
    assert_eq!(refusal(br#"{"code":"A","reason":""}"#), some("A", ""));
    assert_eq!(
        refusal(br#"{"code":"A","reason":"a\"}b"}"#),
        some("A", "a\\\"}b")
    );
    for not in [
        &br#"{"code":"","reason":"x"}"#[..],
        br#"{"code":"a","reason":"x"}"#,
        br#"{"code":"A"}"#,
        br#"{"reason":"x","code":"A"}"#,
        br#"{"code":"A","reason":"x"} "#,
        br#"{"code":"A", "reason":"x"}"#,
        br#"{"code":"A","reason":"x"#,
        b"",
        b"<html>",
    ] {
        assert_eq!(refusal(not), None, "{}", String::from_utf8_lossy(not));
    }
}

/// Proves: a reason is printed only when it is 1 to 64 characters of lower case letters, digits
/// and `_`.
#[test]
fn a_reason_is_a_token_of_lower_case_digits_and_underscore() {
    assert!(is_reason(b"a"));
    assert!(is_reason(b"below_min_version_2"));
    assert!(is_reason("b".repeat(64).as_bytes()));
    assert!(!is_reason("b".repeat(65).as_bytes()));
    for not in ["", "A", "a b", "a-b", "a\\b", "a\"b", "\u{e9}", "a\n"] {
        assert!(!is_reason(not.as_bytes()), "{not:?}");
    }
}

/// Proves: an audience is percent-encoded byte by byte, upper case hex, keeping only letters,
/// digits, `.`, `_`, `~` and `-`.
#[test]
fn an_audience_is_percent_encoded_by_the_byte() {
    assert_eq!(percent(b""), "");
    assert_eq!(percent(b"Az09._~-"), "Az09._~-");
    assert_eq!(
        percent("a b&c#d/\u{e9}".as_bytes()),
        "a%20b%26c%23d%2F%C3%A9"
    );
    assert_eq!(percent(b"\xff\x00%"), "%FF%00%25");
}

/// Proves: a string is a JSON string with `"` and `\` escaped, the controls below 0x20 as
/// `\u00xx` in lower case (so a newline is `\u000a`), 0x7F and everything above as they are, even
/// bytes that are not UTF-8.
#[test]
fn a_string_is_escaped_as_the_script_escaped_it() {
    assert_eq!(json_text(b""), b"\"\"");
    assert_eq!(json_text(b"plain /~"), b"\"plain /~\"");
    assert_eq!(json_text(b"a\"b\\c"), b"\"a\\\"b\\\\c\"");
    assert_eq!(
        json_text(b"\n\t\r\x00\x1f\x07"),
        b"\"\\u000a\\u0009\\u000d\\u0000\\u001f\\u0007\""
    );
    assert_eq!(json_text(b"\x7f\xc3\xa9\xff"), b"\"\x7f\xc3\xa9\xff\"");
}

/// Proves: text is trimmed of space, tab, newline and carriage return alone; one trailing newline
/// (not two, not a carriage return) is dropped; visible text is 0x21 to 0x7E without a backslash
/// and not empty; whole seconds are 1 to 999999 with no sign, no leading zero and no space.
#[test]
fn text_is_trimmed_dropped_and_judged_as_the_script_did() {
    assert_eq!(trim(b" \t\r\nab c \n"), b"ab c");
    assert_eq!(trim(b"   "), b"");
    assert_eq!(trim(b""), b"");
    assert_eq!(trim(b"\x0ba\x0b"), b"\x0ba\x0b");
    assert_eq!(without_newline(b"a\n"), b"a");
    assert_eq!(without_newline(b"a\n\n"), b"a\n");
    assert_eq!(without_newline(b"a\r\n"), b"a\r");
    assert_eq!(without_newline(b"a"), b"a");
    assert!(visible(b"abc.DEF-123"));
    for not in [
        &b""[..],
        b"a b",
        b"a\\b",
        b"a\n",
        b"\xc3\xa9",
        b"a\x7f",
        b"\x01",
    ] {
        assert!(!visible(not), "{not:?}");
    }
    assert_eq!(seconds(b"1"), Some(1));
    assert_eq!(seconds(b"999999"), Some(999_999));
    assert_eq!(seconds(b"30"), Some(30));
    for not in [
        "", "0", "01", "1000000", "-1", "+1", " 1", "1 ", "1.0", "x", "9999999",
    ] {
        assert_eq!(seconds(not.as_bytes()), None, "{not:?}");
    }
}

/// Proves: the base64 of a secret is the standard alphabet with `=` padding.
#[test]
fn bytes_are_standard_base64_with_padding() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"a"), "YQ==");
    assert_eq!(base64(b"ab"), "YWI=");
    assert_eq!(base64(b"abc"), "YWJj");
    assert_eq!(base64(b"k\0\xff"), "awD/");
}

/// Proves: a float flag takes `NaN`, `Infinity`, `-Infinity` and a number as `strtod` writes it
/// (digits with an optional point and exponent, or a point and digits), and writes it into the body
/// as a string, as it was given; anything else, a sign `+`, a point alone, an exponent with no
/// digits, a space or a trailing newline is not a number.
#[test]
fn a_float_is_a_number_or_one_of_three_words() {
    let double = field("w", "w", FieldKind::Double);
    for good in [
        "0",
        "1.5",
        "-2e3",
        ".5",
        "1.",
        "-0",
        "1e5",
        "1E+5",
        "1e-5",
        "123.456e789",
        "NaN",
        "Infinity",
        "-Infinity",
        "-.5",
        "00.10",
    ] {
        assert_eq!(encoded(&double, good), Ok(format!("\"{good}\"")), "{good}");
    }
    for bad in [
        "",
        "+1",
        ".",
        "-",
        "-.",
        "1e",
        "1e+",
        "e5",
        "1.5.5",
        "1 ",
        " 1",
        "1\n",
        "nan",
        "inf",
        "Infinit",
        "+Infinity",
        "0x10",
        "1_0",
        "1,5",
        ".e5",
    ] {
        assert_eq!(
            encoded(&double, bad),
            Err("--w takes a number".to_owned()),
            "{bad:?}"
        );
    }
}

/// Proves: an integer flag is the digits with an optional `-`, leading zeros dropped, `-0` as 0
/// (even for an unsigned kind), a negative refused for an unsigned kind, and a magnitude over the
/// kind's range refused; a 32-bit kind is a JSON number and a 64-bit one a string; the message
/// names the kind.
#[test]
fn an_integer_is_judged_by_its_kind() {
    let kinds = [
        (
            FieldKind::Int32,
            "a 32-bit signed integer",
            "-2147483648",
            "2147483647",
        ),
        (
            FieldKind::Uint32,
            "a 32-bit unsigned integer",
            "0",
            "4294967295",
        ),
        (
            FieldKind::Int64,
            "a 64-bit signed integer",
            "-9223372036854775808",
            "9223372036854775807",
        ),
        (
            FieldKind::Uint64,
            "a 64-bit unsigned integer",
            "0",
            "18446744073709551615",
        ),
    ];
    for (kind, what, low, high) in kinds {
        let f = field("n", "n", kind);
        let quoted = matches!(kind, FieldKind::Int64 | FieldKind::Uint64);
        let json = |text: &str| {
            if quoted {
                format!("\"{text}\"")
            } else {
                text.to_owned()
            }
        };
        assert_eq!(encoded(&f, low), Ok(json(low)), "{what} {low}");
        assert_eq!(encoded(&f, high), Ok(json(high)), "{what} {high}");
        assert_eq!(encoded(&f, "0007"), Ok(json("7")));
        assert_eq!(encoded(&f, "0"), Ok(json("0")));
        assert_eq!(encoded(&f, "-0"), Ok(json("0")));
        assert_eq!(encoded(&f, "-000"), Ok(json("0")));
        assert_eq!(encoded(&f, "00"), Ok(json("0")));
        let refused = Err(format!("--n takes {what}"));
        for bad in [
            "", "-", "--1", "+1", "1x", "x", " 1", "1 ", "0x1", "1_0", "1.0", "\u{661}",
        ] {
            assert_eq!(encoded(&f, bad), refused, "{what} {bad:?}");
        }
        // One past the range, by digits and by length.
        let beyond = format!("{high}0");
        assert_eq!(encoded(&f, &beyond), refused, "{what} {beyond}");
        if !high.ends_with('0') {
            let up = (high.parse::<u128>().expect("a number") + 1).to_string();
            assert_eq!(encoded(&f, &up), refused, "{what} {up}");
        }
        if low == "0" {
            assert_eq!(encoded(&f, "-1"), refused);
        } else {
            let down = format!("-{}", (low[1..].parse::<u128>().expect("a number") + 1));
            assert_eq!(encoded(&f, &down), refused, "{what} {down}");
        }
    }
}

/// Proves: a bool is `true` or `false` as written, an enum is the full name of the value whose
/// flag spelling it is (naming the spellings when it is none), and a string is escaped.
#[test]
fn bools_enums_and_strings_are_encoded_by_their_kind() {
    static VALUES: [EnumValue; 2] = [
        EnumValue {
            flag: "red",
            name: "COLOUR_RED",
            number: 1,
        },
        EnumValue {
            flag: "dark_blue",
            name: "COLOUR_DARK_BLUE",
            number: 2,
        },
    ];
    let bool_field = field("b", "b", FieldKind::Bool);
    assert_eq!(encoded(&bool_field, "true"), Ok("true".to_owned()));
    assert_eq!(encoded(&bool_field, "false"), Ok("false".to_owned()));
    for bad in ["", "True", "yes", "1", "true "] {
        assert_eq!(
            encoded(&bool_field, bad),
            Err("--b takes true or false".to_owned())
        );
    }
    let mut colour = field("c", "c", FieldKind::Enum);
    colour.enum_values = &VALUES;
    assert_eq!(encoded(&colour, "red"), Ok("\"COLOUR_RED\"".to_owned()));
    assert_eq!(
        encoded(&colour, "dark_blue"),
        Ok("\"COLOUR_DARK_BLUE\"".to_owned())
    );
    for bad in ["", "RED", "unspecified", "COLOUR_RED"] {
        assert_eq!(
            encoded(&colour, bad),
            Err("--c takes one of: red, dark_blue".to_owned())
        );
    }
    let s = field("s", "s", FieldKind::String);
    assert_eq!(encoded(&s, "a\"b"), Ok("\"a\\\"b\"".to_owned()));
}

/// Proves: a request file's top level is scanned as the script scanned it: the keys as written
/// with their values' trimmed text and what is between the braces; a string, bracket or escape
/// inside a value does not end it; and what is not one object (an array, text after it, no
/// closing brace, a value with no key, a stray colon) is refused.
#[test]
fn a_request_file_is_scanned_for_its_top_level_keys() {
    let scanned =
        scan_object(b" \n{ \"a\": 1 , \"b\" :{\"c\":[1,2,\"}\"]}, \"d\":\"x,\\\"y\" }\r\n")
            .expect("an object");
    assert_eq!(scanned.keys, [b"a".to_vec(), b"b".to_vec(), b"d".to_vec()]);
    assert_eq!(
        scanned.values,
        [
            b"1".to_vec(),
            b"{\"c\":[1,2,\"}\"]}".to_vec(),
            b"\"x,\\\"y\"".to_vec()
        ]
    );
    assert_eq!(
        scanned.inner,
        b"\"a\": 1 , \"b\" :{\"c\":[1,2,\"}\"]}, \"d\":\"x,\\\"y\"".to_vec()
    );
    assert!(!scanned.key_escaped);
    let empty = scan_object(b"{ }").expect("an object");
    assert_eq!((empty.keys.len(), empty.inner.len()), (0, 0));
    // A key with no value, and a key that is an escape.
    let keyless = scan_object(b"{\"a\"}").expect("an object");
    assert_eq!(
        (keyless.keys, keyless.values),
        (vec![b"a".to_vec()], vec![Vec::new()])
    );
    assert!(
        scan_object(b"{\"na\\u006de\":1}")
            .expect("an object")
            .key_escaped
    );
    assert!(
        !scan_object(b"{\"a\":\"\\n\"}")
            .expect("an object")
            .key_escaped
    );
    // A comma with no value before it leaves the next string a value, not a key.
    let odd = scan_object(b"{\"a\",\"b\":1}").expect("an object");
    assert_eq!(odd.keys, [b"a".to_vec(), b"b".to_vec()]);
    for not in [
        &b""[..],
        b"   ",
        b"[1]",
        b"\"a\"",
        b"{} x",
        b"{}}",
        b"{\"a\":1",
        b"{",
        b"{:1}",
        b"{\"a\":1, :2}",
        b"{\"a\":1:2}",
        b"{\"a\":\"x}",
    ] {
        assert!(
            scan_object(not).is_none(),
            "{}",
            String::from_utf8_lossy(not)
        );
    }
}

/// Proves: the field's JSON name is lower camel case, kinds are encoded as 32- or 64-bit,
/// float, enum, bool or string (a kind that is no flag's as a 64-bit unsigned integer), and only
/// an integer flag named `wait_secs` holds the call.
#[test]
fn a_field_has_its_json_name_its_encoding_and_its_hold() {
    assert_eq!(json_name("node_id"), "nodeId");
    assert_eq!(json_name("a_b_c"), "aBC");
    assert_eq!(json_name("name"), "name");
    let kinds = [
        (FieldKind::Double, "Float"),
        (FieldKind::Float, "Float"),
        (FieldKind::Int32, "I32"),
        (FieldKind::Sint32, "I32"),
        (FieldKind::Sfixed32, "I32"),
        (FieldKind::Uint32, "U32"),
        (FieldKind::Fixed32, "U32"),
        (FieldKind::Int64, "I64"),
        (FieldKind::Sint64, "I64"),
        (FieldKind::Sfixed64, "I64"),
        (FieldKind::Uint64, "U64"),
        (FieldKind::Fixed64, "U64"),
        (FieldKind::Bool, "Bool"),
        (FieldKind::String, "String"),
        (FieldKind::Enum, "Enum"),
        (FieldKind::Bytes, "U64"),
        (FieldKind::Message, "U64"),
    ];
    for (kind, enc) in kinds {
        assert_eq!(format!("{:?}", enc_of(kind)), enc, "{kind:?}");
        let holds = matches!(enc, "I32" | "U32" | "I64" | "U64")
            && !matches!(kind, FieldKind::Bytes | FieldKind::Message);
        assert_eq!(
            is_hold(&field("wait_secs", "wait-secs", kind)),
            holds,
            "{kind:?}"
        );
    }
    let mut file_only = field("wait_secs", "wait-secs", FieldKind::Uint32);
    file_only.input = Input::RequestFile;
    assert!(!is_hold(&file_only));
    assert!(!is_hold(&field("wait", "wait", FieldKind::Uint32)));
}

/// Proves: help names a field's value and shape (a map, a repeated, an optional bool, a message
/// in a oneof), keeps its doc and tells to repeat a repeated flag; a field that comes only through
/// the request file has no flag, and a secret has a file flag and a stdin flag.
#[test]
fn a_fields_help_names_its_value_and_shape() {
    let mut f = field("labels", "labels", FieldKind::String);
    f.label = Label::Map;
    assert_eq!(value_name(&f), "KEY=VALUE");
    assert_eq!(shape(&f), "map");
    assert_eq!(
        field_flags(&f),
        [(
            "--labels KEY=VALUE".to_owned(),
            "A field. Repeat the flag for more.".to_owned()
        )]
    );
    f.label = Label::Repeated;
    f.kind = FieldKind::Int64;
    assert_eq!(value_name(&f), "INT64");
    assert_eq!(shape(&f), "repeated int64");
    f.kind = FieldKind::Bool;
    f.label = Label::Optional;
    assert_eq!(value_name(&f), "true|false");
    assert_eq!(shape(&f), "bool");
    f.kind = FieldKind::Enum;
    assert_eq!(value_name(&f), "VALUE");
    f.kind = FieldKind::Message;
    f.type_name = "x.Inner";
    f.label = Label::Oneof;
    assert_eq!(shape(&f), "x.Inner, in a oneof");
    f.label = Label::Singular;
    assert_eq!(shape(&f), "x.Inner");
    f.input = Input::RequestFile;
    assert_eq!(field_flags(&f), []);
    let mut secret = field("pass_word", "pass-word", FieldKind::String);
    secret.input = Input::SecretFile;
    assert_eq!(
        field_flags(&secret),
        [
            (
                "--pass-word-file PATH".to_owned(),
                "Read the secret `pass_word` from this file; one trailing newline is dropped. A field."
                    .to_owned()
            ),
            (
                "--pass-word-stdin".to_owned(),
                "Read the secret `pass_word` from stdin.".to_owned()
            )
        ]
    );
}

/// Proves: every code of the gateway has one exit code (17 names, `OK` the only 0, the rest 1 to
/// 3), the exit codes are 0 to 3 as the help says, and `OK` or a name that is no code is no
/// refusal.
#[test]
fn every_code_has_one_exit() {
    let names: std::collections::BTreeSet<&str> = EXIT_CODES.iter().map(|(n, _)| *n).collect();
    assert_eq!(names.len(), 17);
    assert_eq!(EXIT_CODES[0], ("OK", 0));
    assert!(EXIT_CODES[1..].iter().all(|(_, e)| (1..=3).contains(e)));
    assert_eq!(MEANINGS.map(|(c, _)| c), [0, 1, 2, 3]);
    assert_eq!(exit_of(b"INVALID_ARGUMENT"), Some(2));
    assert_eq!(exit_of(b"UNAUTHENTICATED"), Some(3));
    assert_eq!(exit_of(b"DATA_LOSS"), Some(1));
    assert_eq!(exit_of(b"OK"), None);
    assert_eq!(exit_of(b"NOPE"), None);
}

/// Proves: only the host API (`hostkeeper.agent.v<n>`) has no command.
#[test]
fn only_the_host_api_has_no_command() {
    let real: Vec<&str> = COMMANDS
        .iter()
        .filter(|c| !crate::is_served(c))
        .map(|c| c.package)
        .collect();
    assert!(!real.is_empty());
    assert!(real.iter().all(|p| *p == "hostkeeper.agent.v1"));
    assert!(COMMANDS.iter().any(crate::is_served));
    let mut agent = COMMANDS[0];
    for (package, served) in [
        ("hostkeeper.agent.v2", false),
        ("hostkeeper.agentx.v1", true),
        ("hostkeeper", true),
        ("hostkeeper.operator.v1", true),
    ] {
        agent.package = package;
        assert_eq!(crate::is_served(&agent), served, "{package}");
    }
}
