//! The tests of `maintenance_reboot`: the collector over a scratch directory standing for the
//! host's files and scripted programs that record their calls, and the parser over fixtures; the
//! due items, the reasons and the calls are asserted by equality.

use std::collections::BTreeSet;
use std::fs;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_collect::LinuxScope;

const KSTA1: &str = include_str!("../tests/fixtures/maintenance/needrestart/ksta1.txt");
const KSTA3: &str = include_str!("../tests/fixtures/maintenance/needrestart/ksta3-services.txt");
const UNKNOWN: &str = include_str!("../tests/fixtures/maintenance/needrestart/unknown-line.txt");

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn put(scratch: &Scratch, path: &str) {
    let file = scratch.path().join(path);
    fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    fs::write(file, "").expect("the file");
}

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A host whose files are those of `paths`, and the two scripted programs.
struct Machine {
    root: Scratch,
    needrestart: FakeLaunchctl,
    needs_restarting: FakeLaunchctl,
}

impl Machine {
    fn new(name: &str, paths: &[&str]) -> Machine {
        let root = scratch(name);
        for path in paths {
            put(&root, path);
        }
        Machine {
            root,
            needrestart: FakeLaunchctl::new(),
            needs_restarting: FakeLaunchctl::new(),
        }
    }

    fn collect(&self) -> Result<Section, String> {
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        RebootNeed::new(
            Box::new(self.needrestart.clone()),
            Box::new(self.needs_restarting.clone()),
            HostRoot::at(self.root.path()),
        )
        .collect(&look)
    }
}

const NEEDRESTART_FILE: &str = "usr/sbin/needrestart";
const NEEDS_RESTARTING_FILE: &str = "usr/bin/needs-restarting";

/// Proves the collector's name, which a failed look's reason starts with, and that the one over
/// the host's own programs and files is built.
#[test]
fn the_collector_is_named_reboot_and_is_built_over_the_host() {
    assert_eq!(RebootNeed::at_host().name(), "reboot");
}

/// Proves a host with none of the three sources has nothing due and no program is run.
#[test]
fn a_host_with_no_source_has_nothing_due() {
    let machine = Machine::new("reboot-nothing", &[]);
    assert_eq!(machine.collect(), Ok(Section::default()));
    assert_eq!(machine.needrestart.calls(), Vec::<Vec<String>>::new());
    assert_eq!(machine.needs_restarting.calls(), Vec::<Vec<String>>::new());
}

/// Proves the file `/run/reboot-required` makes a reboot due for the reason `reboot-required`.
#[test]
fn the_reboot_required_file_makes_a_reboot_due() {
    let machine = Machine::new("reboot-file", &["run/reboot-required"]);
    assert_eq!(
        machine.collect(),
        Ok(Section {
            due: vec![reboot_item("reboot-required", "")],
            ..Section::default()
        })
    );
}

/// Proves `needrestart -b` is run with `-b` alone when installed, and a kernel state of 1 (the
/// running kernel is the expected one) is not a reboot.
#[test]
fn needrestart_with_the_kernel_current_makes_nothing_due() {
    let machine = Machine::new("reboot-ksta1", &[NEEDRESTART_FILE]);
    machine
        .needrestart
        .always(&args(&["-b"]), Ok(LaunchctlOutput::success(KSTA1)));
    assert_eq!(machine.collect(), Ok(Section::default()));
    assert_eq!(machine.needrestart.calls(), vec![args(&["-b"])]);
}

/// Proves a kernel state of 3 (a newer kernel installed) makes a reboot due for the reason
/// `needrestart`, and the services that need a restart, the containers, the sessions and the
/// microcode lines are known and make nothing else due.
#[test]
fn needrestart_with_a_newer_kernel_makes_a_reboot_due() {
    let machine = Machine::new("reboot-ksta3", &[NEEDRESTART_FILE]);
    machine
        .needrestart
        .always(&args(&["-b"]), Ok(LaunchctlOutput::success(KSTA3)));
    assert_eq!(
        machine.collect(),
        Ok(Section {
            due: vec![reboot_item("needrestart", "")],
            ..Section::default()
        })
    );
}

/// Proves the lines of `needrestart -b` that are not known are counted in the section, and the
/// known ones of the same output still act.
#[test]
fn needrestart_lines_that_are_not_known_are_counted() {
    let machine = Machine::new("reboot-unknown", &[NEEDRESTART_FILE]);
    machine
        .needrestart
        .always(&args(&["-b"]), Ok(LaunchctlOutput::success(UNKNOWN)));
    assert_eq!(
        machine.collect(),
        Ok(Section {
            due: vec![reboot_item("needrestart", "")],
            unparsed: 3,
            ..Section::default()
        })
    );
}

/// Proves a `needrestart` that fails or hangs fails the collector with the step's reason.
#[test]
fn a_failing_needrestart_fails_the_collector() {
    let machine = Machine::new("reboot-needrestart-fails", &[NEEDRESTART_FILE]);
    machine.needrestart.then(
        &args(&["-b"]),
        Ok(LaunchctlOutput::exit(2, "needrestart: no\nmore\n")),
    );
    assert_eq!(
        machine.collect(),
        Err("needrestart -b failed with exit code 2: needrestart: no".to_owned())
    );
    machine.needrestart.then(
        &args(&["-b"]),
        Err(LaunchctlError::TimedOut {
            args: "-b".to_owned(),
        }),
    );
    assert_eq!(
        machine.collect(),
        Err("needrestart -b did not finish within its deadline".to_owned())
    );
}

/// Proves `needs-restarting -r` exiting 1 makes a reboot due for the reason `needs-restarting`,
/// and exiting 0 makes nothing due.
#[test]
fn needs_restarting_exit_one_makes_a_reboot_due() {
    let machine = Machine::new("reboot-needs-restarting", &[NEEDS_RESTARTING_FILE]);
    machine.needs_restarting.then(
        &args(&["-r"]),
        Ok(LaunchctlOutput::exit(
            1,
            "Core libraries or services have been updated:\n",
        )),
    );
    machine.needs_restarting.then(
        &args(&["-r"]),
        Ok(LaunchctlOutput::success("No core libraries\n")),
    );
    assert_eq!(
        machine.collect(),
        Ok(Section {
            due: vec![reboot_item("needs-restarting", "")],
            ..Section::default()
        })
    );
    assert_eq!(machine.collect(), Ok(Section::default()));
    assert_eq!(
        machine.needs_restarting.calls(),
        vec![args(&["-r"]), args(&["-r"])]
    );
}

/// Proves any other exit of `needs-restarting` fails the collector, with the code and the first
/// line of its error.
#[test]
fn needs_restarting_with_another_exit_fails_the_collector() {
    let machine = Machine::new("reboot-needs-restarting-fails", &[NEEDS_RESTARTING_FILE]);
    machine.needs_restarting.always(
        &args(&["-r"]),
        Ok(LaunchctlOutput::exit(3, "Error: no rpm database\n")),
    );
    assert_eq!(
        machine.collect(),
        Err("needs-restarting -r failed with exit code 3: Error: no rpm database".to_owned())
    );
}

/// Proves the three sources add up in the order file, `needrestart`, `needs-restarting`.
#[test]
fn every_source_that_says_so_adds_its_reboot_in_order() {
    let machine = Machine::new(
        "reboot-all",
        &[
            "run/reboot-required",
            NEEDRESTART_FILE,
            NEEDS_RESTARTING_FILE,
        ],
    );
    machine
        .needrestart
        .always(&args(&["-b"]), Ok(LaunchctlOutput::success(KSTA3)));
    machine
        .needs_restarting
        .always(&args(&["-r"]), Ok(LaunchctlOutput::exit(1, "")));
    assert_eq!(
        machine.collect(),
        Ok(Section {
            due: vec![
                reboot_item("reboot-required", ""),
                reboot_item("needrestart", ""),
                reboot_item("needs-restarting", ""),
            ],
            ..Section::default()
        })
    );
}

/// Proves a kernel state of 2 (the kernel's ABI changed) is a reboot, 0 is not, and a state that
/// is not a number is one unparsed line.
#[test]
fn the_kernel_state_decides_the_reboot() {
    assert!(parse_needrestart("NEEDRESTART-KSTA: 2\n").reboot);
    assert!(!parse_needrestart("NEEDRESTART-KSTA: 0\n").reboot);
    assert_eq!(
        parse_needrestart("NEEDRESTART-KSTA: x\n"),
        NeedrestartListing {
            reboot: false,
            unparsed: 1
        }
    );
    assert_eq!(
        parse_needrestart(KSTA3),
        NeedrestartListing {
            reboot: true,
            unparsed: 0
        }
    );
}
