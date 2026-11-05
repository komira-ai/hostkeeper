//! The credentials a diagnostic must not repeat: the arguments that may be credentials, the
//! secret values of a message, and the replacement of both in a text.

use std::collections::BTreeSet;

use super::{REDACTED, argument_credentials, extension, known_flags, scrub, values};
use crate::testutil::{fixture, from_json, pool};
use crate::tree;

/// The strings of `items`.
fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// A set of known flag names.
fn known(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// Catches a value attached to a `-stdin` flag, one that follows it, or one attached to a flag
/// that is no flag of the tree left out of the credentials; and a value that is not one
/// (a known flag's, a following flag, a plain word) taken for one.
#[test]
fn the_arguments_that_may_be_credentials_are_found() {
    let known = known(&["endpoint", "token-stdin", "password-stdin", "user", "help"]);
    let cases: &[(&[&str], &[&str])] = &[
        (&["ctl", "x", "--token-stdin=cred-a"], &["cred-a"]),
        (&["--password-stdin", "cred-b", "--user", "u"], &["cred-b"]),
        (&["--password-stdin", "--user", "u"], &[]),
        (&["--password-stdin", "-"], &[]),
        (&["--password-stdin"], &[]),
        (&["--password=cred-c"], &["cred-c"]),
        (&["--user=u", "--endpoint=https://c.example", "--help"], &[]),
        (&["--bogus-stdin=cred-d"], &["cred-d"]),
        (
            &["--token-stdin", "cred-e", "--token-stdin=cred-f"],
            &["cred-e", "cred-f"],
        ),
        (&["--token-stdin", "--token-stdin", "cred-g"], &["cred-g"]),
        (&["--=cred-h"], &["cred-h"]),
        (&["--token-stdin=", "--other="], &["", ""]),
        (&["--", "-x", "plain", "user=u"], &[]),
        (&["plain", "--user", "word"], &[]),
        (&[], &[]),
    ];
    for (args, expected) in cases {
        assert_eq!(
            argument_credentials(&strings(args), &known),
            strings(expected),
            "{args:?}"
        );
    }
}

/// Catches a flag of a subcommand, a global flag or `help` missing from the known flags, which
/// would make its attached value look like a credential or a credential like a flag's value.
#[test]
fn the_known_flags_are_those_of_the_whole_tree() {
    let flags = known_flags(&tree::build(fixture::COMMANDS));
    for flag in [
        "help",
        "endpoint",
        "token-stdin",
        "request",
        "user",
        "password-file",
        "password-stdin",
        "colour",
    ] {
        assert!(flags.contains(flag), "{flag}");
    }
    for flag in ["password", "token", "key"] {
        assert!(!flags.contains(flag), "{flag}");
    }
}

/// Catches a credential left in a text, a replacement searched again, a shorter credential
/// replacing part of a longer one, an empty credential matching everywhere, and a multibyte
/// character split.
#[test]
fn scrub_replaces_every_credential_once_longest_first() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("a cred1 b cred1", &["cred1"], "a <redacted> b <redacted>"),
        ("xx-abc-abcd", &["abc", "abcd"], "xx-<redacted>-<redacted>"),
        ("tokenX", &["token", "tokenX"], "<redacted>"),
        ("ad", &["d"], "a<redacted>"),
        ("d", &["d"], "<redacted>"),
        ("a red b", &["red", "a"], "<redacted> <redacted> b"),
        ("abc", &[""], "abc"),
        ("abc", &[], "abc"),
        ("", &["abc"], ""),
        ("é-secret-é", &["secret"], "é-<redacted>-é"),
        ("é-é", &["é"], "<redacted>-<redacted>"),
        ("one two", &["one", "two"], "<redacted> <redacted>"),
    ];
    for (text, credentials, expected) in cases {
        assert_eq!(scrub(text, &strings(credentials)), *expected, "{text:?}");
    }
    assert_eq!(REDACTED, "<redacted>");
}

/// Catches a secret value left out of the credentials (a list element, a oneof member, bytes,
/// one inside a nested message, a list or a map), one that is not a secret put in, an empty
/// value or bytes that are not text put in, and a pool without the option finding any.
#[test]
fn the_secret_values_of_a_message_are_found_at_any_depth() {
    let pool = pool();
    let ext = extension(&pool);
    let request = from_json(
        "LoginRequest",
        r#"{"user": "u", "password": "p1", "key": "a2V5", "recoveryCodes": ["r1", "", "r2"],
            "otp": "123456"}"#,
    );
    let mut found = values(&request, ext.as_ref());
    found.sort();
    assert_eq!(found, strings(&["123456", "key", "p1", "r1", "r2"]));
    assert!(values(&request, None).is_empty());
    let phrase = from_json("LoginRequest", r#"{"user": "u", "phrase": "not-secret"}"#);
    assert!(values(&phrase, ext.as_ref()).is_empty());

    let response = from_json(
        "LoginResponse",
        r#"{"user": "u", "session": "se", "inner": {"note": "n", "token": "t1"},
            "inners": [{"token": "t2"}], "byName": {"k": {"token": "t3"}},
            "recoveryCodes": ["c1"], "key": "a2V5"}"#,
    );
    let mut found = values(&response, ext.as_ref());
    found.sort();
    assert_eq!(found, strings(&["c1", "key", "se", "t1", "t2", "t3"]));

    // Bytes that are not UTF-8 text, and an empty secret, are not credentials to scrub.
    let binary = from_json("LoginRequest", r#"{"key": "//4=", "password": ""}"#);
    assert!(values(&binary, ext.as_ref()).is_empty());
}
