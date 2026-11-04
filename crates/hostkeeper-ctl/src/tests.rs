//! `ctl` end to end on the fixture table with a fake transport: the tree, what reaches the
//! network and what does not, the exit codes, the output and the secrets; and the tree of the
//! real protos.

use std::collections::BTreeSet;
use std::io::Write as _;

use bytes::Bytes;
use hostkeeper_proto::ctl::{COMMANDS, HAND_WRITTEN_LOCAL, Input, RESERVED_FLAGS};
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use tonic::{Code, Status};

use crate::testutil::{
    Broken, CALL, Fake, file, fixture, from_json, message, run_fixture, run_on, scratch, with_call,
};
use crate::transport::InMemory;
use crate::{Io, PeerIdentity, TransportKind, codes, exit, run_with, tree};

/// The leaf subcommands of a tree, as `service rpc`.
fn leaves(cli: &clap::Command) -> Vec<String> {
    cli.get_subcommands()
        .flat_map(|s| {
            s.get_subcommands()
                .map(move |r| format!("{} {}", s.get_name(), r.get_name()))
        })
        .collect()
}

/// The long flags of one leaf.
fn flags_of(cli: &clap::Command, service: &str, rpc: &str) -> BTreeSet<String> {
    cli.find_subcommand(service)
        .and_then(|s| s.find_subcommand(rpc))
        .expect("the leaf")
        .get_arguments()
        .filter_map(|a| a.get_long().map(str::to_owned))
        .collect()
}

/// Whether a package is one of a host's admin socket, `hostkeeper.local.v<n>`.
fn is_local(package: &str) -> bool {
    package.split('.').take(2).eq(["hostkeeper", "local"])
}

/// A CamelCase name in kebab case, written apart from the generator's.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Catches an RPC of the descriptors skipped, written twice, or named otherwise than its
/// descriptor's name in kebab case; and a tree that is not the descriptors' (a hand-written list).
#[test]
fn every_rpc_has_one_command() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let cli = tree::build(COMMANDS);
    let mut expected = Vec::new();
    for service in pool.services() {
        if is_local(service.package_name()) {
            continue;
        }
        for method in service.methods() {
            expected.push(format!(
                "{} {}",
                kebab(service.name()),
                kebab(method.name())
            ));
        }
    }
    let mut got = leaves(&cli);
    for want in &expected {
        assert_eq!(got.iter().filter(|g| *g == want).count(), 1, "{want}");
    }
    got.sort();
    expected.sort();
    assert_eq!(got, expected);
    assert!(got.contains(&"releases set-desired".to_owned()));
    assert!(got.contains(&"hosts set-labels".to_owned()));
    for command in COMMANDS {
        assert_eq!(
            command.path,
            format!(
                "/{}.{}/{}",
                command.package, command.service, command.method
            )
        );
    }
}

/// Catches the tree built from a fixed list instead of the table: a method that is in a fixture
/// proto and in no real one is a command, and calling it calls its path.
#[test]
fn a_new_rpc_in_a_fixture_proto_appears_without_a_hand_edit() {
    let cli = tree::build(fixture::COMMANDS);
    assert!(leaves(&cli).contains(&"widgets brand-new-method".to_owned()));
    assert!(!leaves(&tree::build(COMMANDS)).contains(&"widgets brand-new-method".to_owned()));
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let ran = run_fixture(&with_call(&["widgets", "brand-new-method"]), &fake, b"");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].path,
        "/hostkeeper.fixture.v1.Widgets/BrandNewMethod"
    );
    assert!(calls[0].request.is_empty());
}

/// Catches a local service given commands, or one neither generated nor listed.
#[test]
fn hand_written_local_verbs_are_listed() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let local: Vec<String> = pool
        .services()
        .filter(|s| is_local(s.package_name()))
        .map(|s| s.full_name().to_owned())
        .collect();
    assert!(
        local.contains(&"hostkeeper.local.v1.Admin".to_owned()),
        "{local:?}"
    );
    assert_eq!(HAND_WRITTEN_LOCAL, local.as_slice());
    for name in HAND_WRITTEN_LOCAL {
        assert!(
            COMMANDS
                .iter()
                .all(|c| !c.path.starts_with(&format!("/{name}/"))),
            "{name}"
        );
    }
    assert_eq!(fixture::HAND_WRITTEN_LOCAL, ["hostkeeper.local.v9.Admin"]);
    assert!(
        !leaves(&tree::build(fixture::COMMANDS))
            .iter()
            .any(|l| l.starts_with("admin "))
    );
}

/// Catches a secret of the real protos made a flag: no flag of any command is a secret field's
/// name, and every secret field is read from a file or stdin, or the request file.
#[test]
fn the_real_protos_expose_no_secret_as_a_flag() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let ext = pool
        .get_extension_by_name("hostkeeper.secret")
        .expect("declared");
    let secrets: BTreeSet<String> = pool
        .all_messages()
        .flat_map(|m| m.fields().collect::<Vec<_>>())
        .filter(|f| f.options().get_extension(&ext).as_ref() == &Value::Bool(true))
        .map(|f| f.name().replace('_', "-"))
        .collect();
    let cli = tree::build(COMMANDS);
    for command in COMMANDS {
        let flags = flags_of(&cli, command.service_command, command.method_command);
        for secret in &secrets {
            assert!(!flags.contains(secret), "{} --{secret}", command.path);
        }
        for field in command.fields.iter().filter(|f| f.secret) {
            assert_ne!(field.input, Input::Flag, "{}", field.name);
        }
    }
    // The same walk finds the fixture's secrets, so it is not empty by mistake.
    let fixture_cli = tree::build(fixture::COMMANDS);
    let login = flags_of(&fixture_cli, "widgets", "login");
    assert!(!login.contains("password") && login.contains("password-file"));
}

/// Catches a global flag that the generator does not keep from the fields, or one it keeps that
/// `ctl` does not define.
#[test]
fn the_global_flags_are_the_reserved_ones() {
    let mut globals: Vec<&str> = tree::GLOBALS.iter().map(|g| g.flag).collect();
    globals.extend(["help", "request"]);
    globals.sort_unstable();
    assert_eq!(globals, RESERVED_FLAGS);
    assert_eq!(fixture::RESERVED_FLAGS, RESERVED_FLAGS);
}

/// Catches `--help` or a usage error that makes a transport or a call: none of them needs the
/// network.
#[test]
fn ctl_help_and_usage_errors_make_no_network_call() {
    let memory = InMemory::new(PeerIdentity::Anonymous);
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let cases: &[(&[&str], u8)] = &[
        (&[], 2),
        (&["--help"], 0),
        (&["widgets"], 2),
        (&["widgets", "--help"], 0),
        (&["widgets", "make", "--help"], 0),
        (&["widgets", "make", "--colour", "purple"], 2),
        (&["widgets", "make"], 2),
        (
            &[
                "widgets",
                "make",
                "--endpoint",
                "https://coordinator.example",
            ],
            2,
        ),
        (
            &[
                "widgets",
                "make",
                "--endpoint",
                "http://coordinator.example",
                "--ca-file",
                "c",
            ],
            2,
        ),
        (&["nothing"], 2),
        (
            &[
                "widgets",
                "make",
                "--i32",
                "many",
                "--endpoint",
                "https://c.example",
                "--ca-file",
                "c",
            ],
            2,
        ),
    ];
    for (args, code) in cases {
        let ran = run_fixture(args, &memory, b"");
        assert_eq!(ran.code, *code, "{args:?}: {ran:?}");
        let ran = run_fixture(args, &fake, b"");
        assert_eq!(ran.code, *code, "{args:?}: {ran:?}");
    }
    assert_eq!(memory.calls(), 0);
    assert!(fake.targets().is_empty());
    assert!(fake.calls().is_empty());
}

/// Catches help on stderr or usage on stdout, and the help of a command losing the proto's
/// comments, the rules, or the list of request-file fields.
#[test]
fn help_goes_to_stdout_and_says_the_rules() {
    let fake = Fake::with(Ok(Bytes::new()));
    let root = run_fixture(&["--help"], &fake, b"");
    assert_eq!(root.stderr, "");
    assert!(root.stdout.contains("hostkeeper ctl"), "{}", root.stdout);
    assert!(
        root.stdout.contains("never `unspecified`"),
        "{}",
        root.stdout
    );
    assert!(
        root.stdout.contains("  3  the server refused the caller"),
        "{}",
        root.stdout
    );
    let make = run_fixture(&["widgets", "make", "--help"], &fake, b"");
    assert!(
        make.stdout
            .contains("Make a widget from every kind of field.")
    );
    assert!(
        make.stdout.contains("--node-id <STRING>"),
        "{}",
        make.stdout
    );
    assert!(
        make.stdout.contains("--maybe <true|false>"),
        "{}",
        make.stdout
    );
    assert!(
        make.stdout.contains("--labels <KEY=VALUE>"),
        "{}",
        make.stdout
    );
    assert!(
        make.stdout.contains("[possible values: red, dark_blue]"),
        "{}",
        make.stdout
    );
    assert!(
        make.stdout
            .contains("Through --request only:\n  inner (hostkeeper.fixture.v1.Inner)")
    );
    assert!(
        make.stdout.contains("  round (string, in a oneof)"),
        "{}",
        make.stdout
    );
    assert!(make.stdout.contains("  counts (map)"), "{}", make.stdout);
    assert!(
        make.stdout
            .contains("  inners (repeated hostkeeper.fixture.v1.Inner)")
    );
    let bare = run_fixture(&[], &fake, b"");
    assert_eq!(bare.stdout, "");
    assert!(
        bare.stderr.contains("Usage: hostkeeper ctl"),
        "{}",
        bare.stderr
    );
}

/// Catches the help of a call written to a stdout that fails being reported as success.
#[test]
fn a_failing_stdout_fails_help_and_output() {
    let fake = Fake::answering(&from_json("Widget", r#"{"name": "w"}"#));
    let mut stdin: &[u8] = b"";
    let mut stderr = Vec::new();
    let code = run_with(
        fixture::COMMANDS,
        fixture::DESCRIPTORS,
        ["ctl", "--help"],
        &fake,
        Io {
            stdin: &mut stdin,
            stdout: &mut Broken,
            stderr: &mut stderr,
        },
    );
    assert_eq!(code, 1);
    let code = run_with(
        fixture::COMMANDS,
        fixture::DESCRIPTORS,
        with_call(&["ctl", "widgets", "make"]),
        &fake,
        Io {
            stdin: &mut stdin,
            stdout: &mut Broken,
            stderr: &mut stderr,
        },
    );
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(stderr).expect("UTF-8"),
        "error: cannot write the response: broken pipe, for the test\n"
    );
    assert!(Broken.flush().is_err());
}

/// Catches a status read as another exit code, a refusal printed on stdout or with more than its
/// token, and a failure printed without its code.
#[test]
fn refusal_reason_token_is_printed_on_stderr_not_stdout() {
    for (code, name) in codes::ALL.iter().skip(1) {
        let fake = Fake::with(Err(Status::new(*code, "below_min_version")));
        let ran = run_fixture(&with_call(&["widgets", "make", "--name", "w"]), &fake, b"");
        assert_eq!(ran.code, exit::code(*code), "{name}");
        assert_eq!(ran.stdout, "", "{name}");
        let expected = match exit::code(*code) {
            2 | 3 => "refused: below_min_version\n".to_owned(),
            _ => format!("error: {name}: below_min_version\n"),
        };
        assert_eq!(ran.stderr, expected, "{name}");
    }
    let fake = Fake::with(Err(Status::new(Code::FailedPrecondition, "x\u{1b}[2Jy\n")));
    let ran = run_fixture(&with_call(&["widgets", "make"]), &fake, b"");
    assert_eq!(ran.stderr, "refused: x\\u{1b}[2Jy\\n\n");
}

/// Catches JSON output in serde's shape (field names as in the proto, numbers for 64-bit
/// integers and enums, defaults written), and text output that differs from it.
#[test]
fn output_json_is_proto3_canonical() {
    let reply = from_json(
        "Widget",
        r#"{"name": "w", "i64": "5000000000", "maybe": false, "colour": "COLOUR_DARK_BLUE",
            "tags": [], "nodeId": "n-1", "u32": 0}"#,
    );
    let fake = Fake::answering(&reply);
    let json = run_fixture(
        &with_call(&["widgets", "make", "--output", "json"]),
        &fake,
        b"",
    );
    assert_eq!(json.code, 0, "{json:?}");
    assert_eq!(
        json.stdout,
        "{\n  \"name\": \"w\",\n  \"i64\": \"5000000000\",\n  \"maybe\": false,\n  \
         \"colour\": \"COLOUR_DARK_BLUE\",\n  \"nodeId\": \"n-1\"\n}\n"
    );
    let text = run_fixture(&with_call(&["widgets", "make"]), &fake, b"");
    assert_eq!(
        text.stdout,
        "name: w\ni64: 5000000000\nmaybe: false\ncolour: COLOUR_DARK_BLUE\nnode_id: n-1\n"
    );
    let nested = from_json(
        "Widget",
        r#"{"tags": ["a", "b"], "labels": {"z": "1", "a": "2"}, "inner": {"note": "n"}}"#,
    );
    let ran = run_fixture(
        &with_call(&["widgets", "make"]),
        &Fake::answering(&nested),
        b"",
    );
    assert_eq!(
        ran.stdout,
        "tags:\n  [\n    \"a\",\n    \"b\"\n  ]\nlabels:\n  {\n    \"a\": \"2\",\n    \"z\": \"1\"\n  }\n\
         inner:\n  {\n    \"note\": \"n\"\n  }\n"
    );
}

/// Catches a secret of a response printed, in either output, at any depth, and the names of
/// those taken out not said.
#[test]
fn a_secret_of_a_response_is_never_printed() {
    let reply = from_json(
        "LoginResponse",
        r#"{"user": "u", "session": "s3cr3t-session", "inner": {"note": "n", "token": "s3cr3t-inner"},
            "inners": [{"token": "s3cr3t-list"}], "byName": {"k": {"token": "s3cr3t-map"}}}"#,
    );
    let fake = Fake::answering(&reply);
    for output in ["json", "text"] {
        let ran = run_fixture(
            &with_call(&["widgets", "login", "--output", output]),
            &fake,
            b"",
        );
        assert_eq!(ran.code, 0, "{ran:?}");
        assert!(!ran.stdout.contains("s3cr3t"), "{output}: {}", ran.stdout);
        assert!(
            ran.stdout.contains("\"note\": \"n\""),
            "{output}: {}",
            ran.stdout
        );
        assert_eq!(
            ran.stderr,
            "redacted: hostkeeper.fixture.v1.Inner.token\n\
             redacted: hostkeeper.fixture.v1.LoginResponse.session\n"
        );
    }
}

/// Catches a secret field given a flag, or its value reaching `--help`, a usage error, the
/// output or stderr; and the secret not reaching the request it belongs in.
#[test]
fn a_secret_field_is_never_a_flag_and_never_printed() {
    const SECRET: &str = "hunter2-very-secret";
    let dir = scratch("secret");
    let password = file(&dir, "password", format!("{SECRET}\n").as_bytes());
    let cli = tree::build(fixture::COMMANDS);
    let flags = flags_of(&cli, "widgets", "login");
    assert!(
        !flags.contains("password") && !flags.contains("key"),
        "{flags:?}"
    );
    for flag in ["password-file", "password-stdin", "key-file", "key-stdin"] {
        assert!(flags.contains(flag), "{flag}");
    }
    let reply = from_json(
        "LoginResponse",
        &format!(r#"{{"user": "u", "session": "{SECRET}"}}"#),
    );
    let fake = Fake::answering(&reply);
    let mut runs = Vec::new();
    for output in ["json", "text"] {
        let args = with_call(&[
            "widgets",
            "login",
            "--user",
            "u",
            "--password-file",
            &password,
            "--output",
            output,
        ]);
        runs.push(run_fixture(&args, &fake, b""));
    }
    // A usage error with the secret's file given: the request file names a flag's field.
    let request = file(&dir, "request.json", br#"{"user": "v"}"#);
    let args = with_call(&[
        "widgets",
        "login",
        "--user",
        "u",
        "--password-file",
        &password,
        "--request",
        &request,
    ]);
    let conflict = run_fixture(&args, &fake, b"");
    assert_eq!(conflict.code, 2);
    assert_eq!(
        conflict.stderr,
        "error: --user and the request file both set `user`\n"
    );
    runs.push(conflict);
    runs.push(run_fixture(&["widgets", "login", "--help"], &fake, b""));
    // From stdin, and a usage error from clap with the secret on stdin.
    runs.push(run_fixture(
        &with_call(&["widgets", "login", "--password-stdin"]),
        &fake,
        SECRET.as_bytes(),
    ));
    runs.push(run_fixture(
        &with_call(&["widgets", "login", "--password-stdin", "--bogus"]),
        &fake,
        SECRET.as_bytes(),
    ));
    for ran in &runs {
        assert!(!ran.stdout.contains(SECRET), "{ran:?}");
        assert!(!ran.stderr.contains(SECRET), "{ran:?}");
    }
    // The request carries it: from the file without its newline, and from stdin as given.
    let calls = fake.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        let sent =
            DynamicMessage::decode(message("LoginRequest"), call.request.clone()).expect("decode");
        assert_eq!(
            sent.get_field_by_name("password").expect("field").as_str(),
            Some(SECRET)
        );
    }
}

/// Catches a `--token` flag, the token sent without `Bearer`, or printed; a token with a space
/// or a control character sent; and the token read from anywhere but a file or stdin.
#[test]
fn the_token_is_read_only_from_a_file_or_stdin() {
    const TOKEN: &str = "tok.en-value_123";
    let dir = scratch("token");
    let token = file(&dir, "token", format!("{TOKEN}\n").as_bytes());
    let cli = tree::build(fixture::COMMANDS);
    assert!(cli.get_arguments().all(|a| a.get_long() != Some("token")));
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let refused = run_fixture(
        &with_call(&["gadgets", "list", "--token", TOKEN]),
        &fake,
        b"",
    );
    assert_eq!(refused.code, 2);
    assert!(!refused.stderr.contains(TOKEN), "{}", refused.stderr);
    for (args, stdin) in [
        (
            vec!["gadgets", "list", "--token-file", token.as_str()],
            &b""[..],
        ),
        (vec!["gadgets", "list", "--token-stdin"], TOKEN.as_bytes()),
    ] {
        let ran = run_fixture(&with_call(&args), &fake, stdin);
        assert_eq!(ran.code, 0, "{ran:?}");
        assert!(!ran.stdout.contains(TOKEN) && !ran.stderr.contains(TOKEN));
    }
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let value = call.metadata.get("authorization").expect("authorization");
        assert_eq!(
            value.as_encoded_bytes(),
            format!("Bearer {TOKEN}").as_bytes()
        );
        assert!(!format!("{value:?}").contains(TOKEN));
    }
    let none = run_fixture(&with_call(&["gadgets", "list"]), &fake, b"");
    assert_eq!(none.code, 0);
    assert!(fake.calls()[2].metadata.get("authorization").is_none());
    for bad in [&b""[..], b"two words", b"bell\x07", "caf\u{e9}".as_bytes()] {
        let ran = run_fixture(
            &with_call(&["gadgets", "list", "--token-stdin"]),
            &fake,
            bad,
        );
        assert_eq!(ran.code, 2);
        assert_eq!(
            ran.stderr,
            "error: the token is empty or holds a character that is not visible ASCII\n"
        );
    }
    let gone = dir.path().join("gone");
    let gone = gone.to_str().expect("UTF-8");
    let missing = run_fixture(
        &with_call(&["gadgets", "list", "--token-file", gone]),
        &fake,
        b"",
    );
    assert_eq!(missing.code, 2);
    assert!(
        missing
            .stderr
            .starts_with(&format!("error: cannot read the token from {gone}: ")),
        "{}",
        missing.stderr
    );
    assert_eq!(fake.calls().len(), 3);
}

/// Catches two inputs reading stdin, which would split it between them.
#[test]
fn at_most_one_input_reads_stdin() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    for args in [
        vec!["widgets", "login", "--request", "-", "--token-stdin"],
        vec!["widgets", "login", "--password-stdin", "--key-stdin"],
        vec!["widgets", "login", "--password-stdin", "--token-stdin"],
    ] {
        let ran = run_fixture(&with_call(&args), &fake, b"{}");
        assert_eq!(ran.code, 2, "{args:?}");
        assert_eq!(
            ran.stderr,
            "error: at most one input may read stdin (--request -, --token-stdin, a secret's -stdin)\n"
        );
    }
    assert!(fake.calls().is_empty());
}

/// Catches the global flags misread into the target: the endpoint, the CA, the client pair, the
/// transport, the timeout; and a call made without an endpoint or a CA.
#[test]
fn the_global_flags_make_the_target() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    // Parsed only; nothing connects.
    let ran = run_fixture(
        &[
            "gadgets",
            "list",
            "--endpoint",
            "https://[::1]:9443",
            "--ca-file",
            "ca.pem",
            "--client-cert-file",
            "c.pem",
            "--client-key-file",
            "k.pem",
            "--transport",
            "gateway",
            "--timeout-secs",
            "7",
        ],
        &fake,
        b"",
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    let target = fake.targets().pop().expect("a target");
    assert_eq!(target.endpoint.host(), "::1");
    assert_eq!(target.endpoint.port(), 9443);
    assert_eq!(target.transport, TransportKind::Gateway);
    assert_eq!(target.ca_file, std::path::PathBuf::from("ca.pem"));
    assert_eq!(
        target.client_identity,
        Some(("c.pem".into(), "k.pem".into()))
    );
    assert_eq!(target.timeout, std::time::Duration::from_secs(7));
    let defaults = run_fixture(&with_call(&["gadgets", "list"]), &fake, b"");
    assert_eq!(defaults.code, 0);
    let target = fake.targets().pop().expect("a target");
    assert_eq!(
        (target.transport, target.endpoint.port()),
        (TransportKind::Grpc, 443)
    );
    assert_eq!(target.timeout, std::time::Duration::from_secs(30));
    assert_eq!(target.client_identity, None);

    for (args, error) in [
        (
            vec!["gadgets", "list", "--ca-file", "c"],
            "error: --endpoint is needed to call an RPC\n",
        ),
        (
            vec!["gadgets", "list", "--endpoint", "https://c.example"],
            "error: --ca-file is needed to call an RPC\n",
        ),
    ] {
        let ran = run_fixture(&args, &fake, b"");
        assert_eq!((ran.code, ran.stderr.as_str()), (2, error));
    }
    let lone = run_fixture(
        &with_call(&["gadgets", "list", "--client-cert-file", "c.pem"]),
        &fake,
        b"",
    );
    assert_eq!(lone.code, 2, "{lone:?}");
    let zero = run_fixture(
        &with_call(&["gadgets", "list", "--timeout-secs", "0"]),
        &fake,
        b"",
    );
    assert_eq!(zero.code, 2, "{zero:?}");
}

/// Catches a transport that cannot be made read as a call that failed (exit 1) rather than the
/// caller's error, and a reply that does not decode read as success.
#[test]
fn a_connect_error_and_a_malformed_reply() {
    let refused = run_fixture(
        &with_call(&["gadgets", "list"]),
        &Fake::refusing_to_connect(),
        b"",
    );
    assert_eq!(
        (refused.code, refused.stderr.as_str()),
        (
            2,
            "error: the CA in ca.pem is not usable: refused by the test\n"
        )
    );
    let garbage = Fake::with(Ok(Bytes::from_static(b"\xff\xff\xff")));
    let ran = run_fixture(&with_call(&["widgets", "make"]), &garbage, b"");
    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr.starts_with("error: malformed reply: "),
        "{}",
        ran.stderr
    );
    assert_eq!(ran.stdout, "");
}

/// Catches a table and descriptors that disagree (a message or field the table names and the
/// descriptors lack, a set that does not decode) read as anything but a failure.
#[test]
fn a_table_the_descriptors_do_not_match_fails() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let ran = run_on(
        fixture::COMMANDS,
        b"\xff",
        &with_call(&["gadgets", "list"]),
        &fake,
        b"",
    );
    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr
            .starts_with("error: the descriptor set does not decode: "),
        "{}",
        ran.stderr
    );

    let make = fixture::COMMANDS
        .iter()
        .find(|c| c.method == "Make")
        .expect("widgets make");
    let base = fixture::COMMANDS
        .iter()
        .find(|c| c.method == "List")
        .copied()
        .expect("gadgets list");
    let cases = [
        (
            hostkeeper_proto::ctl::Command {
                response: "hostkeeper.fixture.v1.Gone",
                ..base
            },
            "error: the descriptors have no message hostkeeper.fixture.v1.Gone\n",
        ),
        (
            hostkeeper_proto::ctl::Command {
                request: "hostkeeper.fixture.v1.Gone",
                ..base
            },
            "error: the descriptors have no message hostkeeper.fixture.v1.Gone\n",
        ),
        (
            hostkeeper_proto::ctl::Command {
                fields: &make.fields[..1],
                ..base
            },
            "error: the descriptors' hostkeeper.fixture.v1.Empty has no field name\n",
        ),
    ];
    for (command, error) in cases {
        let ran = run_on(
            &[command],
            fixture::DESCRIPTORS,
            &with_call(&["gadgets", "list"]),
            &fake,
            b"",
        );
        assert_eq!((ran.code, ran.stderr.as_str()), (1, error));
    }
    assert!(fake.calls().is_empty());
}

/// Catches `run` not being the real table: it calls a real RPC through the in-memory transport
/// and a service it does not serve answers UNIMPLEMENTED (exit 1).
#[test]
fn run_calls_the_real_table() {
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()));
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = crate::run(
        with_call(&["ctl", "hosts", "list"]),
        &memory,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(stderr).expect("UTF-8"),
        "error: UNIMPLEMENTED: \n"
    );
    assert_eq!(memory.calls(), 1);
    assert_eq!(CALL.len(), 4);
}
