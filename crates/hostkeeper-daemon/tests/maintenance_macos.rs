//! A Mac's heartbeat report, and a Mac's look whose program does not end by its deadline: the
//! daemon's maintenance source (`cli::maintenance_source`, as `cli::host_links` makes it, with
//! nothing of the command line deciding what is due) runs over a daemon on a scratch rig and a
//! Mac's collectors `macos`, `boot` and `mac`, over a scripted `sw_vers`, `ioreg`,
//! `softwareupdate`, `sysctl`, `fdesetup` and `pkgutil`, a boot id the test gives and a fake data
//! volume; and the boot facts of a Mac's report when `sysctl` answers otherwise. A dispatcher that
//! runs each look where it is started and a clock the test holds keep it hermetic.

mod support;

use std::sync::Arc;

use hostkeeper_daemon::cli::maintenance_source;
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::mac::ioreg_args;
use hostkeeper_daemon::maintenance_boot::{BOOTTIME_ARGS, MacBoot};
use hostkeeper_daemon::maintenance_collect::{Collector, HostRoot, LinuxScope};
use hostkeeper_daemon::maintenance_mac::{FreeSpace, MacPorts, MacReadiness};
use hostkeeper_daemon::maintenance_macos::{Macos, MacosPorts};
use hostkeeper_launchd::{
    BOOT_SESSION_UUID, FakeLaunchctl, LaunchctlError, LaunchctlOutput, PlatformKind,
    parse_boot_session_uuid,
};
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_link::{Dispatch, Job};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};
use support::Rig;

/// `softwareupdate --list` on a Mac of macOS 26: a minor update and a major upgrade.
const LIST: &str = include_str!("fixtures/maintenance/softwareupdate/macos26_list_major.txt");
/// `softwareupdate --list-full-installers` on a Mac of macOS 26.
const FULL: &str = include_str!("fixtures/maintenance/softwareupdate/macos26_full_installers.txt");
const SW_VERS: &str = include_str!("fixtures/maintenance/mac/sw_vers_macos26.txt");
const IOREG: &str = include_str!("fixtures/maintenance/mac/ioreg.txt");
const PKGUTIL: &str = include_str!("fixtures/maintenance/mac/pkgutil_clt.txt");
/// `sysctl -n kern.boottime`, in the form a Mac prints it.
const BOOTTIME: &str = include_str!("fixtures/maintenance/mac/sysctl_kern_boottime.txt");
/// `sysctl -n kern.bootsessionuuid`, in the form a Mac prints it.
const BOOT_SESSION: &str =
    include_str!("../../hostkeeper-launchd/tests/fixtures/sysctl-bootsessionuuid.txt");

/// The boot id in [`BOOT_SESSION`].
const BOOT_ID: &str = "6F1E9C2A-4B3D-4E8F-A1C7-0D5B9E2F3A64";
/// The `sec` of [`BOOTTIME`].
const BOOT_UNIX: u64 = 1_795_176_765;
/// The second the test's clock holds, at which each look starts.
const NOW: u64 = 2_000_000_000;

/// The free bytes of the fake data volume.
const FREE: u64 = 80_000_000_000;

/// A dispatcher that runs each job where it is started.
struct Inline;

impl Dispatch for Inline {
    fn start(&self, job: Job) {
        job();
    }
}

/// A data volume with [`FREE`] bytes free.
struct Space;

impl FreeSpace for Space {
    fn free_bytes(&self) -> Result<u64, String> {
        Ok(FREE)
    }
}

/// The Mac's boot id: `kern.bootsessionuuid` read as the step record reads it, or none.
struct Boot(Option<&'static str>);

impl BootIdSource for Boot {
    fn read(&self) -> Result<BootId, BootIdError> {
        match self.0 {
            Some(text) => parse_boot_session_uuid(text),
            None => Err(BootIdError::Read {
                from: BOOT_SESSION_UUID,
                source: std::io::Error::other("sysctlbyname failed"),
            }),
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The collector `boot` over the boot id `boot` and a `sysctl -n kern.boottime` answering
/// `boottime`.
fn boot(boot: Option<&'static str>, boottime: Result<LaunchctlOutput, LaunchctlError>) -> MacBoot {
    let sysctl = FakeLaunchctl::new();
    sysctl.always(&args(&BOOTTIME_ARGS), boottime);
    MacBoot::new(Box::new(Boot(boot)), Box::new(sysctl))
}

/// The collector `boot` of a Mac whose boot id and boot time read as the fixtures give them.
fn readable_boot() -> MacBoot {
    boot(Some(BOOT_SESSION), Ok(LaunchctlOutput::success(BOOTTIME)))
}

/// The report of the daemon's heartbeat after one look, on a Mac whose `softwareupdate --list`
/// answers `list` and whose boot facts read as the fixtures give them.
fn reported(name: &str, list: Result<LaunchctlOutput, LaunchctlError>) -> pb::MaintenanceReport {
    reported_with(name, list, readable_boot())
}

/// [`reported`], with the collector `boot` given.
fn reported_with(
    name: &str,
    list: Result<LaunchctlOutput, LaunchctlError>,
    boot: MacBoot,
) -> pb::MaintenanceReport {
    let rig = Rig::new(name);
    let daemon = Arc::new(Daemon::new(
        support::config(),
        rig.dirs(),
        rig.ports(),
        support::OWN,
    ));
    let program = |argv: &[&str], answer: Result<LaunchctlOutput, LaunchctlError>| {
        let fake = FakeLaunchctl::new();
        fake.always(&args(argv), answer);
        Box::new(fake)
    };
    let ioreg = FakeLaunchctl::new();
    ioreg.always(&ioreg_args(), Ok(LaunchctlOutput::success(IOREG)));
    let fdesetup = FakeLaunchctl::new();
    fdesetup.always(
        &args(&["status"]),
        Ok(LaunchctlOutput::success("FileVault is Off.\n")),
    );
    fdesetup.always(
        &args(&["supportsauthrestart"]),
        Ok(LaunchctlOutput::success("true\n")),
    );
    let collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(Macos::new(MacosPorts {
            sw_vers: program(&[], Ok(LaunchctlOutput::success(SW_VERS))),
            ioreg: Box::new(ioreg),
            list: program(&["--list"], list),
            full: program(
                &["--list-full-installers"],
                Ok(LaunchctlOutput::success(FULL)),
            ),
        })),
        Box::new(boot),
        Box::new(MacReadiness::new(MacPorts {
            fdesetup: Box::new(fdesetup),
            pkgutil: program(
                &["--pkg-info=com.apple.pkg.CLTools_Executables"],
                Ok(LaunchctlOutput::success(PKGUTIL)),
            ),
            space: Box::new(Space),
            root: HostRoot::at(rig.scratch.path()),
        })),
    ];
    let source = maintenance_source(
        &daemon,
        PlatformKind::Macos,
        LinuxScope::default(),
        collectors,
        Arc::new(FakeClock::new(NOW, 0)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    )
    .expect("a Mac with its collectors has a maintenance source");
    source.report();
    source.report()
}

fn labels(report: &pb::MaintenanceReport) -> Vec<&str> {
    report.due.iter().map(|item| item.label.as_str()).collect()
}

/// Proves the daemon, with nothing of its command line deciding it, reports the minor
/// update `softwareupdate` lists due and then the newest major upgrade: whether the major upgrade
/// counts is the group's `macos_major`, judged by the coordinator. The report carries the Mac's
/// facts as its collectors read them, its boot id and boot time among them, and no line was
/// unknown.
/// Catches: a major upgrade left out of the report unless the daemon is told, so a group
/// that allows one never has it due.
#[test]
fn the_report_holds_the_newest_major_upgrade_after_the_minor_update() {
    let with = reported("macos-major-due", Ok(LaunchctlOutput::success(LIST)));
    assert_eq!(with.look_failed, "");
    assert_eq!(
        labels(&with),
        ["macOS Tahoe 26.7-25H62", "macOS 27.1-26B70"]
    );
    assert_eq!(
        with.host,
        Some(pb::MaintenanceHost {
            os_name: "macOS".to_owned(),
            os_version: "26.6".to_owned(),
            os_build: "25G62".to_owned(),
            hardware_serial: "H7K2XQ9WLM".to_owned(),
            boot_id: BOOT_ID.to_owned(),
            boot_unix: BOOT_UNIX,
            ..pb::MaintenanceHost::default()
        })
    );
    assert_eq!(
        with.mac,
        Some(pb::MacUpdateFacts {
            filevault: pb::FileVaultState::Off.into(),
            authrestart_supported: true,
            data_free_bytes: Some(FREE),
            command_line_tools: Some(pb::CommandLineToolsState {
                receipt: true,
                files: false,
                version: "26.4.0.0.1.1771234567".to_owned(),
            }),
            ..pb::MacUpdateFacts::default()
        })
    );
    assert_eq!(with.unparsed_lines, 0);
}

/// Proves a `softwareupdate --list` that does not finish by its deadline gives a failed look, not
/// a hang: the report returns, names the collector and the step, and holds nothing due.
#[test]
fn a_listing_past_its_deadline_is_a_failed_look() {
    let report = reported(
        "macos-deadline",
        Err(LaunchctlError::TimedOut {
            args: "--list".to_owned(),
        }),
    );
    assert_eq!(
        report.look_failed,
        "macos: softwareupdate --list did not finish within its deadline"
    );
    assert_eq!(report.due, Vec::new());
    assert_eq!(report.looked_unix, 0);
}

/// The report of a Mac whose `softwareupdate --list` lists the fixture's
/// updates, with the collector `boot` given.
fn reported_boot(name: &str, boot: MacBoot) -> pb::MaintenanceReport {
    reported_with(name, Ok(LaunchctlOutput::success(LIST)), boot)
}

/// `report` as it is with its boot id and boot time set to `boot_id` and `boot_unix`.
fn with_boot(
    report: &pb::MaintenanceReport,
    boot_id: &str,
    boot_unix: u64,
) -> pb::MaintenanceReport {
    let mut report = report.clone();
    let host = report.host.get_or_insert_with(pb::MaintenanceHost::default);
    host.boot_id = boot_id.to_owned();
    host.boot_unix = boot_unix;
    report
}

/// Proves a Mac's report carries the exact boot id `kern.bootsessionuuid` names and the exact
/// `sec` of `kern.boottime`, in a look that succeeded at the clock's second.
/// Catches: a Mac's report with no boot id or boot time, so that a restart cannot be seen in it.
#[test]
fn a_macs_report_carries_its_boot_id_and_boot_time() {
    let report = reported_boot("macos-boot", readable_boot());
    assert_eq!(report.look_failed, "");
    assert_eq!(report.looked_unix, NOW);
    let host = report.host.expect("the host's facts");
    assert_eq!(host.boot_id, BOOT_ID);
    assert_eq!(host.boot_unix, BOOT_UNIX);
    assert_eq!(host.running_kernel, "");
}

/// Proves a `kern.boottime` of another shape gives a boot time of 0 and leaves every other field
/// of the report as it is when the boot time reads: the boot id, the Mac's facts, what is due.
#[test]
fn a_boot_time_of_another_shape_is_zero_and_the_rest_is_unchanged() {
    let readable = reported_boot("macos-boot-readable", readable_boot());
    let other = reported_boot(
        "macos-boot-other-shape",
        boot(
            Some(BOOT_SESSION),
            Ok(LaunchctlOutput::success("1795176765\n")),
        ),
    );
    assert_eq!(other, with_boot(&readable, BOOT_ID, 0));
}

/// Proves a boot id that cannot be read and a `sysctl` that exits non-zero leave the boot id empty
/// and the boot time 0 without failing the look: the rest of the report is as it is when both
/// read.
/// Catches: a failed boot fact failing the whole look, which would hide every other Mac fact.
#[test]
fn a_failing_sysctl_leaves_the_boot_facts_empty_and_the_look_succeeds() {
    let readable = reported_boot("macos-boot-both", readable_boot());
    let failing = reported_boot(
        "macos-boot-failing",
        boot(
            None,
            Ok(LaunchctlOutput::exit(
                1,
                "sysctl: unknown oid 'kern.boottime'\n",
            )),
        ),
    );
    let boot_of = |report: &pb::MaintenanceReport| {
        report
            .host
            .as_ref()
            .map(|host| (host.boot_id.clone(), host.boot_unix))
    };
    assert_eq!(boot_of(&readable), Some((BOOT_ID.to_owned(), BOOT_UNIX)));
    assert_eq!(failing.look_failed, "");
    assert_eq!(failing, with_boot(&readable, "", 0));
}

/// Proves a `sysctl -n kern.boottime` that does not finish by its deadline ends the look, which
/// succeeds at the clock's second with the boot id read and the boot time 0, rather than holding
/// the report.
#[test]
fn a_sysctl_past_its_deadline_ends_the_look() {
    let readable = reported_boot("macos-boot-in-time", readable_boot());
    let late = reported_boot(
        "macos-boot-deadline",
        boot(
            Some(BOOT_SESSION),
            Err(LaunchctlError::TimedOut {
                args: "-n kern.boottime".to_owned(),
            }),
        ),
    );
    assert_eq!(late.look_failed, "");
    assert_eq!(late.looked_unix, NOW);
    assert_eq!(late, with_boot(&readable, BOOT_ID, 0));
}
