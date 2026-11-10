//! A service with no installer recorded has none; a recorded `apt` service has the repository
//! installer's backend and a `macos-pkg` one the `macos-pkg` installer's; an installer the record
//! names that is no installer of this build is refused with what is recorded.

use hostkeeper_config::names::ServiceName;
use hostkeeper_placed::{InstalledEntry, PlacedRecord};
use hostkeeper_release::Installer;

use super::{Steps, installer_of, steps_for};

fn web() -> ServiceName {
    ServiceName::new("web").expect("a name")
}

#[test]
fn a_service_with_no_installer_recorded_has_none() {
    let record = PlacedRecord::default();
    assert_eq!(installer_of(&record, &web()), Ok(None));
    assert!(matches!(steps_for(&record, &web()), Ok(None)));
}

#[test]
fn a_recorded_installer_is_read_apt_and_macos_pkg_have_steps_and_one_without_is_refused() {
    let mut record = PlacedRecord::default();
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("apt"));
    assert_eq!(installer_of(&record, &web()), Ok(Some(Installer::Apt)));
    let steps = steps_for(&record, &web()).expect("steps").expect("apt's");
    assert!(matches!(steps, Steps::Repository(_)));
    assert_eq!(steps.installer(), Installer::Apt);
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("macos-pkg"));
    assert_eq!(installer_of(&record, &web()), Ok(Some(Installer::MacosPkg)));
    let steps = steps_for(&record, &web())
        .expect("steps")
        .expect("macos-pkg's");
    assert!(matches!(steps, Steps::MacosPkg));
    assert_eq!(steps.installer(), Installer::MacosPkg);
    for name in ["oci-image", "later-format"] {
        record
            .installed
            .insert("web".to_owned(), InstalledEntry::new(name));
        let refused = format!(
            "web is installed by the installer {name:?}, which this daemon has no steps of"
        );
        assert_eq!(installer_of(&record, &web()), Err(refused.clone()));
        assert_eq!(steps_for(&record, &web()).err(), Some(refused));
    }
}
