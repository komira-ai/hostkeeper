use serde_json::json;

use super::*;

/// A table with every key away from its default.
fn every_key() -> Maintenance {
    Maintenance {
        xcode_keep: 2,
        xcode_default: XcodeDefault::Version("26.1".to_owned()),
        macos_major: true,
        linux_scope: LinuxScope::All,
        batch: 3,
        min_serving: 0,
        drain_deadline_secs: 900,
        return_secs: 1800,
    }
}

fn checked(edit: impl FnOnce(&mut Maintenance)) -> Result<(), String> {
    let mut table = Maintenance::default();
    edit(&mut table);
    table.check()
}

/// Proves: the default table is no Xcode kept, the newest Xcode as default, no major macOS,
/// security updates, a batch of 1, 1 host that must stay serving, no drain deadline and the
/// platform's return time; it is written `{}`, `{}` reads as it, and it passes the check.
///
/// Catches: a default written into every group's file, or a key read with another default than
/// the one the design names.
#[test]
fn the_default_table_is_written_as_nothing_and_read_back_from_nothing() {
    let table = Maintenance::default();
    assert_eq!(
        table,
        Maintenance {
            xcode_keep: 0,
            xcode_default: XcodeDefault::Latest,
            macos_major: false,
            linux_scope: LinuxScope::Security,
            batch: 1,
            min_serving: 1,
            drain_deadline_secs: 0,
            return_secs: 0,
        }
    );
    assert_eq!(serde_json::to_value(&table).expect("serialises"), json!({}));
    assert_eq!(
        serde_json::from_value::<Maintenance>(json!({})).expect("reads"),
        table
    );
    assert_eq!(table.check(), Ok(()));
}

/// Proves: a table with every key away from its default is written with each key under its name,
/// and read back equal; a key at its default beside one that is not is left out.
///
/// Catches: a key that does not survive the file or the database's `jsonb`, or a default written
/// when another key is set.
#[test]
fn a_table_with_every_key_is_written_by_name_and_read_back() {
    let table = every_key();
    let written = serde_json::to_value(&table).expect("serialises");
    assert_eq!(
        written,
        json!({
            "xcode_keep": 2,
            "xcode_default": "26.1",
            "macos_major": true,
            "linux_scope": "all",
            "batch": 3,
            "min_serving": 0,
            "drain_deadline_secs": 900,
            "return_secs": 1800
        })
    );
    assert_eq!(
        serde_json::from_value::<Maintenance>(written).expect("reads"),
        table
    );
    assert_eq!(table.check(), Ok(()));
    let one = Maintenance {
        batch: 2,
        ..Maintenance::default()
    };
    assert_eq!(
        serde_json::to_value(&one).expect("serialises"),
        json!({"batch": 2})
    );
}

/// Proves: a key this build does not know is refused when the table is read, and a Linux scope
/// that is neither `security` nor `all` is refused.
///
/// Catches: a key a newer release added read as absent, its group maintained by a default.
#[test]
fn a_key_this_build_does_not_know_is_refused() {
    let error =
        serde_json::from_value::<Maintenance>(json!({"reboot": true})).expect_err("refused");
    assert!(error.to_string().starts_with("unknown field"), "{error}");
    let error = serde_json::from_value::<Maintenance>(json!({"linux_scope": "kernel"}))
        .expect_err("refused");
    assert!(error.to_string().starts_with("unknown variant"), "{error}");
}

/// Proves: `xcode_default` is `latest` or 1 to 3 numbers of 1 to 4 digits joined by `.`; every
/// other text is refused with the text named; a version reads back as the text it was read from.
///
/// Catches: a version `xcode-select` would be pointed at that is no version (an empty number, a
/// fourth number, a fifth digit, a letter, a sign), or `Latest` taken in another case.
#[test]
fn xcode_default_is_latest_or_a_version_of_up_to_three_numbers() {
    assert_eq!("latest".parse::<XcodeDefault>(), Ok(XcodeDefault::Latest));
    for good in ["26", "26.1", "26.1.1", "9999.0.1"] {
        assert_eq!(
            good.parse::<XcodeDefault>(),
            Ok(XcodeDefault::Version(good.to_owned())),
            "{good:?}"
        );
        assert_eq!(
            XcodeDefault::try_from(good.to_owned()).map(|d| d.to_string()),
            Ok(good.to_owned())
        );
    }
    for bad in [
        "",
        "Latest",
        "26.",
        ".1",
        "27.2.1.9999",
        "12345",
        "26.1b",
        "26 1",
        "-1",
    ] {
        assert_eq!(
            bad.parse::<XcodeDefault>(),
            Err(BadXcodeDefault(bad.to_owned())),
            "{bad:?}"
        );
    }
    assert_eq!(
        "26."
            .parse::<XcodeDefault>()
            .expect_err("refused")
            .to_string(),
        "xcode_default \"26.\" is neither latest nor a version of 1 to 3 numbers of 1 to 4 digits joined by ."
    );
    assert_eq!(String::from(XcodeDefault::Latest), "latest");
    assert_eq!(
        String::from(XcodeDefault::Version("26.1".to_owned())),
        "26.1"
    );
}

/// Proves: a table whose `xcode_default` is not `latest` or a version is refused when it is
/// read, with the text of the refusal; a good one is read.
///
/// Catches: a bad Xcode version stored in a group and met only when a Mac is maintained.
#[test]
fn a_table_with_a_bad_xcode_default_is_refused_when_read() {
    let error = serde_json::from_value::<Maintenance>(json!({"xcode_default": "26.1b"}))
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        "xcode_default \"26.1b\" is neither latest nor a version of 1 to 3 numbers of 1 to 4 digits joined by ."
    );
    assert_eq!(
        serde_json::from_value::<Maintenance>(json!({"xcode_default": "latest"})).expect("reads"),
        Maintenance::default()
    );
}

/// Proves: each key's range is checked at its bounds: the last value inside it is taken and the
/// first outside is refused with the exact reason; a version as `xcode_default` needs an
/// `xcode_keep` of 1 or more; a drain deadline of any length is taken.
///
/// Catches: an off-by-one at a bound (5 Xcodes kept, a batch of 0 or 10001, a `return_secs` of 59
/// or 86401) stored and acted on.
#[test]
fn every_maintenance_key_is_checked_at_its_bounds() {
    let version = || XcodeDefault::Version("26.1".to_owned());
    assert_eq!(checked(|m| m.xcode_keep = 4), Ok(()));
    assert_eq!(
        checked(|m| m.xcode_keep = 5),
        Err("xcode_keep is over 4".to_owned())
    );
    assert_eq!(
        checked(|m| m.xcode_default = version()),
        Err("xcode_default names 26.1, but an xcode_keep of 0 keeps no Xcode".to_owned())
    );
    assert_eq!(
        checked(|m| {
            m.xcode_keep = 1;
            m.xcode_default = version();
        }),
        Ok(())
    );
    for taken in [1, 10_000] {
        assert_eq!(checked(|m| m.batch = taken), Ok(()), "{taken}");
    }
    for refused in [0, 10_001] {
        assert_eq!(
            checked(|m| m.batch = refused),
            Err("batch is not 1 to 10000".to_owned()),
            "{refused}"
        );
    }
    for taken in [0, 10_000] {
        assert_eq!(checked(|m| m.min_serving = taken), Ok(()), "{taken}");
    }
    assert_eq!(
        checked(|m| m.min_serving = 10_001),
        Err("min_serving is over 10000".to_owned())
    );
    for taken in [0, 60, 86_400] {
        assert_eq!(checked(|m| m.return_secs = taken), Ok(()), "{taken}");
    }
    for refused in [59, 86_401] {
        assert_eq!(
            checked(|m| m.return_secs = refused),
            Err("return_secs is not 0 or 60 to 86400".to_owned()),
            "{refused}"
        );
    }
    assert_eq!(checked(|m| m.drain_deadline_secs = u64::MAX), Ok(()));
}

/// Proves: when several keys are out of range, the first in the order of the table is the one
/// named.
///
/// Catches: a refusal that names a later key than the first, which an operator fixing one key at
/// a time would meet twice.
#[test]
fn the_first_key_out_of_range_in_the_tables_order_is_the_one_named() {
    assert_eq!(
        checked(|m| {
            m.xcode_keep = 5;
            m.batch = 0;
            m.return_secs = 1;
        }),
        Err("xcode_keep is over 4".to_owned())
    );
    assert_eq!(
        checked(|m| {
            m.batch = 0;
            m.min_serving = 10_001;
            m.return_secs = 1;
        }),
        Err("batch is not 1 to 10000".to_owned())
    );
    assert_eq!(
        checked(|m| {
            m.min_serving = 10_001;
            m.return_secs = 1;
        }),
        Err("min_serving is over 10000".to_owned())
    );
}
