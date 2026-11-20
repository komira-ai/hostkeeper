//! The tests of `maintenance_dnf`: the two parsers over fixtures of a Rocky Linux 9 host and over
//! the lines they do not know, and the collector over a scripted `dnf`, asserted by equality.

use std::collections::BTreeSet;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::*;
use crate::maintenance_collect::LinuxScope;

const SECURITY_FIXTURE: &str = include_str!("../tests/fixtures/maintenance/dnf/security.txt");
const CHECK_FIXTURE: &str = include_str!("../tests/fixtures/maintenance/dnf/check_update.txt");

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn security_args() -> Vec<String> {
    args(&["updateinfo", "list", "--security", "-q"])
}

fn check_args() -> Vec<String> {
    args(&["check-update", "-q"])
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn update(name: &str, to: &str) -> DnfUpdate {
    DnfUpdate {
        name: name.to_owned(),
        to: to.to_owned(),
    }
}

/// A `dnf` that lists `advisories` and, exiting `code`, `updates`.
fn dnf(advisories: &str, code: i32, updates: &str) -> (Dnf, FakeLaunchctl) {
    let fake = FakeLaunchctl::new();
    fake.always(&security_args(), Ok(LaunchctlOutput::success(advisories)));
    fake.always(
        &check_args(),
        Ok(LaunchctlOutput {
            code: Some(code),
            stdout: updates.as_bytes().to_vec(),
            stderr: Vec::new(),
        }),
    );
    (Dnf::new(Box::new(fake.clone())), fake)
}

fn collect(dnf: &Dnf, scope: LinuxScope, own: &[&str]) -> Result<Section, String> {
    let own = names(own);
    dnf.collect(&Look {
        scope,
        own_packages: &own,
    })
}

fn labels(section: &Section) -> Vec<&str> {
    section.due.iter().map(|item| item.label.as_str()).collect()
}

/// Proves the fixture of `dnf updateinfo list --security -q` gives the names, without epoch,
/// version, release or architecture, of the packages an advisory covers, and no unknown line.
#[test]
fn security_fixture_gives_the_package_names() {
    assert_eq!(
        parse_dnf_security(SECURITY_FIXTURE),
        SecurityListing {
            packages: names(&[
                "hostkeeper",
                "kernel",
                "kernel-core",
                "kernel-modules",
                "openssl",
                "openssl-libs",
            ]),
            unparsed: 0,
        }
    );
}

/// Proves a line that is not an advisory row is counted: a row of two fields, a row whose severity
/// does not end `/Sec.`, and a package string that has no architecture, no release, or no name.
#[test]
fn security_lines_that_are_not_rows_are_counted() {
    let text = "\
RLSA-2026:1 Important/Sec.
RLBA-2026:2 bugfix bash-5.1.8-9.el9.x86_64
RLSA-2026:3 Low/Sec. nodots
RLSA-2026:4 Low/Sec. onlyname.x86_64
RLSA-2026:5 Low/Sec. -1-2.x86_64
something else entirely here
RLSA-2026:6 Low/Sec. tzdata-2024b-2.el9.noarch
";
    assert_eq!(
        parse_dnf_security(text),
        SecurityListing {
            packages: names(&["tzdata"]),
            unparsed: 6,
        }
    );
}

/// Proves the fixture of `dnf check-update -q` gives every update with its version and release: a
/// name wrapped onto two lines is one row, the epoch stays in the version, the obsoleting section
/// gives its new package and skips the indented line of the one it replaces, and a `Security: `
/// line is known.
#[test]
fn check_update_fixture_gives_the_updates() {
    assert_eq!(
        parse_dnf_check_update(CHECK_FIXTURE),
        UpdateListing {
            updates: vec![
                update("kernel", "5.14.0-503.14.1.el9_5"),
                update("kernel-core", "5.14.0-503.14.1.el9_5"),
                update("kernel-modules", "5.14.0-503.14.1.el9_5"),
                update("openssl", "1:3.2.2-6.el9_5"),
                update("openssl-libs", "1:3.2.2-6.el9_5"),
                update("bash", "5.1.8-9.el9"),
                update("hostkeeper", "1.9.0-1"),
                update(
                    "python3-libs-with-a-rather-long-package-name",
                    "3.9.19-8.el9_5.1"
                ),
                update("tzdata", "2024b-2.el9"),
                update("grub2-tools", "1:2.06-94.el9_5"),
            ],
            unparsed: 0,
        }
    );
}

/// Proves the lines `check-update` does not know are counted: a row without an architecture, a
/// row of the wrong width, and an indented line outside the obsoleting section.
#[test]
fn check_update_lines_that_are_not_rows_are_counted() {
    let text = "\
Last metadata expiration check: 0:01:00 ago on Fri 20 Nov 2026 06:41:09 AM UTC.
noarch 1.0-1 baseos
bash.x86_64 5.1.8-9.el9
\tstray.x86_64 1.0-1 baseos
what is this line
tzdata.noarch 2024b-2.el9 baseos
";
    assert_eq!(
        parse_dnf_check_update(text),
        UpdateListing {
            updates: vec![update("tzdata", "2024b-2.el9")],
            unparsed: 4,
        }
    );
}

/// Proves a long name alone on a line whose next line is not its version and repository is
/// counted, and the next line is read as itself.
#[test]
fn a_wrapped_name_without_its_continuation_is_counted() {
    let text = "\
a-very-long-name.x86_64
bash.x86_64 5.1.8-9.el9 baseos
b-very-long-name.x86_64
\tone-field-only
c-very-long-name.x86_64
\t1.0-1 appstream
";
    assert_eq!(
        parse_dnf_check_update(text),
        UpdateListing {
            updates: vec![
                update("bash", "5.1.8-9.el9"),
                update("c-very-long-name", "1.0-1"),
            ],
            unparsed: 3,
        }
    );
}

/// Proves a name alone on the last line, with no continuation, is counted.
#[test]
fn a_wrapped_name_at_the_end_is_counted() {
    assert_eq!(
        parse_dnf_check_update("tzdata.noarch\n"),
        UpdateListing {
            updates: Vec::new(),
            unparsed: 1,
        }
    );
}

/// Proves an empty listing is no updates and no unknown lines.
#[test]
fn nothing_listed_is_nothing() {
    assert_eq!(parse_dnf_check_update(""), UpdateListing::default());
    assert_eq!(parse_dnf_security("\n"), SecurityListing::default());
}

/// Proves that under the security scope the collector reports the packages with an advisory, runs
/// the two listings with exactly their arguments in order, leaves out a package that is not a
/// security update and hostkeeper's own, gives no installed version, and marks a kernel as needing
/// a reboot.
#[test]
fn security_scope_reports_security_updates_and_kernels() {
    let (collector, fake) = dnf(SECURITY_FIXTURE, 100, CHECK_FIXTURE);
    let section = collect(&collector, LinuxScope::Security, &["hostkeeper"]).expect("a section");
    assert_eq!(
        labels(&section),
        [
            "kernel",
            "kernel-core",
            "kernel-modules",
            "openssl",
            "openssl-libs"
        ]
    );
    assert_eq!(
        section.due[0],
        pb::DueItem {
            kind: pb::MaintenanceKind::Linux.into(),
            from: String::new(),
            to: "5.14.0-503.14.1.el9_5".to_owned(),
            needs_reboot: true,
            label: "kernel".to_owned(),
            security: true,
        }
    );
    assert_eq!(
        section.due[2],
        linux_item("kernel-modules", "", "5.14.0-503.14.1.el9_5", true, false)
    );
    assert_eq!(
        section.due[3],
        linux_item("openssl", "", "1:3.2.2-6.el9_5", true, false)
    );
    assert_eq!(section.unparsed, 0);
    assert_eq!(section.host, pb::MaintenanceHost::default());
    assert_eq!(fake.calls(), [security_args(), check_args()]);
}

/// Proves that under the all scope every update is reported, a package without an advisory with
/// `security` false, and hostkeeper's own package still never is.
#[test]
fn all_scope_reports_every_update_but_our_own() {
    let (collector, _) = dnf(SECURITY_FIXTURE, 100, CHECK_FIXTURE);
    let section = collect(&collector, LinuxScope::All, &["hostkeeper"]).expect("a section");
    assert_eq!(
        labels(&section),
        [
            "kernel",
            "kernel-core",
            "kernel-modules",
            "openssl",
            "openssl-libs",
            "bash",
            "python3-libs-with-a-rather-long-package-name",
            "tzdata",
            "grub2-tools",
        ]
    );
    assert_eq!(
        section.due[5],
        linux_item("bash", "", "5.1.8-9.el9", false, false)
    );
}

/// Proves a kernel package with no advisory is due under the security scope, with a reboot
/// needed and `security` false.
#[test]
fn a_kernel_without_an_advisory_is_due_under_security() {
    let (collector, _) = dnf(
        "",
        100,
        "kernel-uek.x86_64 5.15.0-300.el9uek ol9_UEKR7\nbash.x86_64 5.1.8-9.el9 baseos\n",
    );
    let section = collect(&collector, LinuxScope::Security, &[]).expect("a section");
    assert_eq!(
        section.due,
        [linux_item(
            "kernel-uek",
            "",
            "5.15.0-300.el9uek",
            false,
            true
        )]
    );
    assert!(section.due[0].needs_reboot);
}

/// Proves exit code 0 of `check-update` is no updates, and the unknown lines of both listings are
/// added up.
#[test]
fn exit_zero_is_nothing_due_and_unknown_lines_are_added() {
    let (collector, _) = dnf("surprise\n", 0, "");
    assert_eq!(
        collect(&collector, LinuxScope::All, &[]),
        Ok(Section {
            unparsed: 1,
            ..Section::default()
        })
    );
    let (collector, _) = dnf("surprise\n", 100, "surprise\nsurprise\n");
    assert_eq!(
        collect(&collector, LinuxScope::All, &[])
            .expect("a section")
            .unparsed,
        3
    );
}

/// Proves an exit code of `check-update` that is neither 0 nor 100 fails with its step, the code
/// and the first line of its standard error.
#[test]
fn another_exit_of_check_update_fails() {
    let (collector, fake) = dnf("", 0, "");
    fake.always(
        &check_args(),
        Ok(LaunchctlOutput::exit(
            1,
            "Error: Failed to download metadata for repo 'baseos'\nmore\n",
        )),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All, &[]).err(),
        Some(
            "dnf check-update failed with exit code 1: Error: Failed to download metadata for repo 'baseos'"
                .to_owned()
        )
    );
}

/// Proves a failure of the advisory listing, including exit code 100 which only `check-update`
/// may give, ends the collection before `check-update` runs.
#[test]
fn a_failed_advisory_listing_fails_before_check_update() {
    let (collector, fake) = dnf("", 100, "");
    fake.always(&security_args(), Ok(LaunchctlOutput::exit(100, "")));
    assert_eq!(
        collect(&collector, LinuxScope::All, &[]).err(),
        Some("dnf updateinfo list failed with exit code 100".to_owned())
    );
    assert_eq!(fake.calls(), [security_args()]);
}

/// Proves a listing that does not end by its deadline fails naming the step, for each listing.
#[test]
fn a_listing_that_times_out_fails() {
    let (collector, fake) = dnf("", 100, "");
    fake.always(
        &check_args(),
        Err(LaunchctlError::TimedOut {
            args: "check-update -q".to_owned(),
        }),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All, &[]).err(),
        Some("dnf check-update did not finish within its deadline".to_owned())
    );
    fake.always(
        &security_args(),
        Err(LaunchctlError::TimedOut {
            args: "updateinfo list --security -q".to_owned(),
        }),
    );
    assert_eq!(
        collect(&collector, LinuxScope::All, &[]).err(),
        Some("dnf updateinfo list did not finish within its deadline".to_owned())
    );
}

/// Proves the collector is named `dnf`, which a failed look's reason starts with, and that the
/// collector of the host is built over the program and deadline the design names.
#[test]
fn the_collector_is_named_dnf_and_runs_the_hosts_program() {
    assert_eq!(Dnf::at_host().name(), "dnf");
    assert_eq!(DNF, "/usr/bin/dnf");
    assert_eq!(DNF_DEADLINE, Duration::from_secs(300));
}
