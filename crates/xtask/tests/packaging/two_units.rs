//! The daemon has two unit files. The bootstrap unit, `hostkeeper.service`, is what the deb and
//! the rpm install at `/usr/lib/systemd/system/hostkeeper.service` and the overlay holds a copy
//! of (`overlay.rs` holds the copy to it byte for byte); it starts the installed binary. The
//! package unit, `units/hostkeeper.service`, is what hostkeeper's own package carries, which the
//! release workflow builds with `release package --unit`; its install links it in
//! `/etc/systemd/system/` in place of the bootstrap unit, and it starts `current`.
//! Neither may stand in for the other: a package built from the bootstrap unit would start the
//! installed binary after the first self-update rather than the one the update chose.

use std::fs;
use std::path::Path;

use crate::daemon_flags::services;
use crate::nfpm::File;
use crate::{INSTALLED_UNIT, PACKAGE_UNIT, UNIT, altered, config, linux, refuses};

/// The workflow that builds hostkeeper's own package.
const ARTIFACTS: &str = "../../.github/workflows/artifacts.yml";

/// The package unit, as the workflow names it from the repository root.
const PACKAGE_UNIT_PATH: &str = "packaging/linux/units/hostkeeper.service";

/// The bootstrap unit, from the repository root: the source `nfpm.yaml` installs.
const BOOTSTRAP_UNIT_PATH: &str = "packaging/linux/hostkeeper.service";

/// The copy of the bootstrap unit in the overlay, from `packaging/linux`.
const OVERLAY_UNIT: &str = "overlay/usr/lib/systemd/system/hostkeeper.service";

/// The `--unit` of each `release package` of hostkeeper in `workflow` that is given one (a line
/// that runs the verb without `--service`, which names another service's package; the macOS
/// package takes no unit, and a Linux one without is refused by the verb itself).
fn package_units(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .filter(|l| l.contains("cargo xtask release package ") & !l.contains("--service"))
        .filter_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let unit = words.windows(2).find(|w| w[0] == "--unit");
            unit.map(|w| w[1].to_owned())
        })
        .collect()
}

fn check_two_units(workflow: &str, found: &[String]) -> Result<(), String> {
    let units = package_units(workflow);
    if units != [PACKAGE_UNIT_PATH] {
        return Err(format!(
            "hostkeeper's package is built with --unit {units:?}, not [{PACKAGE_UNIT_PATH:?}]"
        ));
    }
    let mut found = found.to_vec();
    found.sort();
    if found != [UNIT, OVERLAY_UNIT, PACKAGE_UNIT] {
        return Err(format!(
            "packaging/linux holds the units {found:?}, not {UNIT}, its overlay copy and {PACKAGE_UNIT}"
        ));
    }
    Ok(())
}

/// Catches a unit of the daemon beyond the two, or hostkeeper's package carrying the wrong one:
/// the package built from the bootstrap file the deb installs, built twice or without a unit,
/// or another `*.service` beside the two units and the overlay's copy.
#[test]
fn the_daemon_has_a_bootstrap_unit_and_a_package_unit_and_its_package_carries_the_latter() {
    let workflow = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACTS))
        .expect("read the artifacts workflow");
    let mut found = Vec::new();
    services(&linux(), &linux(), &mut found);
    assert_eq!(check_two_units(&workflow, &found), Ok(()));

    let in_workflow = |text: &str| check_two_units(text, &found);
    let flag = format!("--unit {PACKAGE_UNIT_PATH} ");
    let cases = [
        (
            altered(&workflow, &flag, &format!("--unit {BOOTSTRAP_UNIT_PATH} ")),
            "built with --unit [\"packaging/linux/hostkeeper.service\"]",
        ),
        (altered(&workflow, &flag, ""), "built with --unit []"),
        (
            format!("{workflow}      - run: cargo xtask release package {flag}--out other\n"),
            "not [\"packaging/linux/units/hostkeeper.service\"]",
        ),
    ];
    refuses(in_workflow, &cases);

    let mut third = found.clone();
    third.push("package/hostkeeper.service".to_owned());
    let mut missing = found.clone();
    missing.retain(|f| f != PACKAGE_UNIT);
    for changed in [third, missing] {
        let refused = check_two_units(&workflow, &changed).expect_err("not the two units");
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
