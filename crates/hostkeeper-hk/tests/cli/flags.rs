//! Every kind of flag, a secret field and `--request`, as the request body the server receives,
//! read back as the fixture's message; and every usage error of a method's flags, which ends the
//! run before any call.

use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, ReflectMessage as _, Value,
};

use crate::rig::{FIXTURE_SET, Opts, Rig, Server, call};

/// The body the server received in its only call.
fn body(server: &Server) -> String {
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    seen[0].text()
}

/// `json` read as the fixture's message `name`, refusing a field it does not have.
fn message(name: &str, json: &str) -> DynamicMessage {
    let pool = DescriptorPool::decode(FIXTURE_SET).expect("the fixture's descriptors");
    let desc = pool.get_message_by_name(name).expect("a fixture message");
    let mut de = serde_json::Deserializer::from_str(json);
    let message =
        DynamicMessage::deserialize_with_options(desc, &mut de, &DeserializeOptions::new())
            .unwrap_or_else(|e| panic!("{json} is not a {name}: {e}"));
    de.end().expect("one document");
    message
}

/// The value of the field `field` of `message`.
fn field(message: &DynamicMessage, field: &str) -> Value {
    let desc = message
        .descriptor()
        .get_field_by_name(field)
        .expect("a field");
    assert!(message.has_field(&desc), "{field} is set");
    message.get_field(&desc).into_owned()
}

/// Catches a kind written in a form proto3 JSON does not read (a 64-bit integer as a number, an
/// enum by its flag spelling, a float the gateway refuses), a value out of its flag's order, a
/// presence lost (`--maybe false`, `--maybe-count 0`), a repeated flag or a map entry dropped,
/// and a string not escaped: the body is exact, and reads back as the fixture's `Widget`.
#[test]
fn every_flag_kind_reaches_the_request_as_its_proto3_json() {
    let rig = Rig::new("flag-kinds");
    let server = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &[
            "widgets",
            "make",
            "--name",
            "a \"q\" \\ \u{e9}\u{7}",
            "--i32",
            "-2147483648",
            "--i64=-9223372036854775808",
            "--u32",
            "4294967295",
            "--u64",
            "18446744073709551615",
            "--s32",
            "007",
            "--s64",
            "-1",
            "--f32",
            "0",
            "--f64",
            "18446744073709551615",
            "--sf32",
            "2147483647",
            "--sf64",
            "9223372036854775807",
            "--ratio",
            "1.5",
            "--weight=-2e3",
            "--flag",
            "false",
            "--maybe",
            "false",
            "--colour",
            "dark_blue",
            "--colours",
            "red",
            "--colours",
            "dark_blue",
            "--tags",
            "x",
            "--tags",
            "y z",
            "--labels",
            "a=1",
            "--labels",
            "b==2",
            "--longs",
            "-0",
            "--node-id",
            "n1",
            "--maybe-count",
            "0",
            "--weights",
            "NaN",
            "--weights",
            "-Infinity",
            "--weights",
            ".5",
            "--flags",
            "true",
            "--flags",
            "false",
        ],
    ));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{}\n", "")
    );
    let json = body(&server);
    assert_eq!(
        json,
        "{\"name\":\"a \\\"q\\\" \\\\ \u{e9}\\u0007\",\"i32\":-2147483648,\
         \"i64\":\"-9223372036854775808\",\"u32\":4294967295,\"u64\":\"18446744073709551615\",\
         \"s32\":7,\"s64\":\"-1\",\"f32\":0,\"f64\":\"18446744073709551615\",\
         \"sf32\":2147483647,\"sf64\":\"9223372036854775807\",\"ratio\":\"1.5\",\
         \"weight\":\"-2e3\",\"flag\":false,\"maybe\":false,\"colour\":\"COLOUR_DARK_BLUE\",\
         \"colours\":[\"COLOUR_RED\",\"COLOUR_DARK_BLUE\"],\"tags\":[\"x\",\"y z\"],\
         \"labels\":{\"a\":\"1\",\"b\":\"=2\"},\"longs\":[\"0\"],\"nodeId\":\"n1\",\
         \"maybeCount\":0,\"weights\":[\"NaN\",\"-Infinity\",\".5\"],\"flags\":[true,false]}"
    );
    let widget = message("hostkeeper.fixture.v1.Widget", &json);
    assert_eq!(
        field(&widget, "name"),
        Value::String("a \"q\" \\ \u{e9}\u{7}".to_owned())
    );
    assert_eq!(field(&widget, "i32"), Value::I32(i32::MIN));
    assert_eq!(field(&widget, "i64"), Value::I64(i64::MIN));
    assert_eq!(field(&widget, "u64"), Value::U64(u64::MAX));
    assert_eq!(field(&widget, "s32"), Value::I32(7));
    assert_eq!(field(&widget, "maybe"), Value::Bool(false));
    assert_eq!(field(&widget, "maybe_count"), Value::I32(0));
    assert_eq!(field(&widget, "colour"), Value::EnumNumber(2));
    assert_eq!(field(&widget, "weight"), Value::F64(-2000.0));
    assert_eq!(field(&widget, "node_id"), Value::String("n1".to_owned()));
    let Value::List(weights) = field(&widget, "weights") else {
        panic!("weights is a list")
    };
    assert!(matches!(weights[0], Value::F64(w) if w.is_nan()));
    assert_eq!(
        weights[1..],
        [Value::F64(f64::NEG_INFINITY), Value::F64(0.5)]
    );
}

/// Catches a flag value the script would pass on unchecked (a number out of its kind's range,
/// a bool or enum spelled otherwise, a map entry with no `=` or a key twice, a singular flag
/// twice, a flag with no value, a flag the method does not have, a word that is no flag): each
/// is a usage error, exit 2, said on stderr exactly, before any program runs.
#[test]
fn a_flag_value_the_field_cannot_take_is_a_usage_error_before_any_call() {
    let rig = Rig::new("flag-errors");
    let server = Server::canned(&rig.pki, 200, "{}");
    let cases: [(&[&str], &str); 22] = [
        (
            &["--i32", "2147483648"],
            "--i32 takes a 32-bit signed integer",
        ),
        (
            &["--i32", "-2147483649"],
            "--i32 takes a 32-bit signed integer",
        ),
        (&["--i32", "1x"], "--i32 takes a 32-bit signed integer"),
        (&["--i32", "-"], "--i32 takes a 32-bit signed integer"),
        (&["--u32", "-1"], "--u32 takes a 32-bit unsigned integer"),
        (
            &["--u32", "4294967296"],
            "--u32 takes a 32-bit unsigned integer",
        ),
        (
            &["--u64", "18446744073709551616"],
            "--u64 takes a 64-bit unsigned integer",
        ),
        (
            &["--i64", "-9223372036854775809"],
            "--i64 takes a 64-bit signed integer",
        ),
        (
            &["--i64", "9223372036854775808"],
            "--i64 takes a 64-bit signed integer",
        ),
        (
            &["--maybe-count="],
            "--maybe-count takes a 32-bit signed integer",
        ),
        (&["--flag", "yes"], "--flag takes true or false"),
        (
            &["--colour", "unspecified"],
            "--colour takes one of: red, dark_blue",
        ),
        (&["--ratio", "1e"], "--ratio takes a number"),
        (&["--labels", "novalue"], "--labels takes key=value"),
        (
            &["--labels", "a=1", "--labels", "a=2"],
            "--labels names the key \"a\" twice",
        ),
        (&["--name", "a", "--name", "b"], "--name is given twice"),
        (&["--name"], "--name needs a value"),
        (&["--name", "--i32", "1"], "--name needs a value"),
        (&["--nope", "1"], "widgets make has no flag --nope"),
        (&["--blob=x"], "widgets make has no flag --blob"),
        (
            &["stray"],
            "widgets make takes no argument; it has flags only",
        ),
        (
            &["--request", "a", "--request", "b"],
            "--request is given twice",
        ),
    ];
    for (words, why) in cases {
        let mut all = vec!["widgets", "make"];
        all.extend(words);
        let ran = rig.fx(&call(&rig, server.port, &all));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (2, "", format!("hk: {why}\n").as_str()),
            "{words:?}"
        );
    }
    assert!(server.seen().is_empty(), "{:#?}", server.seen());
}

/// Catches a secret put in the body unescaped, a bytes secret not base64 (or with its trailing
/// newline kept, or a padding wrong), a secret read twice or from a file and stdin at once, and a
/// secret file that cannot be read passing silently.
#[test]
fn a_secret_field_is_read_from_its_file_or_stdin_into_the_body() {
    let rig = Rig::new("secret-field");
    let server = Server::canned(&rig.pki, 200, "{}");
    let password = rig.put("password", b"pa\tss\"w\x01rd\n", 0o600);
    let key = rig.put("key", b"k\x00\xff\n", 0o600);
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &[
            "widgets",
            "login",
            "--user",
            "u",
            "--password-file",
            &password,
            "--key-file",
            &key,
        ],
    ));
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let json = body(&server);
    assert_eq!(
        json,
        "{\"user\":\"u\",\"password\":\"pa\\u0009ss\\\"w\\u0001rd\",\"key\":\"awD/\"}"
    );
    let login = message("hostkeeper.fixture.v1.LoginRequest", &json);
    assert_eq!(
        field(&login, "password"),
        Value::String("pa\tss\"w\x01rd".to_owned())
    );
    assert_eq!(
        field(&login, "key"),
        Value::Bytes(vec![b'k', 0, 0xff].into())
    );

    // From stdin, each padding of base64.
    for (input, encoded) in [
        (&b"a"[..], "YQ=="),
        (b"ab", "YWI="),
        (b"abc", "YWJj"),
        (b"abcd\n", "YWJjZA=="),
        (b"\n", ""),
    ] {
        let server = Server::canned(&rig.pki, 200, "{}");
        let ran = rig.run(
            Opts::fixture().stdin(input),
            &call(&rig, server.port, &["widgets", "login", "--key-stdin"]),
        );
        assert_eq!(ran.code, 0, "{ran:?}");
        assert_eq!(body(&server), format!("{{\"key\":\"{encoded}\"}}"));
    }
    let server = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.run(
        Opts::fixture().stdin(b"from stdin\n"),
        &call(&rig, server.port, &["widgets", "login", "--password-stdin"]),
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(body(&server), "{\"password\":\"from stdin\"}");

    let server = Server::canned(&rig.pki, 200, "{}");
    let nul = rig.put("nul", b"a\x00b", 0o600);
    let missing = rig.path("missing").display().to_string();
    let cases: [(Vec<&str>, &[u8], String); 8] = [
        (
            vec!["--password-file", &password, "--password-stdin"],
            b"",
            "--password-file and --password-stdin: give one, once".to_owned(),
        ),
        (
            vec!["--password-file", &password, "--password-file", &password],
            b"",
            "--password-file and --password-stdin: give one, once".to_owned(),
        ),
        (
            vec!["--password-stdin", "--password-stdin"],
            b"",
            "--password-file and --password-stdin: give one, once".to_owned(),
        ),
        (
            vec!["--password-stdin", "--key-stdin"],
            b"",
            "--password-stdin and --key-stdin both read stdin; at most one input may".to_owned(),
        ),
        (
            vec!["--password-stdin=x"],
            b"",
            "--password-stdin takes no value".to_owned(),
        ),
        (
            vec!["--password-file", &missing],
            b"",
            format!("cannot read `password` from {missing}"),
        ),
        (
            vec!["--key-file", &missing],
            b"",
            format!("cannot read `key` from {missing}"),
        ),
        (
            vec!["--password-file", &nul],
            b"",
            "`password` holds a NUL byte".to_owned(),
        ),
    ];
    for (words, stdin, why) in cases {
        let mut all = vec!["widgets", "login"];
        all.extend(words.iter().copied());
        let ran = rig.run(Opts::fixture().stdin(stdin), &call(&rig, server.port, &all));
        assert_eq!(
            (ran.code, ran.stderr.clone()),
            (2, format!("hk: {why}\n")),
            "{words:?}"
        );
    }
    assert!(server.seen().is_empty());
}

/// Catches a request file whose fields are dropped or overridden by a flag, a field the file
/// names twice (by its proto and its JSON name) or a flag sets again passing as the last one
/// wins, a file that is not one JSON object passing, and `--request -` not reading stdin.
#[test]
fn the_request_file_is_the_body_the_flags_add_to_and_a_field_named_twice_is_refused() {
    let rig = Rig::new("request-file");
    let server = Server::canned(&rig.pki, 200, "{}");
    let request = rig.put(
        "request.json",
        b" {\"inner\": {\"note\": \"n\", \"count\": \"2\"}, \"round\":\"r\" }\n",
        0o644,
    );
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &["widgets", "make", "--request", &request, "--name", "w"],
    ));
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let json = body(&server);
    assert_eq!(
        json,
        "{\"inner\": {\"note\": \"n\", \"count\": \"2\"}, \"round\":\"r\",\"name\":\"w\"}"
    );
    let widget = message("hostkeeper.fixture.v1.Widget", &json);
    assert_eq!(field(&widget, "round"), Value::String("r".to_owned()));
    assert_eq!(field(&widget, "name"), Value::String("w".to_owned()));

    // From stdin, alone, and an empty object with flags.
    for (stdin, words, want) in [
        (&b"{\"name\":\"s\"}"[..], vec![], "{\"name\":\"s\"}"),
        (b"{ }", vec!["--i32", "1"], "{\"i32\":1}"),
        (b"{}", vec![], "{}"),
    ] {
        let server = Server::canned(&rig.pki, 200, "{}");
        let mut all = vec!["widgets", "make", "--request", "-"];
        all.extend(words);
        let ran = rig.run(Opts::fixture().stdin(stdin), &call(&rig, server.port, &all));
        assert_eq!(ran.code, 0, "{ran:?}");
        assert_eq!(body(&server), want);
    }

    let server = Server::canned(&rig.pki, 200, "{}");
    let cases: [(&[u8], &[&str], &str); 12] = [
        (
            b"{\"name\":\"a\"}",
            &["--name", "b"],
            "--name and the request file both set `name`",
        ),
        (
            b"{\"nodeId\":\"a\"}",
            &["--node-id", "b"],
            "--node-id and the request file both set `node_id`",
        ),
        (
            b"{\"node_id\":\"a\",\"nodeId\":\"b\"}",
            &[],
            "the request file names `node_id` twice, by its proto name and its JSON name",
        ),
        (
            b"{\"name\":\"a\",\"name\":\"b\"}",
            &[],
            "the request file names `name` twice, by its proto name and its JSON name",
        ),
        (b"[1]", &[], "the request file is not a JSON object"),
        (b"{} x", &[], "the request file is not a JSON object"),
        (b"{\"a\":1", &[], "the request file is not a JSON object"),
        (b"", &[], "the request file is not a JSON object"),
        (b"{:1}", &[], "the request file is not a JSON object"),
        (
            b"{\"a\":1, :2}",
            &[],
            "the request file is not a JSON object",
        ),
        (
            b"{\"na\\u006de\":\"a\"}",
            &[],
            "the request file has a key with an escape in it; spell each key plainly",
        ),
        (b"{\"a\":\"x\x00\"}", &[], "the request holds a NUL byte"),
    ];
    for (text, words, why) in cases {
        let file = rig.put("bad.json", text, 0o644);
        let mut all = vec!["widgets", "make", "--request", &file];
        all.extend(words);
        let ran = rig.fx(&call(&rig, server.port, &all));
        assert_eq!(
            (ran.code, ran.stderr.clone()),
            (2, format!("hk: {why}\n")),
            "{}",
            String::from_utf8_lossy(text)
        );
    }
    assert!(server.seen().is_empty());
    // A secret flag named by the file too names its file flag.
    let file = rig.put("login.json", b"{\"password\":\"p\"}", 0o644);
    let password = rig.put("password", b"q", 0o600);
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &[
            "widgets",
            "login",
            "--request",
            &file,
            "--password-file",
            &password,
        ],
    ));
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: --password-file and the request file both set `password`\n"
        )
    );
    let missing = rig.path("missing").display().to_string();
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &["widgets", "make", "--request", &missing],
    ));
    assert_eq!(
        (ran.code, ran.stderr),
        (2, format!("hk: cannot read the request from {missing}\n"))
    );
    assert!(server.seen().is_empty());
}
