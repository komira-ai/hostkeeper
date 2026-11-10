//! The daemon has two unit files. The bootstrap unit, `hostkeeper.service`, is what the deb and
//! the rpm install at `/usr/lib/systemd/system/hostkeeper.service` and the overlay holds a copy
//! of (`overlay.rs` holds the copy to it byte for byte); it starts the installed binary. The
//! package unit, `units/hostkeeper.service`, is what hostkeeper's own self-update installs; its
//! install links it in `/etc/systemd/system/` in place of the bootstrap unit, and it starts
//! `current`. Neither may stand in for the other: the bootstrap unit would start the installed
//! binary after the first self-update rather than the one the update chose.

use crate::daemon_flags::services;
use crate::nfpm::File;
use crate::{INSTALLED_UNIT, PACKAGE_UNIT, UNIT, config, linux};

/// The package unit, as the workflow names it from the repository root.
const PACKAGE_UNIT_PATH: &str = "packaging/linux/units/hostkeeper.service";

/// The bootstrap unit, from the repository root: the source `nfpm.yaml` installs.
const BOOTSTRAP_UNIT_PATH: &str = "packaging/linux/hostkeeper.service";

/// The copy of the bootstrap unit in the overlay, from `packaging/linux`.
const OVERLAY_UNIT: &str = "overlay/usr/lib/systemd/system/hostkeeper.service";

fn check_two_units(found: &[String]) -> Result<(), String> {
    let mut found = found.to_vec();
    found.sort();
    if found != [UNIT, OVERLAY_UNIT, PACKAGE_UNIT] {
        return Err(format!(
            "packaging/linux holds the units {found:?}, not {UNIT}, its overlay copy and {PACKAGE_UNIT}"
        ));
    }
    Ok(())
}

/// Catches a unit of the daemon beyond the two, or one of the two missing: another `*.service`
/// beside the two units and the overlay's copy.
#[test]
fn the_daemon_has_a_bootstrap_unit_and_a_package_unit() {
    let mut found = Vec::new();
    services(&linux(), &linux(), &mut found);
    assert_eq!(check_two_units(&found), Ok(()));

    let mut third = found.clone();
    third.push("package/hostkeeper.service".to_owned());
    let mut missing = found.clone();
    missing.retain(|f| f != PACKAGE_UNIT);
    for changed in [third, missing] {
        let refused = check_two_units(&changed).expect_err("not the two units");
        assert!(
            refused.starts_with("packaging/linux holds the units"),
            "{refused}"
        );
    }
}

fn check_installed(files: &[File]) -> Result<(), String> {
    let installed: Vec<&File> = files.iter().filter(|f| f.dst == INSTALLED_UNIT).collect();
    match installed.as_slice() {
        [file] if file.src == BOOTSTRAP_UNIT_PATH => Ok(()),
        [file] => Err(format!(
            "{INSTALLED_UNIT} is installed from {}, not the bootstrap unit {BOOTSTRAP_UNIT_PATH}",
            file.src
        )),
        other => Err(format!(
            "{INSTALLED_UNIT} is installed {} times, not once",
            other.len()
        )),
    }
}

/// Catches the deb or the rpm installing the package unit (which starts `current` of a binary
/// directory nothing has yet seeded) or no unit at the installed path.
#[test]
fn the_deb_and_the_rpm_install_the_bootstrap_unit() {
    for format in ["deb", "rpm"] {
        let files = config().files(format).expect("nfpm files");
        assert_eq!(check_installed(&files), Ok(()), "{format}");
        let index = files
            .iter()
            .position(|f| f.dst == INSTALLED_UNIT)
            .expect("the unit is installed");
        let mut package = files.clone();
        package[index].src = PACKAGE_UNIT_PATH.to_owned();
        assert_eq!(
            check_installed(&package),
            Err(format!(
                "{INSTALLED_UNIT} is installed from {PACKAGE_UNIT_PATH}, not the bootstrap unit {BOOTSTRAP_UNIT_PATH}"
            ))
        );
        let mut none = files.clone();
        none.remove(index);
        assert_eq!(
            check_installed(&none),
            Err(format!("{INSTALLED_UNIT} is installed 0 times, not once"))
        );
        let mut twice = files.clone();
        twice.push(files[index].clone());
        assert_eq!(
            check_installed(&twice),
            Err(format!("{INSTALLED_UNIT} is installed 2 times, not once"))
        );
    }
}
