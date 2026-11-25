use super::*;

/// A blob of `kind` as `ssh-keygen` would write it, with `fill` after the type: the SSH string
/// of the type, then bytes the rule does not read.
fn blob(kind: &str, fill: u8) -> String {
    let mut bytes = u32::try_from(kind.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend_from_slice(&[fill; 32]);
    STANDARD.encode(bytes)
}

fn key(fill: u8, comment: &str) -> String {
    let blob = blob("ssh-ed25519", fill);
    match comment {
        "" => format!("ssh-ed25519 {blob}"),
        _ => format!("ssh-ed25519 {blob} {comment}"),
    }
}

/// Proves every type of the list is read, and a line is given back with single spaces, its
/// comment's words kept in order.
#[test]
fn every_listed_type_is_read_and_written_with_single_spaces() {
    for kind in KEY_TYPES {
        let blob = blob(kind, 7);
        assert_eq!(
            public_key(&format!("  {kind}\t{blob}  ops   laptop \n")),
            Ok(format!("{kind} {blob} ops laptop"))
        );
        assert_eq!(
            public_key(&format!("{kind} {blob}")),
            Ok(format!("{kind} {blob}"))
        );
    }
}

/// Proves a line that is not a public key is refused by the rule it breaks: no blob, an
/// unknown type, options before the type, a blob that is not base64, a blob of another type or
/// too short to hold one, and a comment with a byte that is not printable ASCII.
#[test]
fn a_line_that_is_not_a_public_key_is_refused_by_its_rule() {
    let ed = blob("ssh-ed25519", 1);
    let cases = [
        (String::new(), KeyLineError::NotAPublicKey),
        ("ssh-ed25519".to_owned(), KeyLineError::NotAPublicKey),
        (format!("ssh-dss {ed}"), KeyLineError::NotAPublicKey),
        (
            format!("command=\"true\" ssh-ed25519 {ed}"),
            KeyLineError::NotAPublicKey,
        ),
        ("ssh-ed25519 not*base64".to_owned(), KeyLineError::Blob),
        (format!("ssh-rsa {ed}"), KeyLineError::Blob),
        ("ssh-ed25519 AAAA".to_owned(), KeyLineError::Blob),
        (format!("ssh-ed25519 {ed} caf\u{e9}"), KeyLineError::Comment),
    ];
    for (line, why) in cases {
        assert_eq!(public_key(&line), Err(why), "{line:?}");
    }
}

/// Proves a set at the limits is kept: no key, and [`MAX_KEYS`] distinct keys.
#[test]
fn a_set_at_its_limits_is_kept() {
    assert_eq!(check_set(&[]), Ok(()));
    let full: Vec<String> = (0..MAX_KEYS)
        .map(|n| key(u8::try_from(n).unwrap_or(0), "ops"))
        .collect();
    assert_eq!(check_set(&full), Ok(()));
}

/// Proves a set that breaks a rule is refused by the first rule it breaks, naming the key's
/// place and never its text: one key too many, a key too long, a key that is not one, a key
/// with a space more, and the same type and blob twice under two comments.
#[test]
fn a_set_that_breaks_a_rule_is_refused_naming_the_place() {
    let over: Vec<String> = (0..=MAX_KEYS)
        .map(|n| key(u8::try_from(n).unwrap_or(0), ""))
        .collect();
    assert_eq!(
        check_set(&over),
        Err(KeySetError::TooMany {
            count: MAX_KEYS + 1
        })
    );
    let long = key(1, &"c".repeat(MAX_KEY_BYTES));
    assert_eq!(
        check_set(&[key(2, ""), long]),
        Err(KeySetError::TooLong { place: 2 })
    );
    assert_eq!(
        check_set(&["ssh-ed25519".to_owned()]),
        Err(KeySetError::NotAKey {
            place: 1,
            why: KeyLineError::NotAPublicKey
        })
    );
    assert_eq!(
        check_set(&[key(1, ""), key(2, "").replace(' ', "  ")]),
        Err(KeySetError::NotCanonical { place: 2 })
    );
    assert_eq!(
        check_set(&[key(1, "a"), key(2, "b"), key(1, "c")]),
        Err(KeySetError::Twice { place: 3, first: 1 })
    );
}

/// Proves the refusal's text names the place and the rule and holds no key text, so a key
/// given by mistake is not repeated into a log.
#[test]
fn a_refusal_names_the_place_and_never_the_key() {
    assert_eq!(
        KeySetError::NotAKey {
            place: 4,
            why: KeyLineError::Comment
        }
        .to_string(),
        "key 4: a comment that is not printable ASCII"
    );
    assert_eq!(
        KeySetError::Twice { place: 3, first: 1 }.to_string(),
        "key 3 has the type and blob of key 1"
    );
    assert_eq!(
        KeySetError::TooMany { count: 65 }.to_string(),
        "65 keys, more than the 64 a set may hold"
    );
    assert_eq!(
        KeySetError::TooLong { place: 2 }.to_string(),
        "key 2 is longer than 4096 bytes"
    );
    assert_eq!(
        KeySetError::NotCanonical { place: 2 }.to_string(),
        "key 2 is not written with single spaces and nothing around it"
    );
}

/// Proves the digest is the SHA-256 of each key and a newline, in order: the empty set's is that
/// of no bytes, and the order of the keys changes it.
#[test]
fn the_digest_is_of_each_key_and_a_newline_in_order() {
    assert_eq!(
        digest(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        digest(&["a".to_owned(), "b".to_owned()]),
        digest_of(b"a\nb\n")
    );
    assert_ne!(
        digest(&["a".to_owned(), "b".to_owned()]),
        digest(&["b".to_owned(), "a".to_owned()])
    );
    assert!(is_digest(&digest(&[key(1, "")])));
}

fn digest_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Proves only 64 lower-case hex characters are a digest.
#[test]
fn a_digest_is_sixty_four_lower_case_hex_characters() {
    assert!(is_digest(&"0a".repeat(32)));
    assert!(!is_digest(&"0a".repeat(31)));
    assert!(!is_digest(&"0A".repeat(32)));
    assert!(!is_digest(&"0g".repeat(32)));
    assert!(!is_digest(""));
}

/// Catches: a bound of this rule that the schema's comments give differently (a set of 128 keys
/// where `agent.proto` says 64), so that a coordinator sends what a daemon refuses.
#[test]
fn the_bounds_are_the_schemas() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto");
    let text = std::fs::read_to_string(path).expect("read the file");
    let comments = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("//"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    for sentence in [
        format!("The keys, 0 to {MAX_KEYS}, each one line"),
        format!("at most {MAX_KEY_BYTES} bytes, none twice"),
    ] {
        assert!(comments.contains(&sentence), "{sentence}");
    }
}
