//! The compiled table of the rollback of hostkeeper below an installer of what the host holds.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_placed::{InstalledEntry, PlacedRecord};
use hostkeeper_release::Installer;

use super::{INSTALLER_FLOORS, SELF_NATIVE_FLOOR, below, floor_of};

fn v(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).unwrap()
}

/// A `placed.json` whose `installed` entries are `entries`, `(service, installer)`.
fn holding(entries: &[(&str, &str)]) -> PlacedRecord {
    let mut record = PlacedRecord::default();
    for (service, installer) in entries {
        record
            .installed
            .insert((*service).to_owned(), InstalledEntry::new(installer));
    }
    record
}

/// Each installer's row: `hostkeeper-archive` has every release, `apt` the release its steps
/// came in, and an installer this release has no steps of has no row.
#[test]
fn each_installer_has_the_release_it_arrived_in() {
    assert_eq!(floor_of(Installer::HostkeeperArchive), Some(v("0.0.0")));
    assert_eq!(floor_of(Installer::Apt), Some(INSTALLER_FLOORS[0].1));
    for none in [Installer::MacosPkg, Installer::Tarball, Installer::OciImage] {
        assert_eq!(floor_of(none), None, "{none:?}");
    }
    assert!(SELF_NATIVE_FLOOR > INSTALLER_FLOORS[0].1);
}

/// A release that is not older than the binary that runs, or whose version is not
/// `major.minor.patch`, is no rollback, and is never refused here; nor is a rollback on a host
/// that holds only archive versions.
#[test]
fn only_a_rollback_is_judged() {
    let record = holding(&[("web", "apt"), ("hostkeeper", "apt")]);
    let running = v("0.2.0");
    for target in ["0.2.0", "0.3.0", "not.a.version", ""] {
        assert_eq!(below(&record, running, target), None, "{target}");
    }
    assert_eq!(below(&PlacedRecord::default(), running, "0.0.1"), None);
}

/// On a host where hostkeeper itself runs from its native package, a rollback below the first
/// release that updates itself through one is refused, and one to that release goes on; the same
/// rollback where hostkeeper runs from archives is judged by the other services alone.
#[test]
fn a_rollback_below_the_native_self_update_is_refused_where_hostkeeper_moved() {
    let running = v("1.0.0");
    let moved = holding(&[("hostkeeper", "apt")]);
    let below_floor = HostkeeperVersion {
        patch: SELF_NATIVE_FLOOR.patch - 1,
        ..SELF_NATIVE_FLOOR
    };
    assert_eq!(
        below(&moved, running, &below_floor.to_string()),
        Some(format!(
            "hostkeeper runs from its native package here, which no release before \
             {SELF_NATIVE_FLOOR} updates; {below_floor} is older"
        ))
    );
    assert_eq!(below(&moved, running, &SELF_NATIVE_FLOOR.to_string()), None);
    let unknown = holding(&[("hostkeeper", "later-format")]);
    assert!(below(&unknown, running, &below_floor.to_string()).is_some());
}

/// A rollback below the release an installer of a service's version arrived in is refused,
/// naming the service; one to that release goes on; an installer with no row refuses every
/// rollback.
#[test]
fn a_rollback_below_an_installer_of_a_service_is_refused() {
    let running = v("1.0.0");
    let apt = INSTALLER_FLOORS[0].1;
    let older = HostkeeperVersion {
        patch: apt.patch - 1,
        ..apt
    };
    let record = holding(&[("web", "apt")]);
    assert_eq!(
        below(&record, running, &older.to_string()),
        Some(format!(
            "web is installed by apt, which no release before {apt} has; {older} is older"
        ))
    );
    assert_eq!(below(&record, running, &apt.to_string()), None);
    let later = holding(&[("db", "later-format")]);
    assert_eq!(
        below(&later, running, "0.9.0"),
        Some(
            "db is installed by \"later-format\", which this release has no steps of; 0.9.0 is \
             older than this release"
                .to_owned()
        )
    );
}
