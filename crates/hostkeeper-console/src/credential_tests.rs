//! The BMC credential file: a secret file whose text no error and no `Debug` prints.

use super::{BmcCredential, CredentialError, FLAG, MAX_FILE_BYTES};
use crate::file::ReadError;
use crate::testutil::{put, scratch};

const PASSWORD: &str = "Zq9 secret-pass#2026";
const USER: &str = "hk-console";

fn text(user: &str, password: &str) -> String {
    format!("user = \"{user}\"\npassword = \"{password}\"\n")
}

/// Proves: the credential prints as exactly `BmcCredential { user: <redacted>, password:
/// <redacted> }`, in plain and in pretty `Debug`, and the user and password are what the file
/// holds.
///
/// Catches: a `Debug` that prints the user or the password (into a log line or an error), and a
/// credential read as other text than the file's.
#[test]
fn the_credential_formats_redacted() {
    let credential = BmcCredential::parse(&text(USER, PASSWORD)).expect("parsed");
    assert_eq!(
        format!("{credential:?}"),
        "BmcCredential { user: <redacted>, password: <redacted> }"
    );
    assert_eq!(
        format!("{credential:#?}"),
        "BmcCredential { user: <redacted>, password: <redacted> }"
    );
    assert_eq!((credential.user(), credential.password()), (USER, PASSWORD));
}

/// Proves: a credential text that breaks a rule is refused with exactly the error text of its
/// rule, which holds none of the file: a user or password that is empty, too long or not
/// printable ASCII (a space in the user), a missing or unknown key, a value that is not a
/// string, and text that is not TOML; a user of 16 bytes and a password of 20 are taken.
///
/// Catches: a password quoted in an error (the TOML parser's text quotes the line it fails on),
/// a user IPMI would cut, and a file with an extra key read without a word.
#[test]
fn a_credential_that_breaks_a_rule_is_refused_with_none_of_its_text() {
    assert!(BmcCredential::parse(&text(&"u".repeat(16), &"p".repeat(20))).is_ok());
    let user = "--console-bmc-credential-file: user must be 1 to 16 bytes of printable ASCII with no space";
    let password =
        "--console-bmc-credential-file: password must be 1 to 20 bytes of printable ASCII";
    let parse = "--console-bmc-credential-file: it is not TOML of exactly user and password";
    for (text, wanted) in [
        (text("", PASSWORD), user),
        (text(&"u".repeat(17), PASSWORD), user),
        (text("hk console", PASSWORD), user),
        (text("hk\\tconsole", PASSWORD), user),
        (text("h\u{e9}", PASSWORD), user),
        (text(USER, ""), password),
        (text(USER, &"p".repeat(21)), password),
        (text(USER, "Zq9\\u0007secret"), password),
        (format!("user = \"{USER}\"\n"), parse),
        (format!("password = \"{PASSWORD}\"\n"), parse),
        (format!("{}host = \"bmc\"\n", text(USER, PASSWORD)), parse),
        (format!("user = \"{USER}\"\npassword = 20262026\n"), parse),
        (format!("user = \"{USER}\"\npassword = \"{PASSWORD}"), parse),
    ] {
        let refused = BmcCredential::parse(&text)
            .expect_err("refused")
            .to_string();
        assert_eq!(refused, wanted, "{text}");
    }
}

/// Proves: the credential is read from a secret file of mode 0600 or 0400; a file of mode 0644,
/// a directory, a file over the cap and one that is not UTF-8 are refused, each naming the flag
/// and holding neither the password nor the path.
///
/// Catches: the shared BMC password read from a file other users can read, and a refusal that
/// prints the password or where the file is.
#[test]
fn the_credential_is_read_only_from_a_secret_file() {
    let dir = scratch("credential-read");
    for mode in [0o600, 0o400] {
        let path = put(
            dir.path(),
            &format!("cred-{mode:o}"),
            text(USER, PASSWORD).as_bytes(),
            mode,
        );
        let credential = BmcCredential::read(&path).expect("read");
        assert_eq!(credential.password(), PASSWORD);
    }
    let open = put(dir.path(), "open", text(USER, PASSWORD).as_bytes(), 0o644);
    let big = put(
        dir.path(),
        "big",
        &vec![b'#'; usize::try_from(MAX_FILE_BYTES).expect("small") + 1],
        0o600,
    );
    let binary = put(dir.path(), "binary", &[0xff], 0o600);
    let refusals = [
        BmcCredential::read(&open).expect_err("refused"),
        BmcCredential::read(dir.path()).expect_err("refused"),
        BmcCredential::read(&big).expect_err("refused"),
        BmcCredential::read(&binary).expect_err("refused"),
    ];
    assert_eq!(
        refusals[0].to_string(),
        "--console-bmc-credential-file: its mode must be 0600 or 0400"
    );
    assert!(matches!(refusals[0], CredentialError::Secret(_)));
    assert_eq!(
        refusals[2],
        CredentialError::Read(ReadError::TooLong {
            flag: FLAG,
            limit: MAX_FILE_BYTES
        })
    );
    assert_eq!(
        refusals[3],
        CredentialError::Read(ReadError::NotText { flag: FLAG })
    );
    let path = dir.path().display().to_string();
    for refused in &refusals {
        let text = format!("{refused} {refused:?}");
        assert!(text.contains(FLAG), "{text}");
        assert!(!text.contains(PASSWORD) && !text.contains(&path), "{text}");
    }
}
