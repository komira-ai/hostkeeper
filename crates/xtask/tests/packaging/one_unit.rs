//! The daemon has one unit file, `hostkeeper.service`: the deb and the rpm install it, the
//! overlay holds a copy (`overlay.rs` holds the copy to it byte for byte), and hostkeeper's own
//! package carries it, which the release workflow builds with `release package --unit`. The
//! update of hostkeeper links the package's unit in place of the installed one, so a second
//! source could start another binary after the first self-update than before it.

use std::fs;
use std::path::Path;

use crate::daemon_flags::services;
use crate::{UNIT, altered, linux, refuses};

/// The workflow that builds hostkeeper's own package.
const ARTIFACTS: &str = "../../.github/workflows/artifacts.yml";

/// The unit file, as the workflow names it from the repository root.
const UNIT_PATH: &str = "packaging/linux/hostkeeper.service";

/// The copy of the unit in the overlay, from `packaging/linux`.
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

fn check_one_unit(workflow: &str, found: &[String]) -> Result<(), String> {
    let units = package_units(workflow);
    if units != [UNIT_PATH] {
        return Err(format!(
            "hostkeeper's package is built with --unit {units:?}, not [{UNIT_PATH:?}]"
        ));
    }
    let mut found = found.to_vec();
    found.sort();
    if found != [UNIT, OVERLAY_UNIT] {
        return Err(format!(
            "packaging/linux holds the units {found:?}, not {UNIT} and its overlay copy"
        ));
    }
    Ok(())
}

/// Catches a second unit file of the daemon: hostkeeper's package built from another file than
/// the one the deb installs, built twice or without a unit, or another `*.service` beside the
/// unit and its overlay copy (the package's own unit of before, which started the package's
/// binary rather than the one the self-update switches).
#[test]
fn the_daemon_has_one_unit_file_and_its_package_carries_it() {
    let workflow = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACTS))
        .expect("read the artifacts workflow");
    let mut found = Vec::new();
    services(&linux(), &linux(), &mut found);
    assert_eq!(check_one_unit(&workflow, &found), Ok(()));

    let in_workflow = |text: &str| check_one_unit(text, &found);
    let flag = format!("--unit {UNIT_PATH} ");
    let cases = [
        (
            altered(
                &workflow,
                &flag,
                "--unit packaging/linux/package/hostkeeper.service ",
            ),
            "built with --unit [\"packaging/linux/package/hostkeeper.service\"]",
        ),
        (altered(&workflow, &flag, ""), "built with --unit []"),
        (
            format!("{workflow}      - run: cargo xtask release package {flag}--out other\n"),
            "not [\"packaging/linux/hostkeeper.service\"]",
        ),
    ];
    refuses(in_workflow, &cases);

    let mut second = found.clone();
    second.push("package/hostkeeper.service".to_owned());
    let refused = check_one_unit(&workflow, &second).expect_err("a second unit");
    assert!(
        refused.starts_with("packaging/linux holds the units"),
        "{refused}"
    );
}
