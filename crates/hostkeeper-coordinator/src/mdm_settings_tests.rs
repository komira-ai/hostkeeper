//! The settings of the MDM setup: their names, defaults and what each takes.

use std::collections::BTreeMap;

use super::{SETTINGS, find, list_of, number_of, text_of, value_of, whole_of};

fn check(name: &str, value: &str) -> Option<String> {
    find(name).expect("a setting").check(value)
}

/// Proves: the settings this release knows are these eighteen, each with its default from the start
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
            ("push_retry_s", "3600"),
            ("push_expiry_s", "86400"),
            ("push_priority", "10"),
            ("push_timeout_s", "30"),
            ("installs", "[]"),
            ("proxy_wait_s", "10"),
            ("proxy_redirects", "3"),
            ("proxy_index_ttl_s", "300"),
            ("proxy_cache_max_bytes", "21474836480"),
            ("proxy_fetches", "8"),
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
    assert_eq!(number_of(&set, "secret_grace_days", 99), 7);
    set.insert("secret_grace_days".to_owned(), "2".to_owned());
    assert_eq!(value_of(&set, grace), "2");
    assert_eq!(number_of(&set, "secret_grace_days", 99), 2);
    set.insert("secret_grace_days".to_owned(), "two".to_owned());
    assert_eq!(number_of(&set, "secret_grace_days", 99), 99);
    assert_eq!(number_of(&set, "nothing", 5), 5);
}

/// Proves: the push settings take a whole number of seconds in their range (the expiry may be 0,
/// "try once"), a priority is 5 or 10 and nothing else, and each is written as the store keeps it.
///
/// Catches: a retry floor of 0 that pushes in a loop, a priority APNs refuses, and a value a later
/// reader parses differently than the setter judged it.
#[test]
fn the_push_settings_are_judged_once_when_they_are_set() {
    assert_eq!(check("push_retry_s", " 0600 ").as_deref(), Some("600"));
    assert_eq!(check("push_retry_s", "0"), None);
    assert_eq!(check("push_retry_s", "2592000").as_deref(), Some("2592000"));
    assert_eq!(check("push_retry_s", "2592001"), None);
    assert_eq!(check("push_expiry_s", "0").as_deref(), Some("0"));
    assert_eq!(check("push_expiry_s", "2592001"), None);
    assert_eq!(check("push_expiry_s", "-1"), None);
    assert_eq!(check("push_timeout_s", "300").as_deref(), Some("300"));
    assert_eq!(check("push_timeout_s", "0"), None);
    assert_eq!(check("push_timeout_s", "301"), None);
    assert_eq!(check("push_priority", " 5 ").as_deref(), Some("5"));
    assert_eq!(check("push_priority", "10").as_deref(), Some("10"));
    for bad in ["1", "6", "05", "ten", ""] {
        assert_eq!(check("push_priority", bad), None, "{bad:?}");
    }
}

/// Proves: a list setting reads as its numbers, a value a hand edit broke falls back to the default
/// list, and a name with no list is none.
///
/// Catches: an alert threshold list that is empty because the store holds text.
#[test]
fn a_list_setting_reads_as_numbers_and_falls_back_to_its_default() {
    let mut set = BTreeMap::new();
    assert_eq!(list_of(&set, "push_alert_days"), [60, 14]);
    set.insert("push_alert_days".to_owned(), "90, 30,7".to_owned());
    assert_eq!(list_of(&set, "push_alert_days"), [90, 30, 7]);
    set.insert("push_alert_days".to_owned(), "soon".to_owned());
    assert_eq!(list_of(&set, "push_alert_days"), [60, 14]);
    assert_eq!(list_of(&set, "nothing"), Vec::<u32>::new());
}

/// Proves: the setting `installs` takes the JSON of named installs the gate reads, keeps it as
/// the gate writes it back (an unknown key from a later release dropped, the order kept), and
/// refuses text the gate would refuse; its default is no install.
///
/// Catches: an installs value stored that the gate then fails to start with, or one stored in a
/// form that differs from the form the gate reads back.
#[test]
fn the_installs_setting_takes_what_the_gate_reads() {
    let sha = "a".repeat(64);
    let entry = format!(
        r#"{{"name":"hostkeeper","bundle_id":"dev.hostkeeper.hostkeeper","sha256":"{sha}","version":"0.1.66","path":"/var/lib/hostkeeper-mdm-pkg/{sha}.pkg"}}"#
    );
    let with_extra = entry.replace(r#""name""#, r#""later":true,"name""#);
    assert_eq!(
        check("installs", &format!(" [{with_extra}] ")),
        Some(format!("[{entry}]"))
    );
    assert_eq!(check("installs", "[]").as_deref(), Some("[]"));
    for bad in ["", "{}", "[{}]", &format!("[{entry},{entry}]")] {
        assert_eq!(check("installs", bad), None, "{bad:?}");
    }
    assert_eq!(super::INSTALLS, "installs");
}

/// Proves: each proxy setting takes a whole number within its range (the bounds included), written
/// without leading zeros or space, and refuses a sign, a fraction, an empty value or a number out
/// of range; a number is read with the fallback for an unknown name or a hand-edited value.
///
/// Catches: a proxy tunable that takes `0` where zero would stall every request, or a value a
/// reader parses differently than the setter judged it.
#[test]
fn a_proxy_setting_is_a_whole_number_in_its_range() {
    assert_eq!(check("proxy_wait_s", " 010 ").as_deref(), Some("10"));
    assert_eq!(check("proxy_wait_s", "1").as_deref(), Some("1"));
    assert_eq!(check("proxy_wait_s", "3600").as_deref(), Some("3600"));
    assert_eq!(check("proxy_redirects", "0").as_deref(), Some("0"));
    assert_eq!(check("proxy_index_ttl_s", "0").as_deref(), Some("0"));
    assert_eq!(check("proxy_fetches", "1024").as_deref(), Some("1024"));
    assert_eq!(
        check("proxy_cache_max_bytes", "1125899906842624").as_deref(),
        Some("1125899906842624")
    );
    for (name, bad) in [
        ("proxy_wait_s", "0"),
        ("proxy_wait_s", "3601"),
        ("proxy_wait_s", "-1"),
        ("proxy_wait_s", "1.5"),
        ("proxy_wait_s", ""),
        ("proxy_redirects", "11"),
        ("proxy_index_ttl_s", "86401"),
        ("proxy_fetches", "0"),
        ("proxy_fetches", "1025"),
        ("proxy_cache_max_bytes", "0"),
        ("proxy_cache_max_bytes", "1125899906842625"),
        ("proxy_cache_max_bytes", "99999999999999999999999"),
    ] {
        assert_eq!(check(name, bad), None, "{name} {bad:?}");
    }

    let mut set = BTreeMap::new();
    assert_eq!(whole_of(&set, "proxy_fetches", 99), 8);
    set.insert("proxy_fetches".to_owned(), "16".to_owned());
    assert_eq!(whole_of(&set, "proxy_fetches", 99), 16);
    set.insert("proxy_fetches".to_owned(), "many".to_owned());
    assert_eq!(whole_of(&set, "proxy_fetches", 99), 99);
    assert_eq!(whole_of(&set, "nothing", 5), 5);
}

/// Proves: the counts of the ABM sync are whole numbers within their range, written without leading
/// zeros, and refused outside it or with anything but digits; `number_of` reads one with the
/// fallback `number_of` gives.
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
