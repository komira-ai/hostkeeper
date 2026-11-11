//! The seal, the kinds, the redacting types and the start rules of the master key.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::{AbmToken, KEY_FLAG, KeyError, MasterKey, SealError, Secret, SecretKind, TokenError};
use crate::testutil::scratch;

const NONCE: [u8; 12] = [7; 12];

fn key() -> MasterKey {
    MasterKey::from_bytes(&[0x42; 32]).expect("a 32-byte key")
}

fn random_ok(buffer: &mut [u8]) -> bool {
    buffer.fill(0x5a);
    true
}

fn random_fails(_: &mut [u8]) -> bool {
    false
}

fn error(result: Result<Option<MasterKey>, KeyError>) -> String {
    result.expect_err("the key is refused").to_string()
}

const TOKEN: &str = r#"{"consumer_key":"ck-1","consumer_secret":"cs-2","access_token":"at-3","access_secret":"as-4","access_secret_expiry":"2027-10-31T22:01:36Z"}"#;

/// Proves: a value sealed for a kind and version opens, to the same bytes, for that kind and
/// version and no other, and the sealed bytes are the nonce, then the ciphertext and its tag,
/// holding no byte of the plaintext.
///
/// Catches: a seal that is not bound to its kind or its version, which would let a row copied
/// into another kind open there, and a seal that leaves the plaintext readable.
#[test]
fn a_value_opens_for_its_own_kind_and_version_only() {
    let key = key();
    let plain = Secret::new(b"-----BEGIN RSA PRIVATE KEY-----".to_vec());
    let sealed = key.seal("push_key", 3, &plain, NONCE).expect("sealed");
    assert_eq!(sealed.len(), 12 + plain.len() + 16);
    assert_eq!(sealed[..12], NONCE);
    assert!(
        !sealed
            .windows(plain.len())
            .any(|window| window == plain.expose()),
        "the plaintext is not in the sealed bytes"
    );
    assert_eq!(key.open("push_key", 3, &sealed).expect("opens"), plain);
    assert_eq!(
        key.open("abm_key", 3, &sealed),
        Err(SealError::Open),
        "another kind"
    );
    assert_eq!(
        key.open("push_key", 4, &sealed),
        Err(SealError::Open),
        "another version"
    );
}

/// Proves: a value does not open under another key, after one byte of it changed, or when it is
/// too short to be a nonce and a tag; the error names nothing of the value.
///
/// Catches: a store that opens what was changed, or a short input that panics.
#[test]
fn a_value_that_was_changed_or_sealed_under_another_key_does_not_open() {
    let key = key();
    let plain = Secret::new(vec![1, 2, 3]);
    let mut sealed = key.seal("tls_key", 1, &plain, NONCE).expect("sealed");
    let other = MasterKey::from_bytes(&[0x43; 32]).expect("another key");
    assert_eq!(other.open("tls_key", 1, &sealed), Err(SealError::Open));
    for len in [0, 11, 27] {
        assert_eq!(key.open("tls_key", 1, &sealed[..len]), Err(SealError::Open));
    }
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    assert_eq!(key.open("tls_key", 1, &sealed), Err(SealError::Open));
    assert_eq!(
        SealError::Open.to_string(),
        "the sealed value does not open for this kind and version"
    );
    assert_eq!(SealError::Seal.to_string(), "the value could not be sealed");
}

/// Proves: neither `Debug` nor `Display` of a secret, an ABM token or the master key shows a byte
/// of it, by exact equality of the whole text.
///
/// Catches: a derived `Debug` that prints the bytes into a log line or a failed assertion.
#[test]
fn nothing_secret_is_printed() {
    let secret = Secret::new(b"hunter2".to_vec());
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert_eq!(format!("{secret}"), "<redacted>");
    assert_eq!(format!("{secret:#?}"), "Secret(<redacted>)");
    let token = AbmToken::parse(TOKEN.as_bytes()).expect("a token");
    assert_eq!(format!("{token:?}"), "AbmToken(<redacted>)");
    assert_eq!(format!("{:?}", key()), "MasterKey(<redacted>)");
    assert!(!secret.is_empty());
    assert_eq!(secret.len(), 7);
    assert!(Secret::new(Vec::new()).is_empty());
}

/// Proves: the ABM token is read from the JSON of its four parts and their expiry, and refused
/// when it is not JSON, lacks a part or has an empty one, with an error that quotes none of the
/// text.
///
/// Catches: a token with a missing part stored as good, and an error that echoes the token.
#[test]
fn an_abm_token_is_its_four_parts_and_an_expiry() {
    let token = AbmToken::parse(TOKEN.as_bytes()).expect("a token");
    assert_eq!(token.consumer_key, "ck-1");
    assert_eq!(token.consumer_secret, "cs-2");
    assert_eq!(token.access_token, "at-3");
    assert_eq!(token.access_secret, "as-4");
    assert_eq!(token.access_secret_expiry, "2027-10-31T22:01:36Z");
    let missing = TOKEN.replace(r#""access_secret":"as-4","#, "");
    let empty = TOKEN.replace("cs-2", "");
    for bad in [
        &b"not json"[..],
        missing.as_bytes(),
        empty.as_bytes(),
        b"[]",
    ] {
        assert_eq!(AbmToken::parse(bad), Err(TokenError));
    }
    assert_eq!(
        TokenError.to_string(),
        "the ABM token is not a JSON object of consumer_key, consumer_secret, access_token, access_secret and access_secret_expiry, each set"
    );
}

/// Proves: every kind spells as its name, reads back from it, and no other text is a kind; each
/// kind has the audit action of design 4.9.
///
/// Catches: two kinds with one spelling, and an upload audited under another kind's action.
#[test]
fn each_kind_has_one_spelling_and_one_audit_action() {
    let names: Vec<&str> = SecretKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(
        names,
        [
            "push_key",
            "push_cert",
            "push_encryption_key",
            "push_pending_key",
            "abm_key",
            "abm_cert",
            "abm_token",
            "mdm_ca_key",
            "acme_account_key",
            "dns_credential",
            "tls_key",
            "tls_cert"
        ]
    );
    for kind in SecretKind::ALL {
        assert_eq!(SecretKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(SecretKind::parse("Push_Key"), None);
    assert_eq!(SecretKind::parse(""), None);
    let actions: Vec<&str> = SecretKind::ALL
        .iter()
        .map(|kind| kind.put_action())
        .collect();
    assert_eq!(
        actions,
        [
            "mdm_push_cert_put",
            "mdm_push_cert_put",
            "mdm_secret_put",
            "mdm_secret_put",
            "mdm_secret_put",
            "mdm_secret_put",
            "mdm_abm_token_put",
            "mdm_secret_put",
            "mdm_acme_credential_put",
            "mdm_acme_credential_put",
            "mdm_tls_put",
            "mdm_tls_put"
        ]
    );
}

/// Proves: a key of any length but 32 bytes is refused.
///
/// Catches: a cipher key silently padded or truncated.
#[test]
fn a_key_is_32_bytes() {
    for len in [0, 16, 31, 33, 64] {
        assert_eq!(
            MasterKey::from_bytes(&vec![1; len]).err(),
            Some(KeyError::Format)
        );
    }
}

/// Proves: with no flag and no sealed row there is no key and the start goes on; with no flag and
/// a sealed row the start is refused naming the flag.
///
/// Catches: a coordinator that starts with sealed secrets it cannot open.
#[test]
fn no_flag_is_no_key_unless_secrets_are_sealed() {
    assert!(
        MasterKey::load(None, false, &random_ok)
            .expect("no key is no error")
            .is_none()
    );
    assert_eq!(
        error(MasterKey::load(None, true, &random_ok)),
        "--mdm-secrets-key-file is not given and the store holds sealed MDM secrets; give the key file they were sealed under"
    );
}

/// Proves: a file that is not there is made when the store holds no sealed row, with mode 0600 and
/// 64 hex digits and a newline, and the next start reads the same key from it; with a sealed row
/// the start is refused and nothing is made.
///
/// Catches: a silently new key over sealed secrets, which would orphan them, and a key file
/// readable by others.
#[test]
fn the_key_file_is_made_only_when_nothing_is_sealed() {
    let dir = scratch("mdm-key-made");
    let path = dir.path().join("key");
    assert_eq!(
        error(MasterKey::load(Some(&path), true, &random_ok)),
        "--mdm-secrets-key-file: the file is not there and the store holds sealed MDM secrets; restore the key file the secrets were sealed under (a new key would orphan every one)"
    );
    assert!(!path.exists(), "nothing was made");

    let made = MasterKey::load(Some(&path), false, &random_ok)
        .expect("made")
        .expect("a key");
    let mode = fs::metadata(&path).expect("the file").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        fs::read_to_string(&path).expect("readable"),
        format!("{}\n", "5a".repeat(32))
    );
    let sealed = made
        .seal("push_key", 1, &Secret::new(vec![9]), NONCE)
        .expect("sealed");
    let again = MasterKey::load(Some(&path), true, &random_fails)
        .expect("read")
        .expect("a key");
    assert_eq!(
        again.open("push_key", 1, &sealed).expect("the same key"),
        Secret::new(vec![9])
    );
}

/// Proves: a key file that breaks the rule of secret files (mode 0644, a symlink, a directory) or
/// holds no 64 hex digits is refused with the flag's name and no path or content; a system with
/// no random bytes refuses to make one, and a path that cannot be created says why.
///
/// Catches: a key file read though others can read it, a content that is not a key accepted, and
/// an error that prints the key.
#[test]
fn a_key_file_that_is_not_one_is_refused() {
    let dir = scratch("mdm-key-refused");
    let path = dir.path().join("key");
    let hex = "ab".repeat(32);

    fs::write(&path, format!("{hex}\n")).expect("written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode");
    assert_eq!(
        error(MasterKey::load(Some(&path), false, &random_ok)),
        format!("{KEY_FLAG}: its mode must be 0600 or 0400")
    );

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(
        MasterKey::load(Some(&path), false, &random_ok)
            .expect("a good file")
            .is_some()
    );
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).expect("a link");
    assert!(MasterKey::load(Some(&link), false, &random_ok).is_err());

    for (name, text) in [
        ("short", "abcd".to_owned()),
        ("long", format!("{hex}{hex}")),
        ("notahex", "zz".repeat(32)),
        (
            "binary",
            String::from_utf8_lossy(&[0xff, 0xfe]).into_owned(),
        ),
    ] {
        let bad = dir.path().join(name);
        fs::write(&bad, &text).expect("written");
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o600)).expect("mode");
        assert_eq!(
            error(MasterKey::load(Some(&bad), false, &random_ok)),
            format!("{KEY_FLAG}: the file must hold 64 hex digits, the 256-bit key"),
            "{name}"
        );
    }
    let bytes = dir.path().join("bytes");
    fs::write(&bytes, [0xff_u8, 0xfe, 0xfd]).expect("written");
    fs::set_permissions(&bytes, fs::Permissions::from_mode(0o600)).expect("mode");
    assert_eq!(
        error(MasterKey::load(Some(&bytes), false, &random_ok)),
        format!("{KEY_FLAG}: the file must hold 64 hex digits, the 256-bit key")
    );

    let fresh = dir.path().join("fresh");
    assert_eq!(
        error(MasterKey::load(Some(&fresh), false, &random_fails)),
        format!("{KEY_FLAG}: the system gave no random bytes for a new key")
    );
    assert!(!fresh.exists());
    let below = dir.path().join("no-such-directory").join("key");
    assert_eq!(
        error(MasterKey::load(Some(&below), false, &random_ok)),
        format!("{KEY_FLAG}: the file could not be made: entity not found")
    );
}
