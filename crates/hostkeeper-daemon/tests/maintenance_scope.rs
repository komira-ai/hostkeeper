//! `hostkeeper daemon --maintenance-linux-scope` from the command line to the heartbeat's report:
//! the flag is parsed as the daemon parses it, and the scope it gives drives the daemon's
//! maintenance source (`cli::maintenance_source`, as `cli::host_links` makes it) over a daemon on a
//! scratch rig whose `placed.json` records a service's package, and the `apt` collector over a
//! fake `apt-get` that writes an Ubuntu 24.04 simulation. A dispatcher that runs each look where
//! it is started and a clock the test holds keep it hermetic.

mod support;

use std::sync::Arc;

use clap::Parser as _;
use hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;
use hostkeeper_daemon::cli::{Cli, Command, maintenance_source};
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::maintenance_apt::Apt;
use hostkeeper_daemon::maintenance_collect::Collector;
use hostkeeper_daemon::testing::RecordedWaits;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_link::{Dispatch, Job};
use hostkeeper_placed::InstalledEntry;
use support::Rig;

/// `apt-get --simulate -qq dist-upgrade` on an Ubuntu 24.04 host: security updates, a new kernel,
/// `vim-tiny` from `noble-updates` alone, and hostkeeper's own package.
const UBUNTU: &str = include_str!("fixtures/maintenance/apt/ubuntu_2404.txt");

/// A dispatcher that runs each job where it is started.
struct Inline;

impl Dispatch for Inline {
    fn start(&self, job: Job) {
        job();
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The labels the daemon's heartbeat reports due after one look, for the command line `argv`
/// (after `hostkeeper daemon`), on a daemon whose `placed.json` records `openssl` as the package
/// of its service `tls`.
fn reported(name: &str, argv: &[&str]) -> Vec<String> {
    let mut line = vec!["hostkeeper", "daemon"];
    line.extend_from_slice(argv);
    let Command::Daemon(flags) = Cli::try_parse_from(line).expect("the flags parse").command else {
        panic!("`hostkeeper daemon` is the daemon verb");
    };

    let rig = Rig::new(name);
    let mut record = rig.placed_record();
    let mut entry = InstalledEntry::new("apt");
    entry.package = Some("openssl".to_owned());
    record.installed.insert("tls".to_owned(), entry);
    rig.save_placed(&record);
    let daemon = Arc::new(Daemon::new(
        support::config(),
        rig.dirs(),
        rig.ports(),
        support::OWN,
    ));

    let apt_get = FakeLaunchctl::new();
    apt_get.always(&args(&["update", "-qq"]), Ok(LaunchctlOutput::success("")));
    apt_get.always(
        &args(&["--simulate", "-qq", "dist-upgrade"]),
        Ok(LaunchctlOutput::success(UBUNTU)),
    );
    let collectors: Vec<Box<dyn Collector>> = vec![Box::new(Apt::new(
        Box::new(apt_get.clone()),
        Box::new(apt_get),
        RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS).0,
    ))];
    let source = maintenance_source(
        &daemon,
        flags.maintenance_linux_scope,
        collectors,
        Arc::new(FakeClock::new(2_000_000_000, 0)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    )
    .expect("a host with a collector has a maintenance source");
    source.report();
    let report = source.report();
    assert_eq!(report.look_failed, "");
    report.due.into_iter().map(|item| item.label).collect()
}

/// Proves the parsed flag decides what the heartbeat reports: with no flag (scope `security`, the
/// default) and with `--maintenance-linux-scope security`, the installed security updates and the
/// kernel meta-package are due, and neither `vim-tiny`, an update of no security origin, nor the
/// new kernel image and modules, not installed yet, are; with `--maintenance-linux-scope all` they
/// are due as well. In each, `openssl`, the package `placed.json` records for a service, and
/// `hostkeeper`, the daemon's own, are never due.
/// Catches: the flag parsed and not passed on to the source, or the source reading another
/// `placed.json` than the daemon's.
#[test]
fn the_scope_flag_decides_which_updates_the_heartbeat_reports() {
    let security = ["libc6", "libc-bin", "linux-generic"];
    assert_eq!(reported("scope-default", &[]), security);
    assert_eq!(
        reported("scope-security", &["--maintenance-linux-scope", "security"]),
        security
    );
    assert_eq!(
        reported("scope-all", &["--maintenance-linux-scope", "all"]),
        [
            "libc6",
            "libc-bin",
            "linux-image-6.8.0-45-generic",
            "linux-modules-6.8.0-45-generic",
            "linux-generic",
            "vim-tiny",
        ]
    );
}
