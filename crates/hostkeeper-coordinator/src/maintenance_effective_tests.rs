use hostkeeper_wire::Platform;

use super::*;
use crate::host_groups::tests::web;
use crate::maintenance_policy::Maintenance;

/// A group whose maintenance table is `table`.
fn group_with(table: Maintenance) -> HostGroup {
    let mut group = web();
    group.policy.maintenance = table;
    group
}

/// Proves: a group with the default policy resolves to every default: no Xcode kept, `latest`,
/// no major macOS, security updates, a batch of 1, 1 serving, no drain deadline, and a
/// `return_secs` of 3600 on a Mac and 1200 on either Linux platform.
///
/// Catches: a default resolved wrong, or one platform's return time given to another.
#[test]
fn a_group_with_the_default_policy_resolves_to_every_default() {
    let group = web();
    let want = |return_secs| Effective {
        xcode_keep: 0,
        xcode_default: XcodeDefault::Latest,
        macos_major: false,
        linux_scope: LinuxScope::Security,
        batch: 1,
        min_serving: 1,
        drain_deadline_secs: None,
        return_secs,
    };
    assert_eq!(effective(&group, Platform::DarwinArm64), want(3600));
    assert_eq!(effective(&group, Platform::LinuxX86_64), want(1200));
    assert_eq!(effective(&group, Platform::LinuxArm64), want(1200));
}

/// Proves: a group that names every key resolves to those keys, a drain deadline as `Some`, and
/// its `return_secs` on every platform.
///
/// Catches: a key of the table dropped on the way to a run, or the platform default winning over
/// the group's `return_secs`.
#[test]
fn a_group_naming_every_key_resolves_to_them() {
    let group = group_with(Maintenance {
        xcode_keep: 2,
        xcode_default: XcodeDefault::Version("26.1".to_owned()),
        macos_major: true,
        linux_scope: LinuxScope::All,
        batch: 3,
        min_serving: 0,
        drain_deadline_secs: 900,
        return_secs: 1800,
    });
    for platform in [
        Platform::DarwinArm64,
        Platform::LinuxX86_64,
        Platform::LinuxArm64,
    ] {
        assert_eq!(
            effective(&group, platform),
            Effective {
                xcode_keep: 2,
                xcode_default: XcodeDefault::Version("26.1".to_owned()),
                macos_major: true,
                linux_scope: LinuxScope::All,
                batch: 3,
                min_serving: 0,
                drain_deadline_secs: Some(900),
                return_secs: 1800,
            },
            "{platform:?}"
        );
    }
}

/// Proves: a group whose table names every default explicitly (a batch of 1, `latest`, a
/// `return_secs` of the platform's own) resolves the same as a group with no table.
///
/// Catches: a group that says its defaults reading differently from one that says nothing.
#[test]
fn a_group_naming_every_default_resolves_as_one_naming_none() {
    let none = web();
    let named = group_with(Maintenance {
        xcode_keep: 0,
        xcode_default: XcodeDefault::Latest,
        macos_major: false,
        linux_scope: LinuxScope::Security,
        batch: 1,
        min_serving: 1,
        drain_deadline_secs: 0,
        return_secs: 0,
    });
    for platform in [
        Platform::DarwinArm64,
        Platform::LinuxX86_64,
        Platform::LinuxArm64,
    ] {
        assert_eq!(effective(&named, platform), effective(&none, platform));
    }
    let on_a_mac = group_with(Maintenance {
        return_secs: 3600,
        ..Maintenance::default()
    });
    assert_eq!(
        effective(&on_a_mac, Platform::DarwinArm64),
        effective(&none, Platform::DarwinArm64)
    );
}
