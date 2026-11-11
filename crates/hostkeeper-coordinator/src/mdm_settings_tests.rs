//! The settings of the MDM setup: their names, defaults and what each takes.

use std::collections::BTreeMap;

use super::{SETTINGS, days_of, find, value_of};

fn check(name: &str, value: &str) -> Option<String> {
    find(name).expect("a setting").check(value)
}

/// Proves: the settings this release knows are these five, each with its default from the start
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
            ("installs", "[]"),
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
