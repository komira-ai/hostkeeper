//! The tests of the collector `macos`: over a scripted `sw_vers`, `ioreg` and `softwareupdate`,
//! what it reports of the Mac and which macOS updates are due, which programs it runs, and why it
//! fails; each section and reason asserted by equality.

use std::collections::BTreeSet;
use std::time::Duration;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};

use super::*;
use crate::maintenance_collect::LinuxScope;

const SW_VERS_26: &str = include_str!("../tests/fixtures/maintenance/mac/sw_vers_macos26.txt");
const IOREG_OUTPUT: &str = include_str!("../tests/fixtures/maintenance/mac/ioreg.txt");
const MACOS26_MAJOR: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_major.txt");
const MACOS26_NONE: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_none.txt");
const MACOS26_FULL: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_full_installers.txt");
const MACOS27_UNKNOWN: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos27_list_unknown_line.txt");

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The scripted programs of a Mac: `sw_vers` writes `sw_vers`, `ioreg` the fixture, and the two
/// listings `list` and `full`.
struct Mac {
    sw_vers: FakeLaunchctl,
    ioreg: FakeLaunchctl,
    list: FakeLaunchctl,
    full: FakeLaunchctl,
}

impl Mac {
    fn new(sw_vers: &str, list: &str, full: &str) -> Mac {
        let mac = Mac {
            sw_vers: FakeLaunchctl::new(),
            ioreg: FakeLaunchctl::new(),
            list: FakeLaunchctl::new(),
            full: FakeLaunchctl::new(),
        };
        mac.sw_vers
            .always(&[], Ok(LaunchctlOutput::success(sw_vers)));
        mac.ioreg
            .always(&ioreg_args(), Ok(LaunchctlOutput::success(IOREG_OUTPUT)));
        mac.list
            .always(&args(&LIST_ARGS), Ok(LaunchctlOutput::success(list)));
        mac.full
            .always(&args(&FULL_ARGS), Ok(LaunchctlOutput::success(full)));
        mac
    }

    fn collector(&self) -> Macos {
        Macos::new(MacosPorts {
            sw_vers: Box::new(self.sw_vers.clone()),
            ioreg: Box::new(self.ioreg.clone()),
            list: Box::new(self.list.clone()),
            full: Box::new(self.full.clone()),
        })
    }

    fn collect(&self) -> Result<Section, String> {
        let own = BTreeSet::new();
        self.collector().collect(&Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        })
    }
}

fn host() -> pb::MaintenanceHost {
    pb::MaintenanceHost {
        os_name: "macOS".to_owned(),
        os_version: "26.6".to_owned(),
        os_build: "25G62".to_owned(),
        hardware_serial: "H7K2XQ9WLM".to_owned(),
        ..pb::MaintenanceHost::default()
    }
}

fn macos(to: &str, label: &str) -> pb::DueItem {
    pb::DueItem {
        kind: pb::MaintenanceKind::Macos.into(),
        from: "26.6".to_owned(),
        to: to.to_owned(),
        needs_reboot: true,
        label: label.to_owned(),
        security: false,
    }
}

/// Proves a Mac of macOS 26 whose `softwareupdate --list` names a minor update and a major
/// upgrade reports `macOS`, its version, build and serial, lists the full installers too, and
/// reports the minor update due and then the newest major upgrade, with no setting of its own:
/// whether the major upgrade counts is the coordinator's, by the group.
///
/// Catches: a collector that leaves the major upgrade out of the report, or never asks for the
/// full installers, so a group that allows a major upgrade never has one due.
#[test]
fn the_newest_major_upgrade_is_always_reported_after_the_minor_update() {
    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    assert_eq!(
        mac.collect(),
        Ok(Section {
            host: host(),
            due: vec![
                macos("26.7", "macOS Tahoe 26.7-25H62"),
                macos("27.1", "macOS 27.1-26B70"),
            ],
            ..Section::default()
        })
    );
    assert_eq!(mac.list.calls(), [args(&LIST_ARGS)]);
    assert_eq!(mac.full.calls(), [args(&FULL_ARGS)]);
    assert_eq!(mac.ioreg.calls(), [ioreg_args()]);
    assert_eq!(mac.sw_vers.calls(), [Vec::<String>::new()]);
}

/// Proves a major upgrade offered only by `--list-full-installers` (the `--list` of a Mac that has
/// its newest minor update) is reported due alone, named by the installer's title.
#[test]
fn a_major_upgrade_only_a_full_installer_offers_is_reported() {
    let mac = Mac::new(SW_VERS_26, MACOS26_NONE, MACOS26_FULL);
    assert_eq!(
        mac.collect(),
        Ok(Section {
            host: host(),
            due: vec![macos("27.1", "macOS 27.1")],
            ..Section::default()
        })
    );
}

/// Proves the lines no parser knew, of both listings, are added up in the section.
#[test]
fn the_unknown_lines_of_both_listings_are_counted() {
    let mac = Mac::new(SW_VERS_26, MACOS27_UNKNOWN, "Something new\n");
    assert_eq!(mac.collect().map(|s| s.unparsed), Ok(5));
}

/// Proves a `softwareupdate --list` that does not finish by its deadline fails the collector,
/// naming the step, and the full installers are not asked for; a failing full listing fails it
/// too, and so does one that does not finish by its deadline.
#[test]
fn a_listing_that_times_out_or_fails_fails_the_collector() {
    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.list.always(
        &args(&LIST_ARGS),
        Err(LaunchctlError::TimedOut {
            args: "--list".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("softwareupdate --list did not finish within its deadline".to_owned())
    );
    assert_eq!(mac.full.calls(), Vec::<Vec<String>>::new());

    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.full.always(
        &args(&FULL_ARGS),
        Ok(LaunchctlOutput::exit(1, "Network unavailable\n")),
    );
    assert_eq!(
        mac.collect(),
        Err(
            "softwareupdate --list-full-installers failed with exit code 1: Network unavailable"
                .to_owned()
        )
    );

    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.full.always(
        &args(&FULL_ARGS),
        Err(LaunchctlError::TimedOut {
            args: "--list-full-installers".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("softwareupdate --list-full-installers did not finish within its deadline".to_owned())
    );
}

/// Proves a `sw_vers` that fails or names no version, and an `ioreg` that fails or names no
/// serial, each fail the collector with its reason, before any listing.
#[test]
fn the_host_facts_that_cannot_be_read_fail_the_collector() {
    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.sw_vers
        .always(&[], Ok(LaunchctlOutput::exit(1, "sw_vers: broken\n")));
    assert_eq!(
        mac.collect(),
        Err("sw_vers failed with exit code 1: sw_vers: broken".to_owned())
    );

    let mac = Mac::new("BuildVersion:\t25G62\n", MACOS26_MAJOR, MACOS26_FULL);
    assert_eq!(
        mac.collect(),
        Err("sw_vers named no ProductVersion".to_owned())
    );

    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.ioreg.always(
        &ioreg_args(),
        Ok(LaunchctlOutput::success("    \"IOPlatformUUID\" = \"x\"\n")),
    );
    assert_eq!(
        mac.collect(),
        Err("ioreg output has no IOPlatformSerialNumber".to_owned())
    );

    let mac = Mac::new(SW_VERS_26, MACOS26_MAJOR, MACOS26_FULL);
    mac.ioreg.always(
        &ioreg_args(),
        Err(LaunchctlError::TimedOut {
            args: "-rd1".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("ioreg did not finish within its deadline".to_owned())
    );
    assert_eq!(mac.list.calls(), Vec::<Vec<String>>::new());
}

/// Proves `sw_vers` is read by its keys, whatever the white space, and a key it lacks is empty.
#[test]
fn sw_vers_is_read_by_its_keys() {
    assert_eq!(
        parse_sw_vers(SW_VERS_26),
        ("26.6".to_owned(), "25G62".to_owned())
    );
    assert_eq!(
        parse_sw_vers("ProductVersion: 27.1\n"),
        ("27.1".to_owned(), String::new())
    );
    assert_eq!(parse_sw_vers(""), (String::new(), String::new()));
}

/// Proves the collector is named `macos`, which a failed look's reason starts with, and the
/// host's is made over the programs and deadlines the module names.
#[test]
fn the_collector_is_named_macos_and_runs_the_macs_programs() {
    assert_eq!(Macos::at_host().name(), "macos");
    assert_eq!(SW_VERS, "/usr/bin/sw_vers");
    assert_eq!(SOFTWAREUPDATE, "/usr/sbin/softwareupdate");
    assert_eq!(IOREG, "/usr/sbin/ioreg");
    assert_eq!(LIST_DEADLINE, Duration::from_secs(15 * 60));
    assert_eq!(LOOK_DEADLINE, Duration::from_secs(60));
    assert_eq!(TOOL_DEADLINE, Duration::from_secs(30));
}
