//! The tests of `maintenance_apk`: the parser over a fixture of an Alpine 3.20 host and over the
//! lines it does not know, and the collector over a scripted `apk`, asserted by equality.

use std::collections::BTreeSet;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};

use super::*;
use crate::maintenance_collect::LinuxScope;

const FIXTURE: &str = include_str!("../tests/fixtures/maintenance/apk/version.txt");
const EMPTY_FIXTURE: &str = include_str!("../tests/fixtures/maintenance/apk/version_none.txt");

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn refresh_args() -> Vec<String> {
    args(&["update", "-q"])
}

fn version_args() -> Vec<String> {
    args(&["version", "-l", "<"])
}

fn update(name: &str, from: &str, to: &str) -> ApkUpdate {
    ApkUpdate {
        name: name.to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// An `apk` whose index refresh and version listing both succeed, the listing with `text`.
fn apk(text: &str) -> (Apk, FakeLaunchctl) {
    let fake = FakeLaunchctl::new();
    fake.always(&refresh_args(), Ok(LaunchctlOutput::success("")));
    fake.always(&version_args(), Ok(LaunchctlOutput::success(text)));
    (
        Apk::new(Box::new(fake.clone()), Box::new(fake.clone())),
        fake,
    )
}

fn collect(apk: &Apk, scope: LinuxScope) -> Result<Section, String> {
    let own: BTreeSet<String> = ["hostkeeper".to_owned()].into();
    apk.collect(&Look {
        scope,
        own_packages: &own,
    })
}

fn labels(section: &Section) -> Vec<&str> {
    section.due.iter().map(|item| item.label.as_str()).collect()
}

/// Proves the fixture gives each package's name, its installed version and release, and the
/// version it would be updated to; a package name that has dashes and an underscore keeps them,
/// and the header gives no row.
#[test]
fn fixture_gives_the_updates() {
    assert_eq!(
        parse_apk_version(FIXTURE),
        ApkListing {
            updates: vec![
                update("busybox", "1.36.1-r29", "1.36.1-r31"),
                update("musl", "1.2.5-r0", "1.2.5-r1"),
                update("ssl_client", "1.36.1-r29", "1.36.1-r31"),
                update("linux-lts", "6.6.54-r0", "6.6.58-r0"),
                update("openssl", "3.3.2-r0", "3.3.2-r1"),
                update("hostkeeper", "1.8.0-r0", "1.9.0-r0"),
                update("libcrypto3", "3.3.2-r0", "3.3.2-r1"),
            ],
            unparsed: 0,
        }
    );
}

/// Proves a listing with only the header, or nothing, is no updates and no unknown lines.
#[test]
fn a_header_alone_is_nothing() {
    assert_eq!(parse_apk_version(EMPTY_FIXTURE), ApkListing::default());
    assert_eq!(parse_apk_version("\n\n"), ApkListing::default());
}

/// Proves a line that is not a row is counted: a row whose comparison is not `<`, one with a
/// package string that has no release or no name, and one of the wrong width.
#[test]
fn lines_that_are_not_rows_are_counted() {
    let text = "\
Installed:                                  Available:
musl-1.2.5-r0 = 1.2.5-r0
nodashes < 1.0-r1
onlyone-r1 < 2
-1.0-r1 < 1.0-r2
busybox-1.36.1-r29 < 1.36.1-r31 extra
curl-8.9.1-r2 < 8.10.1-r0
";
    assert_eq!(
        parse_apk_version(text),
        ApkListing {
            updates: vec![update("curl", "8.9.1-r2", "8.10.1-r0")],
            unparsed: 5,
        }
    );
}

/// Proves that under the security scope only a kernel is due on Alpine (which has no security
/// origin), with a reboot needed and `security` false; the refresh runs first with exactly its
/// arguments, and hostkeeper's own package is not reported.
#[test]
fn security_scope_reports_only_kernels() {
    let (collector, fake) = apk(FIXTURE);
    let section = collect(&collector, LinuxScope::Security).expect("a section");
    assert_eq!(
        section,
        Section {
            due: vec![linux_item(
                "linux-lts",
                "6.6.54-r0",
                "6.6.58-r0",
                false,
                true
            )],
            ..Section::default()
        }
    );
    assert!(section.due[0].needs_reboot);
    assert!(!section.due[0].security);
    assert_eq!(fake.calls(), [refresh_args(), version_args()]);
}

/// Proves that under the all scope every update is reported with the installed version, except
/// hostkeeper's own package.
#[test]
fn all_scope_reports_every_update_but_our_own() {
    let (collector, _) = apk(FIXTURE);
    let section = collect(&collector, LinuxScope::All).expect("a section");
    assert_eq!(
        labels(&section),
        [
            "busybox",
            "musl",
            "ssl_client",
            "linux-lts",
            "openssl",
            "libcrypto3"
        ]
    );
    assert_eq!(
        section.due[1],
        linux_item("musl", "1.2.5-r0", "1.2.5-r1", false, false)
    );
}

/// Proves every kernel flavour of Alpine is a kernel, and the unknown lines are counted in the
/// section.
#[test]
fn every_kernel_flavour_is_a_kernel_and_unknown_lines_are_counted() {
    let text = "\
linux-virt-6.6.54-r0 < 6.6.58-r0
linux-edge-6.11.1-r0 < 6.11.5-r0
linux-stable-6.11.1-r0 < 6.11.5-r0
linux-rpi-6.6.54-r0 < 6.6.58-r0
linux-lts-doc-6.6.54-r0 < 6.6.58-r0
garbage
";
    let (collector, _) = apk(text);
    let section = collect(&collector, LinuxScope::Security).expect("a section");
    assert_eq!(
        labels(&section),
        ["linux-virt", "linux-edge", "linux-stable", "linux-rpi"]
    );
    assert!(section.due.iter().all(|item| item.needs_reboot));
    assert_eq!(section.unparsed, 1);
}

/// Proves a failed index refresh fails the collection with its step, code and the first line of
/// its standard error, before the listing runs.
#[test]
fn a_failed_refresh_fails_before_the_listing() {
    let (collector, fake) = apk(FIXTURE);
    fake.always(
        &refresh_args(),
        Ok(LaunchctlOutput::exit(
            99,
            "ERROR: https://mirror.example.org/alpine/v3.20/main: temporary error\n",
        )),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All).err(),
        Some(
            "apk update failed with exit code 99: ERROR: https://mirror.example.org/alpine/v3.20/main: temporary error"
                .to_owned()
        )
    );
    assert_eq!(fake.calls(), [refresh_args()]);
}

/// Proves a failed listing, and a listing or refresh that does not end by its deadline, fail
/// naming the step.
#[test]
fn a_failed_or_timed_out_step_fails() {
    let (collector, fake) = apk(FIXTURE);
    fake.always(&version_args(), Ok(LaunchctlOutput::exit(1, "")));
    assert_eq!(
        collect(&collector, LinuxScope::All).err(),
        Some("apk version failed with exit code 1".to_owned())
    );
    fake.always(
        &version_args(),
        Err(LaunchctlError::TimedOut {
            args: "version -l <".to_owned(),
        }),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All).err(),
        Some("apk version did not finish within its deadline".to_owned())
    );
    fake.always(
        &refresh_args(),
        Err(LaunchctlError::TimedOut {
            args: "update -q".to_owned(),
        }),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All).err(),
        Some("apk update did not finish within its deadline".to_owned())
    );
}

/// Proves the collector is named `apk`, which a failed look's reason starts with, and that the
/// collector of the host is built over the program and the deadlines the design names.
#[test]
fn the_collector_is_named_apk_and_runs_the_hosts_program() {
    assert_eq!(Apk::at_host().name(), "apk");
    assert_eq!(APK, "/sbin/apk");
    assert_eq!(APK_REFRESH_DEADLINE, Duration::from_secs(600));
    assert_eq!(APK_LIST_DEADLINE, Duration::from_secs(300));
}
