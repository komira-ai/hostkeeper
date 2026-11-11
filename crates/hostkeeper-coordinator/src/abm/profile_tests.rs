//! The ADE profile ([`crate::abm::profile`]): what it says, that its digest follows what it says,
//! and which names an enrollment URL may carry.

use serde_json::json;

use crate::abm::profile::{AdeProfile, is_host_name};

/// Proves: the profile sends a Mac to `https://<name>/enroll`, is supervised and mandatory, cannot
/// be removed, does not hold Setup Assistant for the MDM, allows pairing, carries no anchor
/// certificate, skips the 18 panes of the kit's list and nothing outside it, and is the same JSON
/// and the same digest each time; a different name or host is a different digest.
///
/// Catches: a profile a Mac can leave the MDM with, and a digest that does not change when the
/// profile does (so that the change is never sent to ABM).
#[test]
fn the_profile_keeps_a_mac_enrolled_and_its_digest_follows_it() {
    let profile = AdeProfile::new("Komira MDM", "mdm.example.com");
    let value = profile.json();
    assert_eq!(value["profile_name"], "Komira MDM");
    assert_eq!(value["url"], "https://mdm.example.com/enroll");
    for (flag, want) in [
        ("is_supervised", true),
        ("is_mandatory", true),
        ("is_mdm_removable", false),
        ("await_device_configured", false),
        ("auto_advance_setup", true),
        ("allow_pairing", true),
    ] {
        assert_eq!(value[flag], json!(want), "{flag}");
    }
    assert_eq!(value["anchor_certs"], json!([]));
    let items = value["skip_setup_items"].as_array().expect("a list");
    assert_eq!(items.len(), 18);
    assert_eq!(items[0], "Accessibility");
    assert_eq!(items[17], "Wallpaper");
    let keys: Vec<&String> = value.as_object().expect("an object").keys().collect();
    assert_eq!(keys.len(), 10);

    let again = AdeProfile::new("Komira MDM", "mdm.example.com");
    assert_eq!(again, profile);
    assert_eq!(again.digest(), profile.digest());
    assert_eq!(profile.digest().len(), 64);
    assert_ne!(
        AdeProfile::new("Other", "mdm.example.com").digest(),
        profile.digest()
    );
    assert_ne!(
        AdeProfile::new("Komira MDM", "other.example").digest(),
        profile.digest()
    );
}

/// Proves: a host name is dot-separated labels of letters, digits and `-` that neither begin nor
/// end with `-`, 1 to 63 characters each and 253 in all; anything else (empty, a space, a slash, a
/// port, an underscore, a leading or trailing dot, an empty label) is not.
///
/// Catches: a setting that makes the profile send Macs to a URL that is not the MDM, such as a
/// name with a path or a port in it.
#[test]
fn only_a_host_name_may_be_the_enrollment_host() {
    for good in [
        "mdm.example.com",
        "a",
        "A-1.b2",
        "xn--bcher-kva.example",
        &"a".repeat(63),
    ] {
        assert!(is_host_name(good), "{good}");
    }
    let long = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    assert!(is_host_name(&long[..253]));
    for bad in [
        "",
        "a b",
        "a/b",
        "a:443",
        "a_b.example",
        ".a",
        "a.",
        "a..b",
        "-a.example",
        "a-.example",
        &"a".repeat(64),
        &format!("{long}x"),
    ] {
        assert!(!is_host_name(bad), "{bad}");
    }
}
