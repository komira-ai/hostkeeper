//! `hostkeeper daemon --maintenance-macos-major` from the command line to the heartbeat's report,
//! and a Mac's look whose program does not end by its deadline: the flag is parsed as the daemon
//! parses it, and the setting it gives drives the daemon's maintenance source
//! (`cli::maintenance_source`, as `cli::host_links` makes it) over a daemon on a scratch rig and a
//! Mac's two collectors, `macos` and `mac`, over a scripted `sw_vers`, `ioreg`, `softwareupdate`,
//! `fdesetup` and `pkgutil` and a fake data volume. A dispatcher that runs each look where it is
//! started and a clock the test holds keep it hermetic.

mod support;

use std::sync::Arc;

use clap::Parser as _;
use hostkeeper_daemon::cli::{Cli, Command, maintenance_source};
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::mac::ioreg_args;
use hostkeeper_daemon::maintenance_collect::{Collector, HostRoot, LinuxScope};
use hostkeeper_daemon::maintenance_mac::{FreeSpace, MacPorts, MacReadiness};
use hostkeeper_daemon::maintenance_macos::{Macos, MacosPorts};
use hostkeeper_daemon::maintenance_softwareupdate::MacosMajor;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_link::{Dispatch, Job};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Rig;

/// `softwareupdate --list` on a Mac of macOS 26: a minor update and a major upgrade.
const LIST: &str = include_str!("fixtures/maintenance/softwareupdate/macos26_list_major.txt");
/// `softwareupdate --list-full-installers` on a Mac of macOS 26.
const FULL: &str = include_str!("fixtures/maintenance/softwareupdate/macos26_full_installers.txt");
const SW_VERS: &str = include_str!("fixtures/maintenance/mac/sw_vers_macos26.txt");
const IOREG: &str = include_str!("fixtures/maintenance/mac/ioreg.txt");
const PKGUTIL: &str = include_str!("fixtures/maintenance/mac/pkgutil_clt.txt");

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

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The report of the daemon's heartbeat after one look, for the command line `argv` (after
/// `hostkeeper daemon`), on a Mac whose `softwareupdate --list` answers `list`.
fn reported(
    name: &str,
    argv: &[&str],
    list: Result<LaunchctlOutput, LaunchctlError>,
) -> pb::MaintenanceReport {
    let mut line = vec!["hostkeeper", "daemon"];
    line.extend_from_slice(argv);
    let Command::Daemon(flags) = Cli::try_parse_from(line).expect("the flags parse").command else {
        panic!("`hostkeeper daemon` is the daemon verb");
    };

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
        Box::new(Macos::new(
            MacosPorts {
                sw_vers: program(&[], Ok(LaunchctlOutput::success(SW_VERS))),
                ioreg: Box::new(ioreg),
                list: program(&["--list"], list),
                full: program(
                    &["--list-full-installers"],
                    Ok(LaunchctlOutput::success(FULL)),
                ),
            },
            MacosMajor::from_flag(flags.maintenance_macos_major),
        )),
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
        LinuxScope::default(),
        collectors,
        Arc::new(FakeClock::new(2_000_000_000, 0)),
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

/// Proves the parsed flag decides whether a major upgrade is due: with no flag only the minor
/// update `softwareupdate` lists is due, though it lists the major upgrade too; with
/// `--maintenance-macos-major` the major upgrade is due after it. In each, the report carries the
/// Mac's facts as both collectors read them, and no line was unknown.
/// Catches: the flag parsed and not passed on to the collector, or a major upgrade due by default.
#[test]
fn the_major_flag_decides_whether_a_major_upgrade_is_due() {
    let listed = || Ok(LaunchctlOutput::success(LIST));
    let without = reported("macos-major-default", &[], listed());
    assert_eq!(without.look_failed, "");
    assert_eq!(labels(&without), ["macOS Tahoe 26.7-25H62"]);
    let with = reported("macos-major-due", &["--maintenance-macos-major"], listed());
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
        &[],
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

/// Proves `--maintenance-macos-major` takes no value: a value after it is refused as a usage
/// error (exit 2, as clap reports it).
#[test]
fn the_major_flag_takes_no_value() {
    let error = Cli::try_parse_from(["hostkeeper", "daemon", "--maintenance-macos-major=yes"])
        .expect_err("a value given to a switch");
    assert_eq!(error.exit_code(), 2);
}
