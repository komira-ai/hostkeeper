//! `registry::check`, `check_set` and `conflict`: one case under and over every limit, on
//! entries that pass every other rule.

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry::{
    EntryError, KeyringError, MAX_KEYRING, SetError, channels, check, check_set, conflict,
};

use super::real;
use super::support::*;

fn s(text: &str) -> String {
    text.to_owned()
}

fn refused(entry: &Repository) -> EntryError {
    check(entry).expect_err("a refused entry")
}

fn with(change: impl FnOnce(&mut Repository)) -> Repository {
    let mut e = entry("komira");
    change(&mut e);
    e
}

/// Proves an entry that follows every rule passes, with the primary key listed, with only the
/// subkey listed, and with both.
#[test]
fn a_valid_entry_passes_whichever_of_a_key_and_its_subkey_is_listed() {
    assert_eq!(check(&entry("komira")), Ok(()));
    for fingerprints in [
        vec![s(real::SUBKEY)],
        vec![s(real::PRIMARY), s(real::SUBKEY)],
    ] {
        assert_eq!(check(&with(|e| e.fingerprints = fingerprints)), Ok(()));
    }
}

/// Proves the channels are one per suite, in the suites' order.
#[test]
fn channels_are_the_names_joined_to_each_suite_in_order() {
    assert_eq!(
        channels(&entry("komira")),
        vec![s("komira-testing"), s("komira-stable")]
    );
}

/// Proves a name must be a repository name, 1 to 32 bytes, and the kind must be apt.
#[test]
fn the_name_and_the_kind_are_judged() {
    for name in ["", "Komira", "ko_mira", "a b"] {
        assert_eq!(
            refused(&with(|e| e.name = s(name))),
            EntryError::Name(s(name))
        );
    }
    let long = "n".repeat(33);
    assert_eq!(
        refused(&with(|e| e.name = long.clone())),
        EntryError::Name(long)
    );
    for kind in ["", "rpm", "APT"] {
        assert_eq!(
            refused(&with(|e| e.kind = s(kind))),
            EntryError::Kind(s(kind))
        );
    }
}

/// Proves a URI is `https://`, a host, an optional port and an optional path, and at most 512 bytes.
#[test]
fn the_uri_is_https_a_host_a_port_and_a_path() {
    for uri in [
        "https://repo.example.com",
        "https://repo.example.com/",
        "https://repo.example.com:8443",
        "https://repo.example.com:1",
        "https://repo.example.com:65535/a/b_c~d.e-f",
        "https://192.0.2.1/x",
        "https://a-b.example.com",
    ] {
        assert_eq!(check(&with(|e| e.uri = s(uri))), Ok(()), "{uri}");
    }
    for uri in [
        "",
        "http://repo.example.com",
        "https://",
        "https:///path",
        "https://.example.com",
        "https://-repo.example.com",
        "https://repo_x.example.com",
        "https://user@repo.example.com",
        "https://repo.example.com:",
        "https://repo.example.com:123456",
        "https://repo.example.com:80a",
        "https://repo.example.com:80:90",
        "https://repo.example.com/a?b",
        "https://repo.example.com/a b",
        "https://repo.example.com/a#b",
        "https://repo.example.com:80?x/",
    ] {
        assert_eq!(
            refused(&with(|e| e.uri = s(uri))),
            EntryError::Uri(s(uri)),
            "{uri}"
        );
    }
    let at_max = format!(
        "https://example.com/{}",
        "a".repeat(512 - "https://example.com/".len())
    );
    assert_eq!(at_max.len(), 512);
    assert_eq!(check(&with(|e| e.uri = at_max.clone())), Ok(()));
    let over = format!("{at_max}a");
    assert_eq!(refused(&with(|e| e.uri = over)), EntryError::UriLength(513));
}

/// Proves 1 to 8 suites are taken, each of `[a-z0-9-]`, none twice, and that `<name>-<suite>`
/// must itself be a name of 32 bytes at most.
#[test]
fn the_suites_are_counted_spelled_and_not_repeated() {
    let eight: Vec<String> = (1..=8).map(|i| format!("s{i}")).collect();
    assert_eq!(check(&with(|e| e.suites = eight.clone())), Ok(()));
    let nine: Vec<String> = (1..=9).map(|i| format!("s{i}")).collect();
    assert_eq!(
        refused(&with(|e| e.suites = nine)),
        EntryError::SuiteCount(9)
    );
    assert_eq!(
        refused(&with(|e| e.suites = vec![])),
        EntryError::SuiteCount(0)
    );
    for suite in ["", "Stable", "sta ble", "sta_ble", &"s".repeat(32)] {
        assert_eq!(
            refused(&with(|e| e.suites = vec![s(suite)])),
            EntryError::Suite(s(suite))
        );
    }
    assert_eq!(
        refused(&with(|e| e.suites = vec![s("a"), s("a")])),
        EntryError::SuiteTwice(s("a"))
    );
    // A name of one byte and a suite of 30 make a channel of 32 bytes; of 31, 33.
    let at_max = with(|e| {
        e.name = s("k");
        e.suites = vec!["s".repeat(30)];
    });
    assert_eq!(check(&at_max), Ok(()));
    let over = with(|e| {
        e.name = s("k");
        e.suites = vec!["s".repeat(31)];
    });
    assert_eq!(
        refused(&over),
        EntryError::Channel(format!("k-{}", "s".repeat(31)))
    );
    let over = with(|e| e.suites = vec!["s".repeat(30)]);
    assert_eq!(
        refused(&over),
        EntryError::Channel(format!("komira-{}", "s".repeat(30)))
    );
}

/// Proves 1 to 16 components are taken, each 1 to 64 bytes of `[A-Za-z0-9._-]`, none twice.
#[test]
fn the_components_are_counted_spelled_and_not_repeated() {
    let sixteen: Vec<String> = (1..=16).map(|i| format!("c{i}")).collect();
    assert_eq!(check(&with(|e| e.components = sixteen)), Ok(()));
    let seventeen: Vec<String> = (1..=17).map(|i| format!("c{i}")).collect();
    assert_eq!(
        refused(&with(|e| e.components = seventeen)),
        EntryError::ComponentCount(17)
    );
    assert_eq!(
        refused(&with(|e| e.components = vec![])),
        EntryError::ComponentCount(0)
    );
    assert_eq!(
        check(&with(|e| e.components = vec!["Ab.c_d-9".repeat(8)])),
        Ok(())
    );
    for component in ["", "ma in", "ma/in", "ma+in", &"c".repeat(65)] {
        assert_eq!(
            refused(&with(|e| e.components = vec![s(component)])),
            EntryError::Component(s(component))
        );
    }
    assert_eq!(
        refused(&with(|e| e.components = vec![s("main"), s("main")])),
        EntryError::ComponentTwice(s("main"))
    );
}

/// Proves 1 to 8 fingerprints are taken, each 40 or 64 upper-case hex characters, none twice,
/// and that a count of 8 passes the count rule.
#[test]
fn the_fingerprints_are_counted_spelled_and_not_repeated() {
    let print = |n: usize| format!("{n:040X}");
    let eight: Vec<String> = (1..=8).map(print).collect();
    assert_eq!(
        refused(&with(|e| e.fingerprints = eight)),
        EntryError::FingerprintAbsent(print(1))
    );
    let nine: Vec<String> = (1..=9).map(print).collect();
    assert_eq!(
        refused(&with(|e| e.fingerprints = nine)),
        EntryError::FingerprintCount(9)
    );
    assert_eq!(
        refused(&with(|e| e.fingerprints = vec![])),
        EntryError::FingerprintCount(0)
    );
    let bad = [
        "A".repeat(39),
        "A".repeat(41),
        "A".repeat(63),
        "A".repeat(65),
        "a".repeat(40),
        format!("{}G", "A".repeat(39)),
        String::new(),
    ];
    for one in bad {
        assert_eq!(
            refused(&with(|e| e.fingerprints = vec![one.clone()])),
            EntryError::Fingerprint(one)
        );
    }
    assert_eq!(
        refused(&with(|e| e.fingerprints = vec![print(1), print(1)])),
        EntryError::FingerprintTwice(print(1))
    );
}

/// Proves a keyring of 1 to 65536 bytes is taken, and an empty one, one over, and one that is no
/// keyring are refused as the keyring is.
#[test]
fn the_keyring_is_sized_and_parsed() {
    assert_eq!(
        refused(&with(|e| e.keyring = vec![])),
        EntryError::KeyringSize(0)
    );
    let body = v4_body(5, 65536 - 3 - 6);
    let ring = old(6, 2, &body);
    assert_eq!(ring.len(), MAX_KEYRING);
    let print = [v4_print(&body)];
    assert_eq!(check(&with_keys("komira", ring.clone(), &print)), Ok(()));
    let mut over = ring;
    over.push(0);
    assert_eq!(
        refused(&with_keys("komira", over, &print)),
        EntryError::KeyringSize(MAX_KEYRING + 1)
    );
    let junk = refused(&with(|e| e.keyring = b"not a keyring".to_vec()));
    assert_eq!(junk, EntryError::Keyring(KeyringError::Header(0)));
    let mut secret = real::keyring();
    secret.extend(new(5, &v4_body(1, 1)));
    assert!(matches!(
        refused(&with(|e| e.keyring = secret)),
        EntryError::Keyring(KeyringError::Secret(_))
    ));
}

/// Proves every listed fingerprint must be a key or subkey of the keyring, and every key must
/// have its own or a subkey's fingerprint listed.
#[test]
fn fingerprints_and_keyring_must_agree() {
    let other = format!("{:040X}", 7);
    assert_eq!(
        refused(&with(
            |e| e.fingerprints = vec![s(real::PRIMARY), other.clone()]
        )),
        EntryError::FingerprintAbsent(other.clone())
    );
    assert_eq!(
        refused(&with(|e| e.fingerprints = vec![other.clone()])),
        EntryError::FingerprintAbsent(other)
    );
    let (a, b, sub) = (v4_body(1, 3), v4_body(2, 3), v4_body(3, 3));
    let mut ring = key(&a);
    ring.extend(subkey(&sub));
    ring.extend(key(&b));
    let (pa, pb, ps) = (v4_print(&a), v4_print(&b), v4_print(&sub));
    assert_eq!(
        refused(&with_keys(
            "komira",
            ring.clone(),
            std::slice::from_ref(&pa)
        )),
        EntryError::KeyUnlisted(pb.clone())
    );
    assert_eq!(
        refused(&with_keys(
            "komira",
            ring.clone(),
            std::slice::from_ref(&pb)
        )),
        EntryError::KeyUnlisted(pa.clone())
    );
    assert_eq!(
        check(&with_keys(
            "komira",
            ring.clone(),
            &[pa.clone(), pb.clone()]
        )),
        Ok(())
    );
    assert_eq!(check(&with_keys("komira", ring, &[ps, pb])), Ok(()));
}

/// Proves only the keyring's faults are the keyring's: the coordinator answers those
/// `keyring_invalid`.
#[test]
fn only_a_keyring_fault_is_a_keyring_error() {
    let keyring = [
        EntryError::Keyring(KeyringError::NotKey),
        EntryError::FingerprintAbsent(s("A")),
        EntryError::KeyUnlisted(s("A")),
    ];
    for error in keyring {
        assert!(error.is_keyring(), "{error}");
    }
    let other = [
        EntryError::Name(s("n")),
        EntryError::Kind(s("k")),
        EntryError::UriLength(1),
        EntryError::Uri(s("u")),
        EntryError::SuiteCount(1),
        EntryError::Suite(s("s")),
        EntryError::SuiteTwice(s("s")),
        EntryError::Channel(s("c")),
        EntryError::ComponentCount(1),
        EntryError::Component(s("c")),
        EntryError::ComponentTwice(s("c")),
        EntryError::FingerprintCount(1),
        EntryError::Fingerprint(s("f")),
        EntryError::FingerprintTwice(s("f")),
        EntryError::KeyringSize(1),
    ];
    for error in other {
        assert!(!error.is_keyring(), "{error}");
    }
}

/// Proves each error names the value that broke the rule.
#[test]
fn entry_errors_name_the_offending_value() {
    let table = [
        (
            EntryError::Name(s("X")),
            "the name \"X\" is not 1 to 32 bytes of [a-z0-9-]",
        ),
        (
            EntryError::Kind(s("rpm")),
            "the kind \"rpm\" is not \"apt\"",
        ),
        (
            EntryError::UriLength(600),
            "the uri is 600 bytes, at most 512",
        ),
        (
            EntryError::Uri(s("u")),
            "the uri \"u\" is not https://, a host, an optional :port and an optional path",
        ),
        (EntryError::SuiteCount(9), "9 suites, not 1 to 8"),
        (
            EntryError::Suite(s("S")),
            "the suite \"S\" is not 1 to 31 bytes of [a-z0-9-]",
        ),
        (
            EntryError::SuiteTwice(s("a")),
            "the suite \"a\" is listed twice",
        ),
        (
            EntryError::Channel(s("k-a")),
            "the channel \"k-a\" is not 1 to 32 bytes of [a-z0-9-]",
        ),
        (EntryError::ComponentCount(17), "17 components, not 1 to 16"),
        (
            EntryError::Component(s("+")),
            "the component \"+\" is not 1 to 64 bytes of [A-Za-z0-9._-]",
        ),
        (
            EntryError::ComponentTwice(s("m")),
            "the component \"m\" is listed twice",
        ),
        (
            EntryError::FingerprintCount(0),
            "0 fingerprints, not 1 to 8",
        ),
        (
            EntryError::Fingerprint(s("f")),
            "the fingerprint \"f\" is not 40 or 64 upper-case hex characters",
        ),
        (
            EntryError::FingerprintTwice(s("F")),
            "the fingerprint F is listed twice",
        ),
        (
            EntryError::KeyringSize(0),
            "the keyring is 0 bytes, not 1 to 65536",
        ),
        (
            EntryError::Keyring(KeyringError::NotKey),
            "the keyring is not valid: the first packet is not a public key",
        ),
        (
            EntryError::FingerprintAbsent(s("F")),
            "the fingerprint F is no key of the keyring",
        ),
        (
            EntryError::KeyUnlisted(s("F")),
            "the key F of the keyring has neither its own fingerprint nor a subkey's listed",
        ),
    ];
    for (error, text) in table {
        assert_eq!(error.to_string(), text);
    }
}

/// Proves a set of 8 passes, 9 is too many, and the first broken entry is named with its fault.
#[test]
fn a_set_is_counted_and_each_entry_judged() {
    let eight: Vec<Repository> = (1..=8).map(|i| entry(&format!("r{i}"))).collect();
    assert_eq!(check_set(&eight), Ok(()));
    assert_eq!(check_set(&[]), Ok(()));
    let nine: Vec<Repository> = (1..=9).map(|i| entry(&format!("r{i}"))).collect();
    assert_eq!(check_set(&nine), Err(SetError::TooMany(9)));
    let broken = with(|e| e.kind = s("rpm"));
    let err = check_set(&[entry("a"), broken]).expect_err("refused");
    assert_eq!(
        err,
        SetError::Entry {
            name: s("komira"),
            source: EntryError::Kind(s("rpm")),
        }
    );
    assert_eq!(
        err.to_string(),
        "repository \"komira\": the kind \"rpm\" is not \"apt\""
    );
}

/// Proves two entries of one name, and two entries whose channels meet, are refused.
#[test]
fn a_set_names_and_channels_are_unique() {
    assert_eq!(
        check_set(&[entry("a"), entry("a")]),
        Err(SetError::NameTwice(s("a")))
    );
    // `a-b` with suite `c` and `a` with suite `b-c` both make the channel `a-b-c`.
    let first = with(|e| {
        e.name = s("a-b");
        e.suites = vec![s("c")];
    });
    let second = with(|e| {
        e.name = s("a");
        e.suites = vec![s("b-c")];
    });
    assert_eq!(
        check_set(&[first, second]),
        Err(SetError::ChannelTwice(s("a-b-c")))
    );
    assert_eq!(
        SetError::TooMany(9).to_string(),
        "9 repositories, at most 8"
    );
    assert_eq!(
        SetError::NameTwice(s("a")).to_string(),
        "the name \"a\" is used by two repositories"
    );
    assert_eq!(
        SetError::ChannelTwice(s("a-b-c")).to_string(),
        "the channel \"a-b-c\" is used by two repositories"
    );
}

/// Proves `conflict` names the first channel of the entry that another entry has, and ignores
/// the entry's own name.
#[test]
fn a_conflict_is_the_first_channel_another_entry_has() {
    let a = with(|e| {
        e.name = s("a-b");
        e.suites = vec![s("x"), s("c"), s("d")];
    });
    let b = with(|e| {
        e.name = s("a");
        e.suites = vec![s("b-d"), s("b-c")];
    });
    assert_eq!(conflict(&a, std::slice::from_ref(&b)), Some(s("a-b-c")));
    assert_eq!(conflict(&a, &[]), None);
    assert_eq!(conflict(&a, &[entry("other")]), None);
    // The entry's own name, as registered, is not another entry.
    assert_eq!(conflict(&a, std::slice::from_ref(&a)), None);
    assert_eq!(conflict(&a, &[a.clone(), b]), Some(s("a-b-c")));
}

/// Proves a base URL is a valid URI of at most 512 bytes with no trailing slash, a path allowed.
#[test]
fn a_base_uri_is_an_https_uri_without_a_trailing_slash() {
    use hostkeeper_release::registry::is_base_uri;
    assert!(is_base_uri("https://packages.example.com"));
    assert!(is_base_uri("https://packages.example.com:8443/org/repo"));
    assert!(!is_base_uri("https://packages.example.com/"));
    assert!(!is_base_uri("http://packages.example.com"));
    assert!(!is_base_uri(&format!("https://e.com/{}", "a".repeat(512))));
}
