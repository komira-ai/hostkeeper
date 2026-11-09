//! A host with no switch refuses one; a recorded `apt` service has the repository installer's
//! steps; an installer the record names that this build has no steps of, or that is no
//! installer, is refused with what is recorded.

use std::path::Path;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::Host;
use hostkeeper_placed::testing::{FakeUnitControl, Log};
use hostkeeper_placed::{InstalledEntry, PlacedRecord, Placer, RealFs, Roots};
use hostkeeper_release::Installer;

use super::{PlacerHost, Steps, installer_of, steps_for};

fn web() -> ServiceName {
    ServiceName::new("web").expect("a name")
}

#[test]
fn a_host_with_no_switch_refuses_one() {
    let fs = RealFs::new(Path::new("/nonexistent-root"));
    let units = FakeUnitControl::new(Log::new());
    let roots = Roots::new(Path::new("/s"), Path::new("/u"), Path::new("/p"));
    let placer = Placer::new(roots, &fs, &units);
    let service = web();
    let host = PlacerHost {
        placer: &placer,
        service: &service,
        switch: None,
    };
    let refused = host
        .switch(&Version::new("1.0.0").expect("a version"))
        .expect_err("no switch");
    assert_eq!(refused.0, "nothing switches 1.0.0 of web here");
}

#[test]
fn a_recorded_installer_is_read_apt_has_its_backend_and_one_without_steps_is_refused() {
    let mut record = PlacedRecord::default();
    assert_eq!(
        installer_of(&record, &web()),
        Ok(Installer::HostkeeperArchive)
    );
    assert_eq!(
        steps_for(&record, &web()).map(|steps| steps.installer()),
        Ok(Installer::HostkeeperArchive)
    );
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("apt"));
    assert_eq!(installer_of(&record, &web()), Ok(Installer::Apt));
    assert_eq!(
        steps_for(&record, &web()).map(|steps| steps.installer()),
        Ok(Installer::Apt)
    );
    assert!(matches!(
        steps_for(&record, &web()),
        Ok(Steps::Repository(_))
    ));
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("macos-pkg"));
    assert!(matches!(steps_for(&record, &web()), Ok(Steps::MacosPkg)));
    assert_eq!(
        steps_for(&record, &web()).map(|steps| steps.installer()),
        Ok(Installer::MacosPkg)
    );
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("oci-image"));
    assert_eq!(
        steps_for(&record, &web()).map(|steps| steps.installer()),
        Err(
            "web is installed by the installer \"oci-image\", which this daemon has no steps of"
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
}
