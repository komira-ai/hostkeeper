//! The grammars that keep a value from becoming something else in a systemd property, as far as
//! the declaration left for `launch.rs` still has them. The service name and path cases moved
//! with their grammar to `hostkeeper-config` (`crates/hostkeeper-config/tests/names.rs`).

use hostkeeper_rights::names::{Capability, Family, is_account_name, is_env_name, is_env_value};

/// Catches an account name that no account has: a letter or `_` first, then letters, digits,
/// `_` and `-`, at most 32 bytes.
#[test]
fn account_names() {
    for good in [
        "root",
        "_apt",
        "tierstore-update",
        "a1_b-c",
        &"a".repeat(32),
    ] {
        assert!(is_account_name(good), "{good}");
    }
    for bad in [
        "",
        "1abc",
        "-a",
        "Root",
        "a b",
        "a:b",
        "a.b",
        &"a".repeat(33),
    ] {
        assert!(!is_account_name(bad), "{bad}");
    }
}

/// Catches an environment entry that systemd would split, quote or expand.
#[test]
fn environment_entries() {
    for good in ["A", "_a", "A_1", "Path"] {
        assert!(is_env_name(good), "{good}");
    }
    for bad in ["", "1A", "A-B", "A B", "A=B"] {
        assert!(!is_env_name(bad), "{bad}");
    }
    for good in ["", "x", "a/b:c@d,e+f=g-h_i.j", &"v".repeat(256)] {
        assert!(is_env_value(good), "{good}");
    }
    for bad in [
        "a b",
        "a\"b",
        "$HOME",
        "%h",
        "a\\b",
        "a\nb",
        "é",
        &"v".repeat(257),
    ] {
        assert!(!is_env_value(bad), "{bad}");
    }
}

/// Catches a capability or family name that is not systemd's spelling.
#[test]
fn capability_and_family_names() {
    let cap = Capability::try_from("CAP_NET_ADMIN".to_owned()).unwrap();
    assert_eq!(cap.as_str(), "CAP_NET_ADMIN");
    assert_eq!(cap.to_string(), "CAP_NET_ADMIN");
    assert_eq!(String::from(cap), "CAP_NET_ADMIN");
    let family = Family::try_from("AF_UNIX".to_owned()).unwrap();
    assert_eq!(family.as_str(), "AF_UNIX");
    for bad in [
        "",
        "CAP_",
        "cap_net_admin",
        "NET_ADMIN",
        "CAP_NET ADMIN",
        "~CAP_X",
        "AF_X",
    ] {
        assert!(Capability::try_from(bad.to_owned()).is_err(), "{bad}");
    }
    for bad in ["", "AF_", "af_unix", "UNIX", "AF_UN IX", "CAP_X"] {
        assert!(Family::try_from(bad.to_owned()).is_err(), "{bad}");
    }
    let message = Family::try_from("x".to_owned()).unwrap_err();
    assert!(message.contains("AF_"), "{message}");
}
