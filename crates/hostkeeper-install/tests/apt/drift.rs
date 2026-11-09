//! Drift (packages-repositories.md section 4.4): a package at a version hostkeeper did not set,
//! reported by the heartbeat's check and repaired by the next activate.

use hostkeeper_install::repository::{Drift, PackageRecord};

use crate::harness::*;

fn record(version: &str, pending: bool) -> PackageRecord {
    PackageRecord {
        installer: "apt".to_owned(),
        package: PACKAGE.to_owned(),
        version: version.to_owned(),
        repository: REPO.to_owned(),
        links: Default::default(),
        pending,
    }
}

fn some(found: Option<&str>, recorded: Option<&str>) -> Option<Drift> {
    Some(Drift {
        found: found.map(str::to_owned),
        recorded: recorded.map(str::to_owned),
    })
}

fn check(env: &Env) -> Option<Drift> {
    env.installer()
        .drift(&service(SERVICE), PACKAGE, 5)
        .expect("the check runs")
}

/// Proves a package moved by hand to another version is reported with the version found and the
/// one recorded, and the next activate of the desired version installs it over the other (a
/// downgrade here), names the drift in `Activated.drift`, holds and records it, and the check
/// then reports none.
#[test]
fn a_version_moved_by_hand_is_reported_and_repaired() {
    let env = Env::new("apt-drift-moved");
    let (order, dir) = env.installed(&deb(V1), &["mdm.service"]);
    assert_eq!(check(&env), None);
    env.host.set_installed(PACKAGE, "2.0-1", 'i', false);
    assert_eq!(check(&env), some(Some("2.0-1"), Some(V1)));
    let activated = env.installer().activate(&order, &dir).expect("repairs");
    assert_eq!(activated.drift, some(Some("2.0-1"), Some(V1)));
    let now = env.host.installed(PACKAGE).expect("installed");
    assert_eq!((now.version.as_str(), now.held), (V1, true));
    assert_eq!(check(&env), None);
}

/// Proves a package installed with no record is drift with `recorded` none, and an activate
/// repairs it and records the version.
#[test]
fn a_package_with_no_record_is_drift() {
    let env = Env::new("apt-drift-unrecorded");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.host.set_installed(PACKAGE, "0.5-1", 'i', false);
    assert_eq!(check(&env), some(Some("0.5-1"), None));
    let activated = env.installer().activate(&order, &dir).expect("repairs");
    assert_eq!(activated.drift, some(Some("0.5-1"), None));
    assert_eq!(env.records.get(SERVICE).expect("record").version, V1);
}

/// Proves a recorded package that is not installed any more is drift with `found` none.
#[test]
fn a_package_removed_by_hand_is_drift() {
    let env = Env::new("apt-drift-removed");
    let (_, _) = env.installed(&deb(V1), &["mdm.service"]);
    env.host.edit(|s| {
        s.installed.clear();
    });
    assert_eq!(check(&env), some(None, Some(V1)));
}

/// Proves a pending record is no drift, whatever the database shows: the install it belongs to
/// is being run again.
#[test]
fn a_pending_record_is_no_drift() {
    let env = Env::new("apt-drift-pending");
    env.records.put(SERVICE, record(V2, true));
    env.host.set_installed(PACKAGE, V1, 'i', true);
    assert_eq!(check(&env), None);
}

/// Proves no record and no package is no drift, and that without a record the check asks about
/// the package the manifest names.
#[test]
fn nothing_recorded_and_nothing_installed_is_no_drift() {
    let env = Env::new("apt-drift-none");
    assert_eq!(check(&env), None);
    assert_eq!(
        env.host.calls()[0].run.args.last().map(String::as_str),
        Some(PACKAGE)
    );
}

/// Proves a package that is half-configured at the recorded version is still that version (the
/// check compares versions; a half-installed package is the install's to report).
#[test]
fn the_check_compares_versions() {
    let env = Env::new("apt-drift-version");
    env.records.put(SERVICE, record(V1, false));
    env.host.set_installed(PACKAGE, V1, 'F', true);
    assert_eq!(check(&env), None);
}
