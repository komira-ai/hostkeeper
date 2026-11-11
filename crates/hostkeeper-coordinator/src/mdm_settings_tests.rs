//! The settings of the MDM setup: their names, defaults and what each takes.

use std::collections::BTreeMap;

use super::{SETTINGS, days_of, find, number_of, text_of, value_of};

fn check(name: &str, value: &str) -> Option<String> {
    find(name).expect("a setting").check(value)
}

/// Proves: the settings this release knows are these eight, each with its default from the start
/// (design 4.9), and a name outside them is none.
///
/// Catches: a tunable with no default, which would make the setup page a precondition.
#[test]
fn each_setting_has_a_default_from_the_start() {
    let known: Vec<(&str, &str)> = SETTINGS.iter().map(|s| (s.name, s.default)).collect();
    assert_eq!(
        known,
        [
            ("push_alert_days", "60,14"),
            ("abm_alert_days", "60"),
            ("tls_renew_days", "30"),
            ("secret_grace_days", "7"),
            ("abm_sync_s", "300"),
            ("abm_request_timeout_s", "30"),
            ("abm_retries", "3"),
            ("ade_profile_name", "Komira MDM"),
        ]
    );
    for setting in SETTINGS {
        assert_eq!(
            setting.check(setting.default).as_deref(),
            Some(setting.default),
            "{} takes its own default",
            setting.name
        );
    }
    assert!(find("mdm_name").is_none());
    assert!(find("abm_key").is_none());
}

/// Proves: a number of days is taken within its range and written without leading zeros or
/// space, and refused when it has a sign, a fraction, an exponent or no digit.
///
/// Catches: a value a later reader parses differently than the setter judged it.
#[test]
fn a_number_of_days_is_judged_once_when_it_is_set() {
    assert_eq!(check("tls_renew_days", " 045 ").as_deref(), Some("45"));
    assert_eq!(check("tls_renew_days", "1").as_deref(), Some("1"));
    assert_eq!(check("tls_renew_days", "365").as_deref(), Some("365"));
    for bad in [
        "0",
        "366",
        "-5",
        "+5",
        "1.5",
        "1e2",
        "",
        " ",
        "x",
        "99999999999",
    ] {
        assert_eq!(check("tls_renew_days", bad), None, "{bad:?}");
    }
    assert_eq!(check("secret_grace_days", "0").as_deref(), Some("0"));
    assert_eq!(check("secret_grace_days", "366"), None);
}

/// Proves: a list of alert days is one to five falling numbers from 1 to 3650, written without
/// space; a list that rises, repeats, is longer, has an empty part or a part out of range is
/// refused.
///
/// Catches: an alert list that never fires in order, or that has an empty entry.
#[test]
fn a_list_of_alert_days_falls() {
    assert_eq!(
        check("push_alert_days", " 90 , 30,7 ").as_deref(),
        Some("90,30,7")
    );
    assert_eq!(check("abm_alert_days", "3650").as_deref(), Some("3650"));
    assert_eq!(
        check("push_alert_days", "5,4,3,2,1").as_deref(),
        Some("5,4,3,2,1")
    );
    for bad in [
        "14,60",
        "60,60",
        "6,5,4,3,2,1",
        "60,",
        ",60",
        "",
        "0",
        "3651",
        "60;14",
    ] {
        assert_eq!(check("push_alert_days", bad), None, "{bad:?}");
    }
}

/// Proves: a setting that was not set has its default, a set one its value, and a number is read
/// from the value with the fallback for a name this release does not know or a value a person
/// edited into the store.
///
/// Catches: a reader that fails a whole setup on one hand-edited row.
#[test]
fn a_setting_not_set_is_its_default() {
    let mut set = BTreeMap::new();
    let grace = find("secret_grace_days").expect("a setting");
    assert_eq!(value_of(&set, grace), "7");
    assert_eq!(days_of(&set, "secret_grace_days", 99), 7);
    set.insert("secret_grace_days".to_owned(), "2".to_owned());
    assert_eq!(value_of(&set, grace), "2");
    assert_eq!(days_of(&set, "secret_grace_days", 99), 2);
    set.insert("secret_grace_days".to_owned(), "two".to_owned());
    assert_eq!(days_of(&set, "secret_grace_days", 99), 99);
    assert_eq!(days_of(&set, "nothing", 5), 5);
}

/// Proves: the counts of the ABM sync are whole numbers within their range, written without leading
/// zeros, and refused outside it or with anything but digits; `number_of` reads one with the
/// fallback `days_of` gives.
///
/// Catches: a sync interval of zero (a request in a loop) or of a week that nothing would notice.
#[test]
fn the_counts_of_the_abm_sync_are_judged_within_their_range() {
    assert_eq!(check("abm_sync_s", " 0300 ").as_deref(), Some("300"));
    assert_eq!(check("abm_sync_s", "60").as_deref(), Some("60"));
    assert_eq!(check("abm_sync_s", "86400").as_deref(), Some("86400"));
    for bad in ["59", "86401", "-1", "1.5", "", "x"] {
        assert_eq!(check("abm_sync_s", bad), None, "{bad:?}");
    }
    assert_eq!(check("abm_request_timeout_s", "5").as_deref(), Some("5"));
    assert_eq!(check("abm_request_timeout_s", "301"), None);
    assert_eq!(check("abm_retries", "0").as_deref(), Some("0"));
    assert_eq!(check("abm_retries", "10").as_deref(), Some("10"));
    assert_eq!(check("abm_retries", "11"), None);
    let mut set = BTreeMap::new();
    assert_eq!(number_of(&set, "abm_sync_s", 1), 300);
    set.insert("abm_sync_s".to_owned(), "120".to_owned());
    assert_eq!(number_of(&set, "abm_sync_s", 1), 120);
    set.insert("abm_sync_s".to_owned(), "soon".to_owned());
    assert_eq!(number_of(&set, "abm_sync_s", 1), 1);
}

/// Proves: the profile name is 1 to 64 printable characters (space and non-ASCII letters allowed),
/// kept as written but for the whitespace around it, and refused empty, over 64 characters or with
/// a control character; `text_of` reads a setting's text, its default when unset, and none for a
/// name this release does not know.
///
/// Catches: a profile name ABM would refuse and a sync that then fails every five minutes.
#[test]
fn the_profile_name_is_a_short_printable_text() {
    assert_eq!(
        check("ade_profile_name", "  Mac Ünit 1 ").as_deref(),
        Some("Mac Ünit 1")
    );
    assert_eq!(
        check("ade_profile_name", &"n".repeat(64)).as_deref(),
        Some(&*"n".repeat(64))
    );
    for bad in ["", "   ", &"n".repeat(65), "a\nb", "tab\there"] {
        assert_eq!(check("ade_profile_name", bad), None, "{bad:?}");
    }
    let mut set = BTreeMap::new();
    assert_eq!(text_of(&set, "ade_profile_name"), Some("Komira MDM"));
    set.insert("ade_profile_name".to_owned(), "Other".to_owned());
    assert_eq!(text_of(&set, "ade_profile_name"), Some("Other"));
    assert_eq!(text_of(&set, "nothing"), None);
}
