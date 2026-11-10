//! After the start pass (design section 6.5): the manifest's `[hook] boot` step of each service,
//! from `current`, after every update has reached its verdict; the readiness notice, exactly once,
//! whatever the start found; systemd's notifier; and the flag that asks for it.

mod support;
mod support_apt;
mod support_boot;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::boot::{BootStep, Outcome};
use hostkeeper_daemon::cli::{self, Cli, Command, EXIT_CONFIG, EXIT_FAILED, SystemdVars};
use hostkeeper_daemon::start::{self, StartError};
use hostkeeper_daemon::testing::{AdditiveFault, FakeNotifier, Log};
use hostkeeper_daemon::{NoNotifier, ReadyNotifier, Refusal, SystemdNotifier};
use hostkeeper_hook::Step;
use hostkeeper_install::repository::tree::hook_path;
use support::{OWN, alerting, d, lines, manifest_with, name};
use support_boot::Bench;

const RESTORE: &str = r#"boot = "restore""#;

/// `service` runs `version` from a sealed package whose manifest names the boot step `restore`.
fn restoring(rig: &Bench, service: &str, version: &str) {
    rig.add_with(service, version, &manifest_with(service, version, RESTORE));
    rig.store.link(service, "current", version);
}

fn logged(log: &Log) -> Vec<String> {
    log.lock().expect("the log").clone()
}

/// Catches a boot step run before an update has reached its verdict, from another release, or a
/// readiness notice sent before the boot steps: `beta`'s update cut off in `prepare` runs to its
/// verdict first, then `alpha`'s `restore` from `current` (its unit stopped first, in case an
/// earlier start left it), and the notice comes last. The step runs the hook of `current` with
/// the params of `current` and the manifest's `cutover` bound.
#[test]
fn the_boot_hook_runs_from_current_after_all_updates_and_before_ready() {
    let rig = Bench::new("steps-order");
    restoring(&rig, "alpha", "1.0.0");
    rig.add("beta", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("beta", Step::Prepare);
    let reply = daemon.order(&rig.update("beta", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed));
    drop(daemon);
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let notifier = FakeNotifier::new(log.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(
        logged(&log),
        [
            "stop updater-beta-prepare-1",
            "run updater-beta-prepare-1",
            "run updater-beta-cutover-1",
            "run updater-beta-verify-1",
            "run updater-beta-cleanup-1",
            "stop updater-alpha-boot-0",
            "additive updater-alpha-boot-0",
            "ready",
        ]
    );
    let report = started.boot_report();
    assert_eq!(report.services["alpha"].boot_step, BootStep::Succeeded);
    assert_eq!(report.services["beta"].boot_step, BootStep::NotRun);
    let ran = rig.launcher.additive_runs();
    let current = rig.packages.join("alpha/1.0.0");
    assert_eq!(ran.len(), 1);
    assert_eq!(
        (ran[0].service.as_str(), ran[0].step.as_str()),
        ("alpha", "restore")
    );
    assert_eq!(
        ran[0].executable,
        current.join("tree").join(hook_path("alpha"))
    );
    assert_eq!(ran[0].timeout_secs, 300);
    // A package's hook is called with no arguments: its program is the whole command.
    assert!(ran[0].hook_args.is_empty(), "{:?}", ran[0].hook_args);
    let params = hostkeeper_hook::Params::parse(&ran[0].params).expect("the params file");
    assert_eq!(params.staged_release(), Some(current.clone()));
    assert_eq!(params.target_version(), Some("1.0.0"));
    assert!(!rig.state.join("alpha/params.toml").exists());
}

/// Catches a boot step invented for a service that has none: a `current` whose manifest names
/// no `boot` step, and a service with no `current`, run nothing, and the notice goes out.
#[test]
fn a_service_without_a_boot_step_runs_no_boot_hook() {
    let rig = Bench::new("steps-none");
    rig.running("alpha", "1.0.0");
    rig.stage("beta", "1.0.0");
    let log = Log::default();
    let notifier = FakeNotifier::new(log.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(report.services["alpha"].boot_step, BootStep::NotRun);
    assert_eq!(report.services["beta"].boot_step, BootStep::NotRun);
    assert!(rig.launcher.additive_runs().is_empty());
    assert_eq!(logged(&log), ["ready"]);
}

/// Catches a failing boot step that stops the start, rolls something back, or goes unreported:
/// a step that exits 1, a `current` with no seal, a garbled seal, a seal of another tree, a hook
/// that is not there, a manifest that does not load, and a unit that cannot be stopped first each
/// fail that service's boot step alone; nothing is rolled back, and the notice goes out once.
#[test]
fn a_failing_boot_hook_is_logged_and_the_daemon_still_becomes_ready() {
    let rig = Bench::new("steps-failing");
    restoring(&rig, "exits", "1.0.0");
    rig.launcher.additive_exits("exits", &[1]);
    restoring(&rig, "unsealed", "1.0.0");
    fs::remove_file(rig.packages.join("unsealed/1.0.0.seal")).expect("remove the seal");
    restoring(&rig, "garbled", "1.0.0");
    fs::write(rig.packages.join("garbled/1.0.0.seal"), "{").expect("garble the seal");
    restoring(&rig, "changed", "1.0.0");
    fs::write(rig.packages.join("changed/1.0.0/extra"), "x").expect("change the tree");
    // A package whose tree has no hook at its fixed path (sealed as it is).
    restoring(&rig, "hookless", "1.0.0");
    fs::remove_file(
        rig.packages
            .join("hookless/1.0.0")
            .join("tree")
            .join(hook_path("hookless")),
    )
    .expect("remove the hook");
    rig.store.seal("hookless", "1.0.0", &d());
    // A package whose `target.json`, the entry it was staged for, does not load (sealed as it is).
    restoring(&rig, "unreadable", "1.0.0");
    let target_json = rig.packages.join("unreadable/1.0.0/target.json");
    fs::set_permissions(&target_json, fs::Permissions::from_mode(0o644)).expect("its mode");
    fs::write(&target_json, "{").expect("garble the entry");
    rig.store.seal("unreadable", "1.0.0", &d());
    restoring(&rig, "stuck", "1.0.0");
    rig.launcher.fail_stops("stuck");
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let report = started.boot_report();
    for service in [
        "exits",
        "unsealed",
        "garbled",
        "changed",
        "hookless",
        "unreadable",
        "stuck",
    ] {
        let step = &report.services[service].boot_step;
        assert!(matches!(step, BootStep::Failed(_)), "{service}: {step:?}");
    }
    assert_eq!(
        report.services["exits"].boot_step,
        BootStep::Failed("the boot step ended with code Some(1): ".to_owned())
    );
    let BootStep::Failed(why) = &report.services["changed"].boot_step else {
        unreachable!()
    };
    assert!(why.starts_with("artifact_changed: "), "{why}");
    let ran: Vec<String> = rig
        .launcher
        .additive_runs()
        .into_iter()
        .map(|ran| ran.service)
        .collect();
    assert_eq!(ran, ["exits"]);
    assert!(rig.launcher.runs().is_empty());
    assert_eq!(notifier.sent(), 1);
    assert_eq!(report.ready, Some(Ok(())));
}

/// Catches a boot step run beside an update that is not over: `alpha`'s update to 2.0.0 is cut
/// off in `prepare` again during the start; `alpha` is left unfinished and its `restore` does not
/// run, while the notice still goes out.
#[test]
fn a_service_with_an_unfinished_update_runs_no_boot_hook() {
    let rig = Bench::new("steps-unfinished");
    restoring(&rig, "alpha", "1.0.0");
    rig.add("alpha", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("alpha", Step::Prepare);
    let reply = daemon.order(&rig.update("alpha", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed));
    drop(daemon);
    rig.launcher.lose("alpha", Step::Prepare);
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let alpha = &started.boot_report().services["alpha"];
    assert!(
        matches!(alpha.outcome, Outcome::Interrupted { .. }),
        "{alpha:?}"
    );
    assert_eq!(alpha.boot_step, BootStep::Held);
    assert!(rig.launcher.additive_runs().is_empty());
    assert_eq!(notifier.sent(), 1);
}

/// Catches a notice sent twice, or not at all, when the start found trouble: with a held record
/// and a failing boot step, the notice is sent exactly once, after the boot steps; a notice that
/// fails is reported, and the daemon still starts.
#[test]
fn ready_is_sent_exactly_once() {
    let rig = Bench::new("steps-ready-once");
    restoring(&rig, "alpha", "1.0.0");
    rig.launcher.additive_exits("alpha", &[1]);
    fs::create_dir_all(rig.state.join("beta")).expect("beta's dir");
    fs::write(rig.state.join("beta/step-record.json"), "{").expect("a corrupt record");
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let notifier = FakeNotifier::new(log.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(notifier.sent(), 1);
    assert_eq!(logged(&log).last().map(String::as_str), Some("ready"));
    assert_eq!(
        logged(&log).iter().filter(|line| *line == "ready").count(),
        1
    );
    assert!(matches!(
        started.boot_report().services["beta"].outcome,
        Outcome::Corrupt(_)
    ));
    drop(started);

    let failing = FakeNotifier::failing();
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*failing)
        .expect("the daemon starts");
    assert_eq!(failing.sent(), 1);
    assert_eq!(
        started.boot_report().ready,
        Some(Err("the service manager is gone".to_owned()))
    );
}

/// Catches a notice that does not reach systemd, or reaches it as anything but `READY=1`: the
/// notifier sends exactly `READY=1` to a datagram socket at the path it is given; a path where no
/// socket listens is an error; the notifier of a daemon without the flag does nothing.
#[test]
fn the_systemd_notifier_writes_ready_to_the_given_socket() {
    let rig = Bench::new("steps-notify");
    let path = rig.scratch.path().join("n.sock");
    let socket = UnixDatagram::bind(&path).expect("bind the socket");
    SystemdNotifier::new(path.as_os_str())
        .ready()
        .expect("the notice");
    let mut buf = [0u8; 64];
    let n = socket.recv(&mut buf).expect("a datagram");
    assert_eq!(&buf[..n], b"READY=1");

    let nobody = SystemdNotifier::new(rig.scratch.path().join("nobody.sock").as_os_str());
    assert!(nobody.ready().is_err());
    assert!(NoNotifier.ready().is_ok());
}

/// Catches an abstract notify socket (`@name`, as systemd may set it on Linux) taken for a path:
/// the notice reaches the abstract socket of that name.
#[cfg(target_os = "linux")]
#[test]
fn the_systemd_notifier_writes_ready_to_an_abstract_socket() {
    let rig = Bench::new("steps-notify-abstract");
    let mut buf = [0u8; 64];
    let unique = rig
        .scratch
        .path()
        .file_name()
        .expect("the scratch dir's name");
    let name = format!("hostkeeper-test-{}", unique.to_string_lossy());
    let address = {
        use std::os::linux::net::SocketAddrExt;
        std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).expect("a name")
    };
    let socket = UnixDatagram::bind_addr(&address).expect("bind the abstract socket");
    SystemdNotifier::new(format!("@{name}"))
        .ready()
        .expect("the notice");
    let n = socket.recv(&mut buf).expect("a datagram");
    assert_eq!(&buf[..n], b"READY=1");
}

/// Catches `--notify-systemd` without a socket ignored, or a socket used without the flag: the
/// flag is off by default; with it and no notify socket the daemon exits 1 before it changes
/// anything; with a socket it starts as usual (here, to the config error of a missing config).
#[test]
fn the_notify_systemd_flag_needs_the_socket_systemd_sets() {
    let rig = Bench::new("steps-flag");
    let Command::Daemon(flags) = Cli::parse_from(["hostkeeper", "daemon"]).command else {
        panic!("`daemon` parses as the daemon verb");
    };
    assert!(!flags.notify_systemd);
    assert!(cli::notifier(&flags, Some(OsString::from("/x"))).is_ok());
    let missing = rig.scratch.path().join("missing.toml");
    let args = |notify: bool| {
        let mut args: Vec<OsString> = vec![
            "hostkeeper".into(),
            "daemon".into(),
            "--config".into(),
            missing.clone().into(),
            "--state-dir".into(),
            rig.state.clone().into(),
            "--packages-dir".into(),
            rig.packages.clone().into(),
        ];
        if notify {
            args.push("--notify-systemd".into());
        }
        Cli::parse_from(args)
    };
    let code = cli::run_with(args(true), SystemdVars::default());
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(EXIT_FAILED))
    );
    assert!(!rig.state.exists());
    let error = StartError::NoNotifySocket;
    assert_eq!(error.exit_code(), EXIT_FAILED);
    assert_eq!(
        error.to_string(),
        "--notify-systemd was given, but no notify socket was set for the daemon (NOTIFY_SOCKET)"
    );
    let socket = Some(OsString::from(rig.scratch.path().join("n.sock")));
    let code = cli::run_with(
        args(true),
        SystemdVars {
            notify_socket: socket,
            watchdog_usec: None,
        },
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(EXIT_CONFIG))
    );
    assert!(!rig.state.exists());
}

/// Catches a boot step unit whose name cannot be found again: it is
/// `updater-<service>-boot-<generation>`, the generation of the service's last update.
#[test]
fn the_boot_step_unit_carries_the_generation_of_the_last_update() {
    let rig = Bench::new("steps-generation");
    restoring(&rig, "alpha", "1.0.0");
    // The update switches `current` to 2.0.0, so the boot step is 2.0.0's.
    rig.publish("alpha", "2.0.0", &manifest_with("alpha", "2.0.0", RESTORE));
    let (daemon, mut report) = rig.booted();
    let reply = daemon.order(&rig.update("alpha", "2.0.0", 1));
    assert!(reply.refusal().is_none(), "{reply:?}");
    let notifier = FakeNotifier::new(Log::default());
    daemon.after_boot(&mut report, &*notifier);
    assert_eq!(report.services["alpha"].boot_step, BootStep::Succeeded);
    let ran = rig.launcher.additive_runs();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].unit, "updater-alpha-boot-1");
    assert_eq!(rig.launcher.stops(), ["updater-alpha-boot-1"]);
}

/// A daemon whose start pass ran on `rig`, its alerts in a log, and the report after its boot
/// steps and readiness notice.
fn after_boot(
    rig: &Bench,
) -> (
    hostkeeper_daemon::Daemon,
    hostkeeper_daemon::BootReport,
    Log,
) {
    fs::create_dir_all(&rig.state).expect("the state dir");
    let (daemon, alerts) = alerting(rig.daemon());
    let mut report = daemon.boot_pass().expect("the start pass");
    daemon.after_boot(&mut report, &*FakeNotifier::new(Log::default()));
    (daemon, report, alerts)
}

/// Catches a boot step whose launcher lost it being forgotten while its unit may still run, or a
/// failure that goes unlogged: a hook that cannot start fails the step with its params file gone;
/// a unit the launcher lost is stopped before its params file goes; one that cannot be stopped
/// keeps its params file and its service locked, so an order of it is `busy`; each failure is
/// one exact alert.
#[test]
fn a_boot_step_the_launcher_lost_is_stopped_or_its_service_held() {
    let rig = Bench::new("steps-lost");
    for service in ["lost", "nostart", "stuck"] {
        restoring(&rig, service, "1.0.0");
    }
    rig.launcher.additive_fault("lost", AdditiveFault::Lost);
    rig.launcher
        .additive_fault("nostart", AdditiveFault::NoStart);
    rig.launcher
        .additive_fault("stuck", AdditiveFault::LostUnstoppable);
    let (daemon, report, alerts) = after_boot(&rig);
    let failed = |service: &str| match &report.services[service].boot_step {
        BootStep::Failed(why) => why.clone(),
        other => panic!("{service}: {other:?}"),
    };
    assert_eq!(
        failed("lost"),
        "the launcher failed: updater-lost-boot-0 was lost; its unit updater-lost-boot-0 was stopped"
    );
    assert_eq!(failed("nostart"), "the hook did not start: no such file");
    let stuck = "the launcher failed: updater-stuck-boot-0 was lost; its unit updater-stuck-boot-0 could not be stopped (the launcher failed: updater-stuck-boot-0 could not be stopped), so stuck stays locked";
    assert_eq!(failed("stuck"), stuck);
    assert_eq!(
        rig.launcher.stops(),
        [
            "updater-lost-boot-0",
            "updater-lost-boot-0",
            "updater-nostart-boot-0",
            "updater-stuck-boot-0",
            "updater-stuck-boot-0",
        ]
    );
    assert!(!rig.state.join("lost/params.toml").exists());
    assert!(!rig.state.join("nostart/params.toml").exists());
    assert!(rig.state.join("stuck/params.toml").exists());
    assert!(
        daemon
            .ctx()
            .locks
            .holder(&name("stuck"))
            .expect("held")
            .stuck
    );
    let line = |alert: &str, service: &str, why: &str| {
        format!("hostkeeper daemon: level=error alert={alert} service={service}: {why}")
    };
    assert_eq!(
        lines(&alerts),
        [
            line(
                "boot_step_failed",
                "lost",
                "the launcher failed: updater-lost-boot-0 was lost; its unit updater-lost-boot-0 was stopped"
            ),
            line(
                "boot_step_failed",
                "nostart",
                "the hook did not start: no such file"
            ),
            line("unstoppable_unit", "stuck", stuck),
            line("boot_step_failed", "stuck", stuck),
        ]
    );
    rig.add("stuck", "2.0.0");
    let reply = daemon.order(&rig.update("stuck", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
}

/// Catches a boot step run without its params file, or one whose params file cannot be removed
/// taken for a success: params that cannot be written (a directory is in the way) run nothing;
/// params the step leaves unremovable fail the step after it ran; each is reported, the notice
/// goes out, and once the way is clear the next start runs the step.
#[test]
fn a_boot_step_whose_params_cannot_be_written_or_removed_fails() {
    let rig = Bench::new("steps-params");
    restoring(&rig, "alpha", "1.0.0");
    let params = rig.state.join("alpha/params.toml");
    fs::create_dir_all(params.join("inside")).expect("a directory in the way");
    let (_, report, _) = after_boot(&rig);
    assert!(matches!(
        report.services["alpha"].boot_step,
        BootStep::Failed(_)
    ));
    assert!(rig.launcher.additive_runs().is_empty());
    assert_eq!(report.ready, Some(Ok(())));
    fs::remove_dir_all(&params).expect("clear the way");

    let in_the_way = params.clone();
    rig.launcher.during_additive(move |_| {
        fs::remove_file(&in_the_way).expect("the params file");
        fs::create_dir_all(in_the_way.join("inside")).expect("a directory in its place");
    });
    let (_, report, _) = after_boot(&rig);
    assert!(matches!(
        report.services["alpha"].boot_step,
        BootStep::Failed(_)
    ));
    assert_eq!(rig.launcher.additive_runs().len(), 1);

    fs::remove_dir_all(&params).expect("clear the way");
    rig.launcher.during_additive(|_| {});
    let (_, report, _) = after_boot(&rig);
    assert_eq!(report.services["alpha"].boot_step, BootStep::Succeeded);
    assert_eq!(rig.launcher.additive_runs().len(), 2);
}

/// Catches a `current` that leads the boot step out of the service's versions: a `current` that
/// is a directory, that links out of the package directory, that is absolute, or whose name is not
/// a version fails the boot step and runs nothing; one that links to a version that is not there
/// fails it too.
#[test]
fn a_current_that_is_not_a_version_of_the_service_runs_no_boot_step() {
    let rig = Bench::new("steps-current");
    restoring(&rig, "other", "1.0.0");
    let elsewhere = rig.packages.join("other/1.0.0");
    let cases: [(&str, Option<std::path::PathBuf>); 5] = [
        ("dir", None),
        ("outside", Some("../other/1.0.0".into())),
        ("absolute", Some(elsewhere)),
        ("dotted", Some(".hidden".into())),
        ("dangling", Some("9.9.9".into())),
    ];
    for (service, target) in &cases {
        restoring(&rig, service, "1.0.0");
        let current = rig.packages.join(service).join("current");
        fs::remove_file(&current).expect("remove current");
        match target {
            Some(target) => std::os::unix::fs::symlink(target, &current).expect("a link"),
            None => fs::create_dir(&current).expect("a directory"),
        }
    }
    rig.store.link("other", "current", "missing");
    let (_, report, _) = after_boot(&rig);
    for (service, _) in &cases {
        let step = &report.services[*service].boot_step;
        assert!(matches!(step, BootStep::Failed(_)), "{service}: {step:?}");
    }
    let BootStep::Failed(why) = &report.services["outside"].boot_step else {
        unreachable!()
    };
    assert_eq!(
        why,
        "`current` links to ../other/1.0.0, not to a version directory beside it"
    );
    assert!(rig.launcher.additive_runs().is_empty());
}

/// Catches a readiness notice that hangs the start on a full queue: the notify socket's queue is
/// filled, and the notice fails at once with an error the daemon reports.
#[test]
fn a_full_notify_queue_fails_the_notice_without_waiting() {
    let rig = Bench::new("steps-full-queue");
    let path = rig.scratch.path().join("q.sock");
    let _receiver = UnixDatagram::bind(&path).expect("bind the socket");
    // Each notice is a socket of its own, so only the receiver's queue stops the fill.
    let notifier = SystemdNotifier::new(path.as_os_str());
    while notifier.ready().is_ok() {}
    let error = notifier.ready().expect_err("a full queue");
    // A full datagram queue is EAGAIN (`WouldBlock`) on Linux and ENOBUFS on macOS, which std
    // maps to no kind of its own.
    let full = if cfg!(target_os = "macos") {
        error.raw_os_error() == Some(libc::ENOBUFS)
    } else {
        error.kind() == std::io::ErrorKind::WouldBlock
    };
    assert!(full, "{error:?}");
}
