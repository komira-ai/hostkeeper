//! The tests of `maintenance_collectors`: the collectors each platform lists, and the package
//! manager chosen by the programs a scratch host root holds.

use hostkeeper_testkit::Scratch;

use super::*;
use crate::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A host root holding each of `programs` as a file.
fn root_with(scratch: &Scratch, programs: &[&str]) -> HostRoot {
    for program in programs {
        let path = scratch.path().join(program.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(path, "").expect("the program");
    }
    HostRoot::at(scratch.path())
}

fn names(collectors: &[Box<dyn Collector>]) -> Vec<&'static str> {
    collectors
        .iter()
        .map(|collector| collector.name())
        .collect()
}

/// Proves each package manager is chosen by its program being a file, `apt-get` before `dnf`
/// before `apk`, and a host with none has no package collector.
#[test]
fn the_package_manager_is_the_first_program_the_host_has() {
    let cases = [
        (
            "collectors-all",
            vec![
                maintenance_apt::APT_GET,
                maintenance_dnf::DNF,
                maintenance_apk::APK,
            ],
            Some("apt"),
        ),
        (
            "collectors-dnf",
            vec![maintenance_dnf::DNF, maintenance_apk::APK],
            Some("dnf"),
        ),
        ("collectors-apk", vec![maintenance_apk::APK], Some("apk")),
        ("collectors-none", vec![], None),
    ];
    for (name, programs, expected) in cases {
        let scratch = scratch(name);
        let root = root_with(&scratch, &programs);
        assert_eq!(
            package_manager(&root, DEFAULT_PACKAGE_LOCK_WAIT_SECS)
                .map(|collector| collector.name()),
            expected,
            "{name}"
        );
    }
}

/// Proves a Linux host lists the host's facts, the reboot need, and then its package manager,
/// and one with no package manager lists the first two alone.
#[test]
fn a_linux_host_lists_the_host_the_reboot_and_the_package_manager() {
    let scratch = scratch("collectors-linux");
    let root = root_with(&scratch, &[maintenance_apt::APT_GET]);
    assert_eq!(
        names(&linux_under(&root, DEFAULT_PACKAGE_LOCK_WAIT_SECS)),
        ["host", "reboot", "apt"]
    );
    let bare = HostRoot::at(&scratch.path().join("bare"));
    assert_eq!(
        names(&linux_under(&bare, DEFAULT_PACKAGE_LOCK_WAIT_SECS)),
        ["host", "reboot"]
    );
}

/// Proves the host's own collectors start with the host's facts and the reboot need on Linux,
/// systemd or OpenRC alike, whatever the Mac's setting, and a Mac reads `macos`, its boot facts,
/// `mac`, then its Xcodes, its update account and its device-management conflicts, under either
/// setting.
#[test]
fn the_platforms_list_their_collectors() {
    for major in [MacosMajor::NotDue, MacosMajor::Due] {
        for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
            let collectors = at_host(kind, major, DEFAULT_PACKAGE_LOCK_WAIT_SECS);
            assert_eq!(names(&collectors)[..2], ["host", "reboot"]);
        }
        assert_eq!(
            names(&at_host(
                PlatformKind::Macos,
                major,
                DEFAULT_PACKAGE_LOCK_WAIT_SECS
            )),
            ["macos", "boot", "mac", "xcode", "update-account", "mdm"]
        );
        assert_eq!(
            names(&mac(major)),
            ["macos", "boot", "mac", "xcode", "update-account", "mdm"]
        );
    }
}
