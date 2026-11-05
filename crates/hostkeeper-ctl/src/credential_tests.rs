//! Credentials and the request's inputs end to end on the fixture table with a fake transport:
//! what clap's usage errors and a server's diagnostics repeat, the secrets of a request file and
//! of a response in every shape, the inputs that read stdin, the refused `unspecified`, and a
//! failing diagnostics stream.

use std::io::{self, Write};

use bytes::Bytes;
use prost_reflect::DynamicMessage;
use tonic::{Code, Status};

use crate::testutil::{
    Broken, Fake, file, fixture, from_json, message, run_fixture, scratch, with_call,
};
use crate::{Io, codes, exit, run_with, tree};

/// A credential typed where none belongs.
const SECRET: &str = "hunter2-credential";

/// A bearer token.
const TOKEN: &str = "tok.en-value_123";

/// The error of a run that reads stdin for more than one input.
const ONE_STDIN: &str =
    "error: at most one input may read stdin (--request -, --token-stdin, a secret's -stdin)\n";

/// A login request that sets a secret of every shape through the request file.
const LOGIN_REQUEST: &str = r#"{"user": "u", "password": "pw-1", "key": "a2V5LTI=",
    "recoveryCodes": ["rc-1", "rc-2"], "otp": "otp-1"}"#;

/// A login response that holds a secret of every shape, and a secret in a nested message.
const LOGIN_RESPONSE: &str = r#"{"user": "u", "session": "se-1",
    "inner": {"note": "n", "token": "t-1"}, "recoveryCodes": ["rc-1", "rc-9"], "key": "a2V5LTI="}"#;

/// A writer whose first write fails and whose later writes are kept.
#[derive(Default)]
struct FailsOnce {
    failed: bool,
    written: Vec<u8>,
}

impl Write for FailsOnce {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.failed {
            self.failed = true;
            return Err(io::Error::other("stderr closed, for the test"));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs `ctl` on the fixture with `args` after the program's name, empty stdin and the given
/// stderr; returns the exit code and what was written to stdout.
fn run_to<W: Write>(stderr: &mut W, args: Vec<&str>, fake: &Fake) -> (u8, String) {
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let code = run_with(
        fixture::COMMANDS,
        fixture::DESCRIPTORS,
        std::iter::once("ctl").chain(args),
        fake,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr,
        },
    );
    (code, String::from_utf8(stdout).expect("UTF-8 stdout"))
}

/// A fake that answers a login with `json` as the response.
fn login_answering(json: &str) -> Fake {
    Fake::answering(&from_json("LoginResponse", json))
}

/// Catches a credential typed in the wrong place (attached to a `-stdin` flag, after one, or
/// as the value of a secret's name typed as a flag) repeated in clap's usage error; the error
/// is clap's own text with only the credential replaced.
#[test]
fn a_credential_in_a_usage_error_is_replaced_and_nothing_else_changes() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let cases: &[(&str, &str, bool)] = &[
        ("login", "--token-stdin=hunter2-credential", true),
        ("login", "--password-stdin=hunter2-credential", true),
        ("list", "--token-stdin=hunter2-credential", true),
        ("login", "--password=hunter2-credential", false),
        ("login", "--session=hunter2-credential", false),
    ];
    for (rpc, flag, echoed) in cases {
        let service = if *rpc == "login" {
            "widgets"
        } else {
            "gadgets"
        };
        let args = with_call(&[service, *rpc, *flag]);
        let raw = tree::build(fixture::COMMANDS)
            .try_get_matches_from(std::iter::once("ctl").chain(args.iter().copied()))
            .expect_err("a usage error")
            .to_string();
        let ran = run_fixture(&args, &fake, SECRET.as_bytes());
        assert_eq!(ran.code, 2, "{flag}");
        assert_eq!(ran.stdout, "", "{flag}");
        assert_eq!(ran.stderr, raw.replace(SECRET, "<redacted>"), "{flag}");
        assert!(!ran.stderr.contains(SECRET), "{flag}: {}", ran.stderr);
        if *echoed {
            assert!(raw.contains(SECRET), "clap no longer repeats it: {raw}");
            assert!(ran.stderr.contains("<redacted>"), "{flag}");
        }
    }
    // The word that follows a `-stdin` flag is a credential as well.
    for args in [
        vec!["widgets", "login", "--token-stdin", SECRET],
        vec!["widgets", "login", "--password-stdin", SECRET],
        vec!["gadgets", "list", "--token-stdin", SECRET],
    ] {
        let args = with_call(&args);
        let raw = tree::build(fixture::COMMANDS)
            .try_get_matches_from(std::iter::once("ctl").chain(args.iter().copied()))
            .expect_err("a usage error")
            .to_string();
        let ran = run_fixture(&args, &fake, b"");
        assert_eq!(ran.code, 2, "{args:?}");
        assert_eq!(ran.stderr, raw.replace(SECRET, "<redacted>"), "{args:?}");
        assert!(raw.contains(SECRET), "clap no longer repeats it: {raw}");
        assert!(!ran.stderr.contains(SECRET), "{args:?}: {}", ran.stderr);
    }
    // The first line of the attached case, whole.
    let attached = run_fixture(
        &with_call(&["gadgets", "list", "--token-stdin=hunter2-credential"]),
        &fake,
        b"",
    );
    assert_eq!(
        attached.stderr.lines().next(),
        Some(
            "error: unexpected value '<redacted>' for '--token-stdin' found; no more were expected"
        )
    );
    assert!(fake.calls().is_empty() && fake.targets().is_empty());
}

/// Catches a usage error that is not about a credential changed by the scrub: a bad enum value
/// and an unknown flag, whose arguments hold no credential, print clap's text as it is.
#[test]
fn a_usage_error_without_a_credential_is_clap_s_text_unchanged() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    for args in [
        vec!["widgets", "make", "--colour", "purple"],
        vec!["widgets", "login", "--bogus"],
        vec!["widgets", "login", "--password-stdin", "--bogus"],
    ] {
        let args = with_call(&args);
        let raw = tree::build(fixture::COMMANDS)
            .try_get_matches_from(std::iter::once("ctl").chain(args.iter().copied()))
            .expect_err("a usage error")
            .to_string();
        let ran = run_fixture(&args, &fake, b"");
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{args:?}");
        assert_eq!(ran.stderr, raw, "{args:?}");
    }
}

/// Catches a status message that repeats the bearer token (from a file or stdin) or a secret of
/// the request (a flag-file secret, or a repeated, oneof or bytes one of the request file), in a
/// refusal or in a failure, and a control character that survives; and a message with no
/// credential changed.
#[test]
fn a_server_diagnostic_never_repeats_a_credential() {
    let dir = scratch("echo");
    let token = file(&dir, "token", format!("{TOKEN}\n").as_bytes());
    let password = file(&dir, "password", b"hunter2-pw\n");
    let failure = |code: Code, text: &str| format!("error: {}: {text}\n", codes::name(code));
    struct Case<'a> {
        args: Vec<&'a str>,
        stdin: &'a [u8],
        status: Status,
        code: u8,
        stderr: String,
    }
    let all_secrets = "rc-1 rc-2 otp-1 key-2 pw-1 user-u";
    let cases = [
        Case {
            args: vec!["gadgets", "list", "--token-file", token.as_str()],
            stdin: b"",
            status: Status::unauthenticated(format!("the token {TOKEN} is not valid")),
            code: 3,
            stderr: "refused: the token <redacted> is not valid\n".to_owned(),
        },
        Case {
            args: vec!["gadgets", "list", "--token-stdin"],
            stdin: TOKEN.as_bytes(),
            status: Status::unauthenticated(format!("{TOKEN}{TOKEN}")),
            code: 3,
            stderr: "refused: <redacted><redacted>\n".to_owned(),
        },
        Case {
            args: vec![
                "widgets",
                "login",
                "--token-file",
                token.as_str(),
                "--password-file",
                password.as_str(),
            ],
            stdin: b"",
            status: Status::permission_denied(format!("{TOKEN} may not log in with hunter2-pw")),
            code: 3,
            stderr: "refused: <redacted> may not log in with <redacted>\n".to_owned(),
        },
        Case {
            args: vec!["widgets", "login", "--password-file", password.as_str()],
            stdin: b"",
            status: Status::internal("bad hunter2-pw"),
            code: 1,
            stderr: failure(Code::Internal, "bad <redacted>"),
        },
        Case {
            args: vec!["widgets", "login", "--request", "-"],
            stdin: LOGIN_REQUEST.as_bytes(),
            status: Status::permission_denied(all_secrets),
            code: 3,
            stderr: "refused: <redacted> <redacted> <redacted> <redacted> <redacted> user-u\n"
                .to_owned(),
        },
        Case {
            args: vec!["gadgets", "list", "--token-file", token.as_str()],
            stdin: b"",
            status: Status::failed_precondition(format!("\u{1b}[2J {TOKEN}\n")),
            code: 2,
            stderr: "refused: \\u{1b}[2J <redacted>\\n\n".to_owned(),
        },
        Case {
            args: vec!["gadgets", "list", "--token-file", token.as_str()],
            stdin: b"",
            status: Status::unauthenticated("token_expired"),
            code: 3,
            stderr: "refused: token_expired\n".to_owned(),
        },
    ];
    for case in cases {
        let fake = Fake::with(Err(case.status));
        let ran = run_fixture(&with_call(&case.args), &fake, case.stdin);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (case.code, "", case.stderr.as_str()),
            "{:?}",
            case.args
        );
        assert_eq!(fake.calls().len(), 1);
    }
}

/// Catches a secret of a repeated, oneof, bytes or nested field of a request file lost on the
/// way to the wire, and one of a response shown in either output: the complete stdout and
/// stderr of each are compared.
#[test]
fn secrets_of_every_shape_are_sent_and_never_printed() {
    let fake = login_answering(LOGIN_RESPONSE);
    let notices = "redacted: hostkeeper.fixture.v1.Inner.token\n\
                   redacted: hostkeeper.fixture.v1.LoginResponse.key\n\
                   redacted: hostkeeper.fixture.v1.LoginResponse.recovery_codes\n\
                   redacted: hostkeeper.fixture.v1.LoginResponse.session\n";
    let json = "{\n  \"user\": \"u\",\n  \"inner\": {\n    \"note\": \"n\"\n  }\n}\n";
    let text = "user: u\ninner:\n  {\n    \"note\": \"n\"\n  }\n";
    for (output, expected) in [("json", json), ("text", text)] {
        let args = with_call(&["widgets", "login", "--request", "-", "--output", output]);
        let ran = run_fixture(&args, &fake, LOGIN_REQUEST.as_bytes());
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, expected, notices),
            "{output}"
        );
    }
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let sent =
            DynamicMessage::decode(message("LoginRequest"), call.request.clone()).expect("decode");
        assert_eq!(sent, from_json("LoginRequest", LOGIN_REQUEST));
        assert_eq!(
            sent.get_field_by_name("key").expect("key").as_bytes(),
            Some(&Bytes::from_static(b"key-2"))
        );
        assert_eq!(
            sent.get_field_by_name("otp").expect("otp").as_str(),
            Some("otp-1")
        );
        assert_eq!(
            sent.get_field_by_name("recovery_codes")
                .expect("codes")
                .as_list()
                .map(<[prost_reflect::Value]>::len),
            Some(2)
        );
    }
    // A response that is only secrets leaves an empty object, or nothing.
    let only = login_answering(r#"{"key": "//4=", "recoveryCodes": ["rc"]}"#);
    let notices = "redacted: hostkeeper.fixture.v1.LoginResponse.key\n\
                   redacted: hostkeeper.fixture.v1.LoginResponse.recovery_codes\n";
    let ran = run_fixture(
        &with_call(&["widgets", "login", "--output", "json"]),
        &only,
        b"",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{}\n", notices)
    );
    let ran = run_fixture(&with_call(&["widgets", "login"]), &only, b"");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", notices)
    );
}

/// Catches the nested secrets of a response (in a message, a list and a map) shown or not
/// reported, with the rest of the output kept: the complete stdout and stderr are compared.
#[test]
fn nested_secrets_of_a_response_leave_the_rest_of_the_output_whole() {
    let fake = login_answering(
        r#"{"user": "u", "session": "s3cr3t-session",
            "inner": {"note": "n", "token": "s3cr3t-inner"},
            "inners": [{"token": "s3cr3t-list"}], "byName": {"k": {"token": "s3cr3t-map"}}}"#,
    );
    let notices = "redacted: hostkeeper.fixture.v1.Inner.token\n\
                   redacted: hostkeeper.fixture.v1.LoginResponse.session\n";
    let json = "{\n  \"user\": \"u\",\n  \"inner\": {\n    \"note\": \"n\"\n  },\n  \
                \"inners\": [\n    {}\n  ],\n  \"byName\": {\n    \"k\": {}\n  }\n}\n";
    let text = "user: u\ninner:\n  {\n    \"note\": \"n\"\n  }\ninners:\n  [\n    {}\n  ]\n\
                by_name:\n  {\n    \"k\": {}\n  }\n";
    for (output, expected) in [("json", json), ("text", text)] {
        let ran = run_fixture(
            &with_call(&["widgets", "login", "--output", output]),
            &fake,
            b"",
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, expected, notices),
            "{output}"
        );
    }
}

/// Catches two inputs reading stdin that are not in the existing table (the request with a
/// secret), and a single stdin reader refused: a request file or a token from a file leaves
/// stdin to the other input, which is read whole.
#[test]
fn stdin_goes_to_one_input_and_the_others_come_from_files() {
    let fake = login_answering(r#"{"user": "u"}"#);
    for args in [
        vec!["widgets", "login", "--request", "-", "--password-stdin"],
        vec!["widgets", "login", "--request", "-", "--key-stdin"],
        vec!["widgets", "login", "--token-stdin", "--key-stdin"],
    ] {
        let ran = run_fixture(&with_call(&args), &fake, b"{}");
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{args:?}");
        assert_eq!(ran.stderr, ONE_STDIN, "{args:?}");
    }
    assert!(fake.calls().is_empty());

    let dir = scratch("stdin-split");
    let request = file(&dir, "request.json", br#"{"user": "from-file"}"#);
    let token = file(&dir, "token", TOKEN.as_bytes());
    // The token from stdin, the request from a file.
    let ran = run_fixture(
        &with_call(&["widgets", "login", "--request", &request, "--token-stdin"]),
        &fake,
        TOKEN.as_bytes(),
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "user: u\n"), "{ran:?}");
    // The request from stdin, the token from a file.
    let ran = run_fixture(
        &with_call(&["widgets", "login", "--request", "-", "--token-file", &token]),
        &fake,
        br#"{"user": "from-stdin"}"#,
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "user: u\n"), "{ran:?}");
    // A secret from stdin, the request from a file.
    let ran = run_fixture(
        &with_call(&[
            "widgets",
            "login",
            "--request",
            &request,
            "--password-stdin",
        ]),
        &fake,
        b"pw-stdin",
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "user: u\n"), "{ran:?}");

    let calls = fake.calls();
    assert_eq!(calls.len(), 3);
    let sent = |i: usize| {
        DynamicMessage::decode(message("LoginRequest"), calls[i].request.clone()).expect("decode")
    };
    let bearer = format!("Bearer {TOKEN}");
    assert_eq!(
        calls[0]
            .metadata
            .get("authorization")
            .map(|v| v.as_encoded_bytes()),
        Some(bearer.as_bytes())
    );
    assert_eq!(
        sent(0),
        from_json("LoginRequest", r#"{"user": "from-file"}"#)
    );
    assert_eq!(
        calls[1]
            .metadata
            .get("authorization")
            .map(|v| v.as_encoded_bytes()),
        Some(bearer.as_bytes())
    );
    assert_eq!(
        sent(1),
        from_json("LoginRequest", r#"{"user": "from-stdin"}"#)
    );
    assert_eq!(
        sent(2),
        from_json(
            "LoginRequest",
            r#"{"user": "from-file", "password": "pw-stdin"}"#
        )
    );
}

/// Catches two sources of one credential, or a client key without its certificate, accepted
/// and a call made: each is a usage error before any transport is made.
#[test]
fn conflicting_credential_sources_are_refused_before_a_call() {
    let fake = login_answering(r#"{"user": "u"}"#);
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["gadgets", "list", "--token-file", "t.txt", "--token-stdin"],
            &["--token-file", "--token-stdin"],
        ),
        (
            &[
                "widgets",
                "login",
                "--password-file",
                "p.txt",
                "--password-stdin",
            ],
            &["--password-file", "--password-stdin"],
        ),
        (
            &["widgets", "login", "--key-file", "k.txt", "--key-stdin"],
            &["--key-file", "--key-stdin"],
        ),
        (
            &["gadgets", "list", "--client-key-file", "k.pem"],
            &["--client-cert-file"],
        ),
        (
            &["gadgets", "list", "--client-cert-file", "c.pem"],
            &["--client-key-file"],
        ),
    ];
    for (args, named) in cases {
        let ran = run_fixture(&with_call(args), &fake, b"");
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{args:?}");
        assert!(
            ran.stderr.starts_with("error: the "),
            "{args:?}: {}",
            ran.stderr
        );
        for flag in *named {
            assert!(ran.stderr.contains(*flag), "{args:?}: {}", ran.stderr);
        }
    }
    assert!(fake.calls().is_empty() && fake.targets().is_empty());
}

/// Catches `unspecified` accepted for an enum whose zero value is spelled `UNSPECIFIED` with no
/// prefix, the valid values not listed, and a valid one sent as another number.
#[test]
fn an_unprefixed_unspecified_enum_value_is_refused_and_the_valid_ones_are_listed() {
    let fake = Fake::answering(&DynamicMessage::new(message("Empty")));
    let ran = run_fixture(
        &with_call(&["gadgets", "tune", "--speed", "unspecified"]),
        &fake,
        b"",
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{ran:?}");
    assert_eq!(
        ran.stderr,
        "error: invalid value 'unspecified' for '--speed <VALUE>'\n  \
         [possible values: slow, fast]\n\nFor more information, try '--help'.\n"
    );
    assert!(fake.calls().is_empty());
    for (word, number) in [("slow", 1_u8), ("fast", 2_u8)] {
        let ran = run_fixture(
            &with_call(&["gadgets", "tune", "--speed", word]),
            &fake,
            b"",
        );
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{word}");
        let calls = fake.calls();
        let call = calls.last().expect("a call");
        assert_eq!(call.path, "/hostkeeper.fixture.v1.Gadgets/Tune");
        assert_eq!(call.request.to_vec(), vec![0x08, number]);
    }
}

/// Catches a redaction notice that cannot be written ignored: the response is not printed, and
/// the run fails with exit 1 and says why; while a response with nothing redacted does not need
/// the stream at all.
#[test]
fn a_failing_diagnostics_stream_fails_the_run_and_prints_nothing_secret() {
    let secret_reply = login_answering(r#"{"user": "u", "session": "se-1"}"#);
    for output in ["json", "text"] {
        let mut stderr = FailsOnce::default();
        let (code, stdout) = run_to(
            &mut stderr,
            with_call(&["widgets", "login", "--output", output]),
            &secret_reply,
        );
        assert_eq!((code, stdout.as_str()), (exit::FAILURE, ""), "{output}");
        assert_eq!(
            String::from_utf8(stderr.written).expect("UTF-8"),
            "error: cannot write the response: stderr closed, for the test\n",
            "{output}"
        );
    }
    let plain_reply = login_answering(r#"{"user": "u"}"#);
    let (code, stdout) = run_to(&mut Broken, with_call(&["widgets", "login"]), &plain_reply);
    assert_eq!((code, stdout.as_str()), (exit::SUCCESS, "user: u\n"));
}
