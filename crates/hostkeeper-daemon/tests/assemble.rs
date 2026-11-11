//! The daemon put together: the order of the start (the registration socket, the sweep of package
//! leftovers, the boot pass, the boot step and the readiness notice), the alerts of the parts of
//! it that do not stop the start, and the host build's ports and files, built from paths alone.

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
use hostkeeper_daemon::boot::{ALERT_START, BootReport, BootStep, Stage, alert_line};
use hostkeeper_daemon::cli::{
    ConsoleFiles, DEADMAN_POLL_SECS, GH_BINARY, HostFiles, PKI_DIR, SIGSTORE_TRUSTED_ROOT,
    host_ports, start_with,
};
use hostkeeper_daemon::console::{AGETTY, ENROLLMENT_ISSUE};
use hostkeeper_daemon::start::{
    self, EXIT_CONFIG, EXIT_FAILED, Settings, StartError, before_records,
};
use hostkeeper_daemon::testing::{FakeNotifier, Log};
use hostkeeper_daemon::{NoNotifier, Ports, ReadyNotifier};
use hostkeeper_hook::{Invocation, Step};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_observe::{CommandOutput, RunError};
use hostkeeper_placed::testing::{Event, Log as PlacedLog};
use hostkeeper_steps::{Bound, StepCall};
use support::{OWN, Rig, Tap, alerting, config, d, lines, manifest_with};

const RESTORE: &str = r#"boot = "restore""#;

/// The adapters of a Linux host, the unit control's `systemctl` run through `systemctl` (which
/// answers what it is told and records each command).
fn linux_over(systemctl: &FakeCommandRunner) -> Adapters {
    Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(systemctl.clone(), SYSTEMCTL_DEADLINE),
    )
}

/// The adapters of a Linux host whose `systemctl` is never asked.
fn linux() -> Adapters {
    linux_over(&FakeCommandRunner::default())
}

/// A readiness notifier that puts a mark in the placed log, in its place among the writes and
/// calls of the service manager, and then tells `inner`.
struct MarkReady {
    inner: Arc<FakeNotifier>,
    log: PlacedLog,
}

impl ReadyNotifier for MarkReady {
    fn ready(&self) -> io::Result<()> {
        self.log.push(Event::Mark("ready".to_owned()));
        self.inner.ready()
    }
}

fn make_dir(dir: &Path, mode: u32) {
    fs::create_dir_all(dir).expect("make the dir");
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("set its mode");
}

/// `web` at 1.0.0 names the boot step `restore`, and runs from `current`.
fn restoring_web(rig: &Rig) {
    rig.store
        .add("web", "1.0.0", &manifest_with("web", "1.0.0", RESTORE));
    rig.store.seal("web", "1.0.0", &d());
    rig.store.link("web", "current", "1.0.0");
}

/// Catches a start that does its parts out of order, or tells systemd too early: a registration
/// socket kbf's agent left is removed first; the boot pass reads and writes its records, and the
/// readiness notice comes after the boot step, with the stages in that order.
#[test]
fn the_daemon_reaches_ready_only_after_the_boot_pass() {
    let rig = Rig::new("assemble-order");
    restoring_web(&rig);
    let run_dir = rig.settings().kbf_run_dir();
    assert_eq!(run_dir, rig.run_dir.join("updater"));
    fs::create_dir_all(&run_dir).expect("the run dir");
    let stale = run_dir.join("register.sock");
    fs::write(&stale, "").expect("a stale registration socket");
    rig.placed_log.clear();

    let ports = Ports {
        launcher: Tap::new(rig.launcher.clone(), rig.placed_log.clone()),
        ..rig.ports()
    };
    let notifier = MarkReady {
        inner: FakeNotifier::new(Log::default()),
        log: rig.placed_log.clone(),
    };
    let started = start::startup_notifying(&rig.settings(), ports, OWN, &notifier)
        .expect("the daemon starts");

    assert!(!stale.exists(), "the stale registration socket is removed");
    let report = started.boot_report();
    assert_eq!(
        report.stages,
        [
            Stage::UnlinkRegistrationSocket,
            Stage::ClearPolicyRcD,
            Stage::BeforeRecords,
            Stage::ReadRecords,
            Stage::WriteBootId,
            Stage::ResumeUninstalls,
            Stage::RunUpdates,
            Stage::RestartWatchers,
            Stage::StartRunUnits,
            Stage::BootSteps,
            Stage::Ready,
        ]
    );
    assert_eq!(report.services["web"].boot_step, BootStep::Succeeded);
    assert!(report.archive_versions.is_empty());

    let events = rig.placed_log.events();
    let at = |wanted: &Event| {
        events
            .iter()
            .position(|event| event == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is not in {events:?}"))
    };
    let boot = at(&Event::Mark("additive restore".to_owned()));
    let ready = at(&Event::Mark("ready".to_owned()));
    assert!(boot < ready, "{events:?}");
    assert_eq!(notifier.inner.sent(), 1);
}

/// Catches a crashed stage's leftovers surviving a start: a `.stage-*` directory and a seal with
/// no version beside it, under a service directory, are removed by the start, and the version
/// that is there, its seal and `current` are left alone.
#[test]
fn orphans_are_removed_at_start() {
    let rig = Rig::new("assemble-sweep");
    rig.add("web", "2.0.0");
    rig.store.seal("web", "2.0.0", &d());
    let service = rig.packages.join("web");
    make_dir(&service.join(".stage-x"), 0o700);
    fs::write(service.join("9.9.9.seal"), "{}").expect("a seal with no version");
    fs::write(rig.packages.join("not-a-dir"), "x").expect("a file among the services");
    fs::write(service.join(".fetch-123-0"), "half a download").expect("an earlier fetch's file");
    start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    assert!(!service.join(".fetch-123-0").exists());
    assert!(!service.join(".stage-x").exists());
    assert!(!service.join("9.9.9.seal").exists());
    assert!(service.join("2.0.0").is_dir() && service.join("2.0.0.seal").is_file());
    assert!(fs::symlink_metadata(service.join("current")).is_ok());
}

/// Catches a `.fetch-*` file an earlier release left that cannot be removed stopping the start or
/// passing unsaid: the staging sweep has nothing to do there, the fetch sweep fails, and that is
/// one alert naming the service.
#[test]
fn a_fetch_file_that_cannot_be_removed_is_an_alert() {
    let rig = Rig::new("assemble-fetch-alert");
    rig.add("web", "2.0.0");
    let service = rig.packages.join("web");
    fs::write(service.join(".fetch-123-0"), "half a download").expect("an earlier fetch's file");
    fs::set_permissions(&service, fs::Permissions::from_mode(0o555)).expect("read-only");
    let (daemon, log) = alerting(rig.daemon());
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    fs::set_permissions(&service, fs::Permissions::from_mode(0o755)).expect("open again");
    let seen = lines(&log);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&alert_line(ALERT_START, "service=web: ")),
        "{seen:?}"
    );
    assert!(service.join(".fetch-123-0").is_file());
    assert_eq!(report.stages, [Stage::BeforeRecords]);
}

/// Catches a sweep that fails silently, or a packages directory that cannot be listed or opened
/// stopping the start: each is one alert and the start goes on. A service directory that cannot
/// be swept is named, a packages directory that cannot be listed or opened is not blamed on a
/// service.
#[test]
fn a_sweep_that_fails_or_a_packages_directory_that_cannot_be_used_is_an_alert() {
    let rig = Rig::new("assemble-sweep-alert");
    rig.add("web", "2.0.0");
    let service = rig.packages.join("web");
    fs::set_permissions(&service, fs::Permissions::from_mode(0o000)).expect("closed");
    let (daemon, log) = alerting(rig.daemon());
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    fs::set_permissions(&service, fs::Permissions::from_mode(0o755)).expect("open again");
    let seen = lines(&log);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&alert_line(ALERT_START, "service=web: ")),
        "{seen:?}"
    );
    assert_eq!(report.stages, [Stage::BeforeRecords]);

    let rig = Rig::new("assemble-list-alert");
    fs::write(&rig.packages, "x").expect("a file where the packages go");
    let why = fs::read_dir(&rig.packages)
        .expect_err("not a directory")
        .to_string();
    let (daemon, log) = alerting(rig.daemon());
    before_records(&daemon, &mut BootReport::default());
    assert_eq!(
        lines(&log)[0],
        alert_line(
            ALERT_START,
            &format!("the package directory cannot be listed, so no leftover is removed: {why}")
        )
    );

    // A packages directory that is a link lists, and is not opened: the sweep says so once.
    let rig = Rig::new("assemble-open-alert");
    let real = rig.scratch.path().join("real-packages");
    make_dir(&real.join("web"), 0o755);
    std::os::unix::fs::symlink(&real, &rig.packages).expect("a link where the packages go");
    let (daemon, log) = alerting(rig.daemon());
    before_records(&daemon, &mut BootReport::default());
    let seen = lines(&log);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&alert_line(
            ALERT_START,
            "the package directory cannot be opened, so no leftover is removed: "
        )),
        "{seen:?}"
    );
}

/// Catches a start that goes on past something it could not make safe: a path where the
/// registration socket is that cannot be removed stops the start naming the path, with nothing
/// made in the run directory.
#[test]
fn a_registration_socket_that_cannot_be_removed_stops_the_start() {
    let rig = Rig::new("assemble-unlink");
    let stale = rig.settings().kbf_run_dir().join("register.sock");
    fs::create_dir_all(stale.join("inner")).expect("a directory where the socket was");
    let error = start::startup(&rig.settings(), rig.ports(), OWN)
        .err()
        .expect("refused");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == stale),
        "{error}"
    );
    assert_eq!(error.exit_code(), EXIT_FAILED);
    let made: Vec<_> = fs::read_dir(&rig.run_dir)
        .expect("the run directory the test made")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(
        made,
        ["updater"],
        "nothing else is made in the run directory"
    );
}

/// Catches the run directory of kbf's agent taken from the wrong directory: `updater` under the
/// settings' run directory, whatever shape the run directory has.
#[test]
fn the_registration_socket_directory_is_updater_under_the_run_directory() {
    let rig = Rig::new("assemble-run-dir");
    let rows = [
        ("/run", "/run/updater"),
        ("/var/run", "/var/run/updater"),
        ("/a/b/c", "/a/b/c/updater"),
        ("/", "/updater"),
        ("run", "run/updater"),
    ];
    for (run_dir, expected) in rows {
        let settings = Settings {
            run_dir: PathBuf::from(run_dir),
            ..rig.settings()
        };
        assert_eq!(settings.kbf_run_dir(), Path::new(expected), "{run_dir}");
    }
}

/// Catches ports of the host's build that read the wrong place, or fall back to the unwired ones:
/// the dead-man poll and the host's directories for placement. Catches a launcher or a unit
/// control left unwired: the ports' launcher is the host's systemd launcher, which refuses a hook
/// that is not there by its path before it spawns anything, and their unit control runs
/// `systemctl` through the adapters' launcher. Catches a package store that fetches by a path of
/// its own: a stores file that is not sound stops the ports as `Stores` naming it, and one that
/// is not there gives the ports no store.
#[test]
fn the_host_ports_are_built_from_flags_and_files_without_the_network() {
    let rig = Rig::new("assemble-host-ports");
    let files = HostFiles {
        repositories: None,
        os_root: rig.scratch.path().join("os"),
        stores: rig.scratch.path().join("stores.toml"),
        macos_pkg_signers: None,
        pki: rig.scratch.path().join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    };
    make_dir(&rig.packages, 0o755);
    let systemctl = FakeCommandRunner::default();
    let ports = host_ports(
        &config(),
        &rig.settings(),
        &files,
        OWN,
        linux_over(&systemctl),
    )
    .expect("the ports");
    assert_eq!(ports.deadman_poll_secs, Some(DEADMAN_POLL_SECS));
    assert_eq!(ports.placement.roots.packages().dir(), rig.packages);

    // The launcher is the host's: a hook that is not there does not start, named by its path
    // (the unwired launcher would answer that it has none), and nothing is spawned for it.
    let missing = rig.scratch.path().join("no-such-hook");
    let unit = "updater-web-prepare-1";
    let call = StepCall {
        service: "web",
        hook_args: &[],
        invocation: Invocation {
            step: Step::Prepare,
            artifact: Some(rig.packages.join("web")),
            params: Some(rig.scratch.path().join("params.toml")),
        },
        executable: &missing,
        unit,
        bound: Bound::Unbounded,
    };
    let refused = ports
        .launcher
        .run(&call)
        .err()
        .map(|error| error.to_string());
    assert_eq!(
        refused,
        Some(format!(
            "the hook did not start: {}: No such file or directory (os error 2)",
            missing.display()
        ))
    );

    // The unit control is the adapters' `systemctl`, not the unwired one that refused.
    systemctl.then(Ok(CommandOutput {
        exit_code: Some(0),
        stdout: Vec::new(),
        truncated: false,
    }));
    assert_eq!(ports.units.daemon_reload(), Ok(()));
    assert_eq!(
        systemctl.calls(),
        vec![(
            vec!["systemctl".to_owned(), "daemon-reload".to_owned()],
            SYSTEMCTL_DEADLINE
        )]
    );
    // Nothing more answered: the next command cannot run, and that is the call's error.
    assert_eq!(
        ports
            .units
            .stop_unit("web.service")
            .map_err(|e| e.to_string()),
        Err(format!(
            "systemctl stop -- web.service failed: {}",
            RunError::Empty
        ))
    );

    // A stores file that is not sound stops the ports, naming the file.
    fs::write(&files.stores, "not a stores file").expect("write the stores file");
    fs::set_permissions(&files.stores, fs::Permissions::from_mode(0o644)).expect("its mode");
    let error = host_ports(&config(), &rig.settings(), &files, OWN, linux())
        .err()
        .expect("a stores file that is not sound");
    assert!(
        matches!(&error, StartError::Stores { path, .. } if *path == files.stores),
        "{error}"
    );
    assert_eq!(error.exit_code(), EXIT_CONFIG);
}

/// Catches a start of the host's build that goes on past ports it could not build: a config that
/// does not load stops the start as a `Config` error, before any port is built; and a build for
/// another system than macOS has no macOS adapters.
#[test]
fn the_start_of_the_host_build_stops_at_the_ports_it_cannot_build() {
    let rig = Rig::new("assemble-start-with");
    let files = HostFiles {
        repositories: None,
        os_root: rig.scratch.path().join("os"),
        stores: rig.scratch.path().join("stores.toml"),
        macos_pkg_signers: None,
        pki: rig.scratch.path().join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    };
    let notifier = FakeNotifier::new(Log::default());
    let settings = Settings {
        config: rig.scratch.path().join("no-such.toml"),
        ..rig.settings()
    };
    let error = start_with(&settings, &files, OWN, &*notifier, &Platform::linux())
        .err()
        .expect("no config");
    assert!(matches!(error, StartError::Config { .. }), "{error}");

    // Only a build for another system than macOS has no macOS adapters; a Mac's build has them,
    // and a start with them would reach for the Mac's real launchd directories.
    #[cfg(not(target_os = "macos"))]
    {
        let error = start_with(&rig.settings(), &files, OWN, &*notifier, &Platform::macos())
            .err()
            .expect("no macOS adapters in a Linux build");
        assert!(
            matches!(error, StartError::NoAdapters { platform: "macOS" }),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "this build of hostkeeper has no adapters for macOS"
        );
        assert_eq!(error.exit_code(), EXIT_FAILED);
    }
    assert_eq!(notifier.sent(), 0);
}

/// Catches a Mac given the Linux files: no console's issue file is written, the start reads the
/// managed preferences the pkg's postinstall reads, no repository directory is read, and the
/// signers of `macos-pkg` are read from `/etc/hostkeeper/macos-pkg.toml`.
#[test]
fn a_mac_has_its_managed_preferences_and_signers_and_no_console_or_repositories() {
    let mac = HostFiles {
        repositories: None,
        os_root: PathBuf::from("/"),
        stores: PathBuf::from("/etc/hostkeeper/stores.toml"),
        macos_pkg_signers: Some(PathBuf::from("/etc/hostkeeper/macos-pkg.toml")),
        pki: PathBuf::from("/etc/updater/pki"),
        console: None,
        managed_config: Some(PathBuf::from(
            "/Library/Managed Preferences/dev.hostkeeper.config.plist",
        )),
        managed_stores: Some(PathBuf::from(
            "/Library/Managed Preferences/dev.hostkeeper.stores.plist",
        )),
        managed_macos_pkg: Some(PathBuf::from(
            "/Library/Managed Preferences/dev.hostkeeper.macos-pkg.plist",
        )),
    };
    assert_eq!(HostFiles::for_platform(PlatformKind::Macos), mac);
}

/// Catches a Linux host given another host's files: the console's issue file is the daemon's
/// runtime directory's `enrollment.issue`, redrawn by `/sbin/agetty`, as on every systemd host;
/// an OpenRC host has the same files and the same console files (its start leaves the console
/// out through its adapters); the repository entries are read from their directory.
#[test]
fn a_linux_host_has_its_console_and_repositories() {
    let linux = HostFiles {
        repositories: Some(PathBuf::from("/etc/hostkeeper/repositories")),
        os_root: PathBuf::from("/"),
        stores: PathBuf::from("/etc/hostkeeper/stores.toml"),
        macos_pkg_signers: None,
        pki: PathBuf::from("/etc/updater/pki"),
        console: Some(ConsoleFiles {
            issue: PathBuf::from("/run/hostkeeper/enrollment.issue"),
            agetty: PathBuf::from("/sbin/agetty"),
        }),
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    };
    assert_eq!(HostFiles::for_platform(PlatformKind::Linux), linux);
    assert_eq!(
        HostFiles::for_platform(PlatformKind::Openrc),
        HostFiles {
            console: None,
            ..linux
        }
    );
    assert_eq!(ENROLLMENT_ISSUE, "/run/hostkeeper/enrollment.issue");
    assert_eq!(AGETTY, "/sbin/agetty");
    // The packaged files hostkeeper's own package installs for `hostkeeper-pxe`.
    assert_eq!(GH_BINARY, "/usr/lib/hostkeeper/gh");
    assert_eq!(
        SIGSTORE_TRUSTED_ROOT,
        "/usr/lib/hostkeeper/trusted_root.json"
    );
}

/// Catches the files of this host chosen for another platform than the binary's: they are the
/// files of this build's platform, and the pki directory is unchanged.
#[test]
fn this_host_has_the_files_of_its_own_platform() {
    assert_eq!(
        HostFiles::host(),
        HostFiles::for_platform(Platform::host().kind)
    );
    assert_eq!(PKI_DIR, "/etc/updater/pki");
}

/// Catches a daemon config that no longer starts the daemon for keys it reads no more: the config
/// of a host set up while a release that requires them may run keeps `release_channel` and
/// `[trusted_signers]`; the loader accepts them and the daemon starts, reading neither.
#[test]
fn a_config_with_a_release_channel_and_trusted_signers_still_starts_the_daemon() {
    let rig = Rig::new("assemble-old-config");
    let text = fs::read_to_string(&rig.config).expect("the config");
    assert!(text.contains("release_channel = "), "{text}");
    assert!(text.contains("[trusted_signers]"), "{text}");
    let config = start::load_config(&rig.settings()).expect("the loader accepts both keys");
    assert_eq!(
        config.release_channel.as_ref().map(|u| u.as_str()),
        Some("https://releases.example")
    );
    assert_eq!(config.trusted_signers.organisation, "komira-ai");
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    assert_eq!(started.boot_report().ready, Some(Ok(())));
}

/// Catches a start with no notifier changing: `NoNotifier` is not a notifier that fails.
#[test]
fn a_start_with_no_notifier_is_ready() {
    let rig = Rig::new("assemble-no-notifier");
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("started");
    assert_eq!(started.boot_report().ready, Some(Ok(())));
    assert!(NoNotifier.ready().is_ok());
}
