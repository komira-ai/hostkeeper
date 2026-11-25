//! The tests of the collector `boot`: over a scripted `sysctl` and a boot id the test sets, the
//! boot id and the boot time it reports, the program it runs, and that no failure of either
//! fails the look; each section asserted by equality.

use std::collections::BTreeSet;

use hostkeeper_launchd::{
    BOOT_SESSION_UUID, FakeLaunchctl, LaunchctlError, LaunchctlOutput, parse_boot_session_uuid,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::MAX_KERNEL_TEXT;
use hostkeeper_steps::{BootId, BootIdError};

use super::*;
use crate::maintenance_collect::LinuxScope;

/// `sysctl -n kern.boottime`, in the form a Mac prints it.
const BOOTTIME: &str = include_str!("../tests/fixtures/maintenance/mac/sysctl_kern_boottime.txt");
/// `sysctl -n kern.bootsessionuuid`, in the form a Mac prints it.
const BOOT_SESSION: &str =
    include_str!("../../hostkeeper-launchd/tests/fixtures/sysctl-bootsessionuuid.txt");

/// The boot id in [`BOOT_SESSION`].
const BOOT_ID: &str = "6F1E9C2A-4B3D-4E8F-A1C7-0D5B9E2F3A64";
/// The `sec` of [`BOOTTIME`].
const BOOT_UNIX: u64 = 1_795_176_765;

/// A boot id source that answers what the test gives it.
enum Boot {
    /// The value of `kern.bootsessionuuid`, read as the step record reads it.
    Sysctl(&'static str),
    /// A boot id of any text, for a length the kernel never gives.
    Text(String),
    /// A source that cannot be read.
    Unreadable,
}

impl BootIdSource for Boot {
    fn read(&self) -> Result<BootId, BootIdError> {
        match self {
            Boot::Sysctl(text) => parse_boot_session_uuid(text),
            Boot::Text(text) => BootId::new(BOOT_SESSION_UUID, text),
            Boot::Unreadable => Err(BootIdError::Read {
                from: BOOT_SESSION_UUID,
                source: std::io::Error::other("sysctlbyname failed"),
            }),
        }
    }
}

fn boottime_args() -> Vec<String> {
    BOOTTIME_ARGS.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A `sysctl` whose `-n kern.boottime` answers `answer`.
fn sysctl(answer: Result<LaunchctlOutput, LaunchctlError>) -> FakeLaunchctl {
    let fake = FakeLaunchctl::new();
    fake.always(&boottime_args(), answer);
    fake
}

/// What the collector over `boot` and `sysctl` gives.
fn collect(boot: Boot, sysctl: &FakeLaunchctl) -> Result<Section, String> {
    let own = BTreeSet::new();
    MacBoot::new(Box::new(boot), Box::new(sysctl.clone())).collect(&Look {
        scope: LinuxScope::Security,
        own_packages: &own,
    })
}

/// The section holding `boot_id` and `boot_unix` and nothing else.
fn section(boot_id: &str, boot_unix: u64) -> Section {
    Section {
        host: pb::MaintenanceHost {
            boot_id: boot_id.to_owned(),
            boot_unix,
            ..pb::MaintenanceHost::default()
        },
        ..Section::default()
    }
}

/// Proves the collector reports the Mac's boot id as `kern.bootsessionuuid` names it and the
/// `sec` of `kern.boottime`, sets no other field, and runs `sysctl -n kern.boottime` once.
#[test]
fn a_mac_reports_its_boot_id_and_boot_time() {
    let program = sysctl(Ok(LaunchctlOutput::success(BOOTTIME)));
    assert_eq!(
        collect(Boot::Sysctl(BOOT_SESSION), &program),
        Ok(section(BOOT_ID, BOOT_UNIX))
    );
    assert_eq!(program.calls(), [boottime_args()]);
}

/// Proves a boot id longer than the wire's bound is cut to [`MAX_KERNEL_TEXT`] bytes, as
/// the Linux collector cuts its own.
#[test]
fn a_long_boot_id_is_cut_to_its_bound() {
    let long = "b".repeat(MAX_KERNEL_TEXT + 10);
    let program = sysctl(Ok(LaunchctlOutput::success(BOOTTIME)));
    assert_eq!(
        collect(Boot::Text(long), &program),
        Ok(section(&"b".repeat(MAX_KERNEL_TEXT), BOOT_UNIX))
    );
}

/// Proves a boot id that cannot be read, is empty or is not a UUID leaves `boot_id` empty and
/// keeps the boot time, without failing the look.
#[test]
fn an_unreadable_boot_id_leaves_it_empty() {
    for boot in [
        Boot::Unreadable,
        Boot::Sysctl("\n"),
        Boot::Sysctl("not-a-uuid\n"),
    ] {
        let program = sysctl(Ok(LaunchctlOutput::success(BOOTTIME)));
        assert_eq!(collect(boot, &program), Ok(section("", BOOT_UNIX)));
    }
}

/// Proves a `sysctl` that cannot start, does not end by its deadline, exits non-zero or writes
/// its output's bound leaves `boot_unix` 0 and keeps the boot id, without failing the look.
#[test]
fn a_failing_sysctl_leaves_the_boot_time_zero() {
    let answers = [
        Err(LaunchctlError::Spawn {
            args: "-n kern.boottime".to_owned(),
            why: "No such file or directory".to_owned(),
        }),
        Err(LaunchctlError::TimedOut {
            args: "-n kern.boottime".to_owned(),
        }),
        Ok(LaunchctlOutput::exit(
            1,
            "sysctl: unknown oid 'kern.boottime'\n",
        )),
        Ok(LaunchctlOutput::success(
            &"x".repeat(hostkeeper_launchd::launchctl::OUTPUT_MAX),
        )),
    ];
    for answer in answers {
        let program = sysctl(answer);
        assert_eq!(
            collect(Boot::Sysctl(BOOT_SESSION), &program),
            Ok(section(BOOT_ID, 0))
        );
        assert_eq!(program.calls(), [boottime_args()]);
    }
}

/// Proves the boot time is read from the `sec` of the braces' text, wherever it stands in them
/// and with or without spaces, and anything of another shape is 0: no braces, no closing brace,
/// no `sec`, a `sec` that is not a whole number or is negative, and an empty text.
#[test]
fn the_boot_time_is_the_sec_of_the_braces() {
    assert_eq!(parse_kern_boottime(BOOTTIME), BOOT_UNIX);
    let cases = [
        ("{ sec = 1795176765, usec = 482911 }\n", BOOT_UNIX),
        ("  {sec=1795176765,usec=482911}", BOOT_UNIX),
        ("{ usec = 482911, sec = 1795176765 } Fri", BOOT_UNIX),
        ("{ sec = 0, usec = 0 }", 0),
        ("1795176765\n", 0),
        ("sec = 1795176765, usec = 482911\n", 0),
        ("{ sec = 1795176765, usec = 482911\n", 0),
        ("{ usec = 482911 }", 0),
        ("{ secs = 1795176765 }", 0),
        ("{ sec = 17951.76765 }", 0),
        ("{ sec = -1795176765 }", 0),
        ("{ sec }", 0),
        ("Fri Nov 20 12:12:45 2026 { sec = 1795176765 }", 0),
        ("", 0),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_kern_boottime(text), expected, "{text:?}");
    }
}

/// Proves the host's collector is named `boot`, the name the Mac's list shows it by.
#[test]
fn the_collector_is_named_boot() {
    assert_eq!(MacBoot::at_host().name(), "boot");
}
