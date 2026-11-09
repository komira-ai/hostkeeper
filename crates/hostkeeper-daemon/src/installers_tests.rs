//! A service with no installer recorded has none; a recorded `apt` service has the repository
//! installer's backend; an installer the record names that this build has no backend of, or that
//! is no installer, is refused with what is recorded.

use hostkeeper_config::names::ServiceName;
use hostkeeper_placed::{InstalledEntry, PlacedRecord};
use hostkeeper_release::Installer;

use super::{backend_for, installer_of};

fn web() -> ServiceName {
    ServiceName::new("web").expect("a name")
}

#[test]
fn a_service_with_no_installer_recorded_has_none() {
    let record = PlacedRecord::default();
    assert_eq!(installer_of(&record, &web()), Ok(None));
    assert!(matches!(backend_for(&record, &web()), Ok(None)));
}

#[test]
fn a_recorded_installer_is_read_apt_has_its_backend_and_one_without_one_is_refused() {
    let mut record = PlacedRecord::default();
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("apt"));
    assert_eq!(installer_of(&record, &web()), Ok(Some(Installer::Apt)));
    let backend = backend_for(&record, &web())
        .expect("a backend")
        .expect("apt's");
    assert_eq!(backend.installer(), Installer::Apt);
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("macos-pkg"));
    assert_eq!(installer_of(&record, &web()), Ok(Some(Installer::MacosPkg)));
    assert_eq!(
        backend_for(&record, &web()).err(),
        Some(
            "web is installed by the installer \"macos-pkg\", which this daemon has no steps of"
                .to_owned()
        )
    );
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("later-format"));
    assert_eq!(
        installer_of(&record, &web()),
        Err(
            "web is installed by the installer \"later-format\", which this daemon has no steps of"
                .to_owned()
        )
    );
    assert!(backend_for(&record, &web()).is_err());
}
