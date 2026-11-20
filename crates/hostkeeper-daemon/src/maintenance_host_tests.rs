//! The tests of `maintenance_host`: the collector over a scratch directory standing for the host's
//! files and a scripted `uname`, and the two parsers over fixtures; each fact and reason is
//! asserted by equality.

use std::collections::BTreeSet;
use std::fs;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_collect::LinuxScope;

const UBUNTU: &str = include_str!("../tests/fixtures/maintenance/host/os-release-ubuntu.txt");
const ALPINE: &str = include_str!("../tests/fixtures/maintenance/host/os-release-alpine.txt");
const BARE: &str = include_str!("../tests/fixtures/maintenance/host/os-release-bare.txt");
const STAT: &str = include_str!("../tests/fixtures/maintenance/host/proc-stat.txt");
const BOOT_ID_VALUE: &str = "5f1d6a0e-33c4-4b9e-8d2a-0c7b9e1f4a21";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn put(scratch: &Scratch, path: &str, text: &str) {
    let file = scratch.path().join(path);
    fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    fs::write(file, text).expect("the file");
}

/// A host with Ubuntu's `os-release`, the boot id, the kernel counters and, under
/// `/lib/modules`, one kernel for each of `kernels`.
fn host(name: &str, kernels: &[&str]) -> Scratch {
    let scratch = scratch(name);
    put(&scratch, "etc/os-release", UBUNTU);
    put(
        &scratch,
        "proc/sys/kernel/random/boot_id",
        &format!("{BOOT_ID_VALUE}\n"),
    );
    put(&scratch, "proc/stat", STAT);
    fs::create_dir_all(scratch.path().join("lib/modules")).expect("the modules directory");
    for kernel in kernels {
        fs::create_dir_all(scratch.path().join(format!("lib/modules/{kernel}/kernel")))
            .expect("a kernel");
    }
    scratch
}

fn uname(answer: Result<LaunchctlOutput, LaunchctlError>) -> FakeLaunchctl {
    let fake = FakeLaunchctl::new();
    fake.always(&["-r".to_owned()], answer);
    fake
}

fn running(release: &str) -> FakeLaunchctl {
    uname(Ok(LaunchctlOutput::success(&format!("{release}\n"))))
}

fn collect(scratch: &Scratch, fake: &FakeLaunchctl) -> Result<Section, String> {
    let own = BTreeSet::new();
    let look = Look {
        scope: LinuxScope::Security,
        own_packages: &own,
    };
    HostFacts::new(Box::new(fake.clone()), HostRoot::at(scratch.path())).collect(&look)
}

/// Proves the collector's name, which a failed look's reason starts with, and that the one over
/// the host's own programs and files is built.
#[test]
fn the_collector_is_named_host_and_is_built_over_the_host() {
    assert_eq!(HostFacts::at_host().name(), "host");
}

/// Proves the facts of an up-to-date host: the `ID` and `VERSION_ID` of `os-release`, the running
/// kernel, the newest installed (the greatest, compared as numbers) and the boot time, with
/// nothing due while the running kernel is the newest.
#[test]
fn an_up_to_date_host_reports_its_facts_and_nothing_due() {
    let host = host(
        "host-up-to-date",
        &["6.8.0-9-generic", "6.8.0-45-generic", "6.8.0-4-generic"],
    );
    let section = collect(&host, &running("6.8.0-45-generic")).expect("a section");
    assert_eq!(
        section,
        Section {
            host: pb::MaintenanceHost {
                os_name: "ubuntu".to_owned(),
                os_version: "24.04".to_owned(),
                running_kernel: "6.8.0-45-generic".to_owned(),
                newest_kernel: "6.8.0-45-generic".to_owned(),
                boot_id: BOOT_ID_VALUE.to_owned(),
                boot_unix: 1_760_000_000,
                ..pb::MaintenanceHost::default()
            },
            ..Section::default()
        }
    );
}

/// Proves the report carries the boot id the kernel gives, without its line end.
#[test]
fn the_report_carries_the_boot_id() {
    let host = host("host-boot-id", &["6.8.0-45-generic"]);
    let section = collect(&host, &running("6.8.0-45-generic")).expect("a section");
    assert_eq!(section.host.boot_id, "5f1d6a0e-33c4-4b9e-8d2a-0c7b9e1f4a21");
}

/// Proves a reboot is due for a newer installed kernel with nothing to install: a kernel under
/// `/lib/modules` greater than the running one is the newest, and the reboot is due for the
/// reason `kernel`, from the running kernel.
#[test]
fn reboot_is_due_for_a_newer_installed_kernel_with_nothing_to_install() {
    let host = host(
        "host-newer-kernel",
        &["6.8.0-45-generic", "6.8.0-100-generic"],
    );
    let section = collect(&host, &running("6.8.0-45-generic")).expect("a section");
    assert_eq!(section.host.newest_kernel, "6.8.0-100-generic");
    assert_eq!(section.due, vec![reboot_item("kernel", "6.8.0-45-generic")]);
    assert_eq!(section.due[0].kind, i32::from(pb::MaintenanceKind::Reboot));
    assert!(section.due[0].needs_reboot);
}

/// Proves a directory of `/lib/modules` without a `kernel` directory (the leftover of a removed
/// kernel) is not an installed kernel, and a file there is not one either.
#[test]
fn a_leftover_modules_directory_is_not_an_installed_kernel() {
    let host = host("host-leftover", &["6.8.0-45-generic"]);
    fs::create_dir_all(host.path().join("lib/modules/6.8.0-100-generic")).expect("a leftover");
    put(&host, "lib/modules/6.9.0-1-generic", "a file");
    let section = collect(&host, &running("6.8.0-45-generic")).expect("a section");
    assert_eq!(section.host.newest_kernel, "6.8.0-45-generic");
    assert_eq!(section.due, Vec::new());
}

/// Proves that with no installed kernel at all (a container) the newest is the running kernel and
/// nothing is due.
#[test]
fn with_no_installed_kernel_the_newest_is_the_running_one() {
    let host = host("host-no-kernel", &[]);
    let section = collect(&host, &running("6.8.0-45-generic")).expect("a section");
    assert_eq!(section.host.newest_kernel, "6.8.0-45-generic");
    assert_eq!(section.due, Vec::new());
}

/// Proves a missing `/proc/stat` is a boot time of 0, not a failure, and that a host text is cut
/// to its bound.
#[test]
fn a_missing_stat_file_is_a_boot_time_of_zero_and_long_texts_are_cut() {
    let host = host("host-no-stat", &[]);
    fs::remove_file(host.path().join("proc/stat")).expect("remove it");
    put(
        &host,
        "etc/os-release",
        &format!("ID={}\nVERSION_ID=1\n", "a".repeat(40)),
    );
    let long = "6.8.0-".to_owned() + &"9".repeat(80);
    let section = collect(&host, &running(&long)).expect("a section");
    assert_eq!(section.host.boot_unix, 0);
    assert_eq!(section.host.os_name, "a".repeat(32));
    assert_eq!(section.host.running_kernel, &long[..64]);
    assert_eq!(section.host.newest_kernel, &long[..64]);
}

/// Proves a hung `uname` is killed at its deadline and the collector fails, naming the deadline.
#[test]
fn a_hung_uname_fails_the_collector_naming_its_deadline() {
    let host = host("host-hung-uname", &[]);
    let fake = uname(Err(LaunchctlError::TimedOut {
        args: "-r".to_owned(),
    }));
    assert_eq!(
        collect(&host, &fake),
        Err("uname -r did not finish within its deadline".to_owned())
    );
}

/// Proves a `uname` that fails gives its exit code and the first line of its error, and one that
/// prints nothing is a failure too.
#[test]
fn a_failing_or_silent_uname_fails_the_collector() {
    let host = host("host-bad-uname", &[]);
    assert_eq!(
        collect(
            &host,
            &uname(Ok(LaunchctlOutput::exit(1, "uname: broken\nmore\n")))
        ),
        Err("uname -r failed with exit code 1: uname: broken".to_owned())
    );
    assert_eq!(
        collect(&host, &uname(Ok(LaunchctlOutput::success("\n")))),
        Err("uname -r printed nothing".to_owned())
    );
}

/// Proves each file the collector must read fails it, naming the file, when it cannot be read: a
/// missing `os-release`, a missing `/lib/modules`, a missing boot id, a `/proc/stat` that is
/// there and cannot be read.
#[test]
fn a_file_that_cannot_be_read_fails_the_collector_naming_it() {
    let reason =
        |scratch: &Scratch| collect(scratch, &running("6.8.0-45-generic")).expect_err("a failure");

    let host_a = host("host-no-os-release", &[]);
    fs::remove_file(host_a.path().join("etc/os-release")).expect("remove it");
    assert!(reason(&host_a).starts_with("/etc/os-release could not be read: "));

    let host_b = host("host-no-modules", &[]);
    fs::remove_dir(host_b.path().join("lib/modules")).expect("remove it");
    assert!(reason(&host_b).starts_with("/lib/modules could not be read: "));

    let host_c = host("host-no-boot-id", &[]);
    fs::remove_file(host_c.path().join("proc/sys/kernel/random/boot_id")).expect("remove it");
    assert!(reason(&host_c).starts_with("/proc/sys/kernel/random/boot_id could not be read: "));

    let host_d = host("host-stat-dir", &[]);
    fs::remove_file(host_d.path().join("proc/stat")).expect("remove it");
    fs::create_dir(host_d.path().join("proc/stat")).expect("a directory in its place");
    assert!(reason(&host_d).starts_with("/proc/stat could not be read: "));
}

/// Proves `ID` and `VERSION_ID` are read from the quoted values of Ubuntu's file, the single
/// quotes of Alpine's, and the bare values of a file with a comment and a blank line; `VERSION`,
/// `VERSION_CODENAME` and `ID_LIKE` are not taken for them.
#[test]
fn os_release_values_are_read_quoted_or_bare() {
    assert_eq!(
        parse_os_release(UBUNTU),
        ("ubuntu".to_owned(), "24.04".to_owned())
    );
    assert_eq!(
        parse_os_release(ALPINE),
        ("alpine".to_owned(), "3.20.3".to_owned())
    );
    assert_eq!(
        parse_os_release(BARE),
        ("debian".to_owned(), "12".to_owned())
    );
}

/// Proves a file with neither key gives empty texts.
#[test]
fn os_release_without_the_keys_gives_empty_texts() {
    assert_eq!(
        parse_os_release("NAME=\"Thing\"\nID_LIKE=debian\n"),
        (String::new(), String::new())
    );
}

/// Proves the boot time is the `btime` line's number, and 0 when the line is absent or garbled.
#[test]
fn the_boot_time_is_the_btime_line_or_zero() {
    assert_eq!(parse_boot_time(STAT), 1_760_000_000);
    assert_eq!(parse_boot_time("cpu 1 2 3\n"), 0);
    assert_eq!(parse_boot_time("btime soon\n"), 0);
}
