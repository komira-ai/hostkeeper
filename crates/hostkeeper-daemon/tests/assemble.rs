//! The daemon put together: the order of the start (the registration socket, the sweep of package
//! leftovers, the reconciliation of the placed paths, the boot pass, the boot step and the
//! readiness notice), the alerts of the parts of it that do not stop the start, and the real
//! ports of the host's build, built from paths alone.

mod support;
mod support_archive;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::boot::{ALERT_START, BootReport, BootStep, Stage, alert_line};
use hostkeeper_daemon::cli::{
    CA_BUNDLE, DEADMAN_POLL_SECS, GH_BINARY, GH_HOME, HostFiles, HostVerifier, PKI_DIR,
    SIGSTORE_TRUSTED_ROOT, host_ports, start_with, tls_roots,
};
use hostkeeper_daemon::packages::{PackageError, Packages, VersionDir};
use hostkeeper_daemon::start::{self, EXIT_FAILED, Settings, StartError, before_records};
use hostkeeper_daemon::testing::{FakeNotifier, FakePackages, Log};
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_daemon::{NoNotifier, Ports, ReadyNotifier, Refusal};
use hostkeeper_hook::{Invocation, Step};
use hostkeeper_package::ports::{Attest, CommitId, SourceRef, Verifier, VerifyError};
use hostkeeper_placed::testing::{Event, FsWrite, Log as PlacedLog, UnitCall};
use hostkeeper_steps::{Bound, Stamp, StepCall};
use support::{OWN, Rig, Tap, alerting, config, d, lines, name, update};
use support_archive::{Answer, Rig as ArchiveRig, SERVICE, VERSION};

const RESTORE: &str = r#"boot = "restore""#;

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

/// A package store whose sweep always fails.
struct SweepFails(Arc<FakePackages>);

impl Packages for SweepFails {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &PackageDigest,
    ) -> Result<VersionDir, PackageError> {
        self.0.version_dir(service, version, digest)
    }

    fn remove_orphans(&self, _service: &ServiceName) -> Result<(), PackageError> {
        Err(PackageError::Other("the disk".to_owned()))
    }
}

fn make_dir(dir: &Path, mode: u32) {
    fs::create_dir_all(dir).expect("make the dir");
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("set its mode");
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("it exists").permissions().mode() & 0o777
}

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// `web` at 1.0.0 places a unit, its `WantedBy=` link and a file, names the boot step `restore`,
/// and runs from `current`.
fn restoring_web(rig: &Rig) -> PathBuf {
    let dir = rig.placed("web", "1.0.0", None);
    let text = fs::read_to_string(dir.join("manifest.toml")).expect("the manifest");
    let text = text.replace("[hook]\n", &format!("[hook]\n{RESTORE}\n"));
    fs::write(dir.join("manifest.toml"), text).expect("write the manifest");
    rig.store.seal("web", "1.0.0", &d());
    rig.store.link("web", "current", "1.0.0");
    dir
}

/// Catches a start that does its parts out of order, or tells systemd too early: a register
/// socket kbf's agent left is removed first; the placed path a crash left `pending` is linked
/// and `daemon-reload` runs (the reconciliation) before the `boot` step runs, and the readiness
/// notice comes after the boot step, with the stages in that order.
#[test]
fn the_daemon_reaches_ready_only_after_the_boot_pass_and_reconcile() {
    let rig = Rig::new("assemble-order");
    let dir = restoring_web(&rig);
    let run_dir = rig.settings().kbf_run_dir();
    assert_eq!(run_dir, rig.scratch.path().join("updater"));
    fs::create_dir_all(&run_dir).expect("the run dir");
    let stale = run_dir.join("register.sock");
    fs::write(&stale, "").expect("a stale registration socket");

    // The paths of `web` are recorded `pending` and not linked: the daemon died after `begin`.
    let manifest = Manifest::load(
        &dir,
        &Expected {
            name: name("web"),
            version: version("1.0.0"),
        },
        &OWN,
    )
    .expect("the manifest loads");
    {
        let daemon = rig.daemon();
        let placer = daemon.ctx().placer();
        let claims = placer.desired(&manifest).expect("the claims");
        placer.begin(&claims).expect("begin");
    }
    assert!(rig.placed_paths("web").iter().all(|path| !rig.there(path)));
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
            Stage::BeforeRecords,
            Stage::Reconcile,
            Stage::ReadRecords,
            Stage::WriteBootId,
            Stage::ResumeUninstalls,
            Stage::RunUpdates,
            Stage::RestartWatchers,
            Stage::BootSteps,
            Stage::Ready,
        ]
    );
    let reconciled = report
        .reconciled
        .clone()
        .expect("the reconciliation ran")
        .expect("and went through");
    assert_eq!(reconciled.linked.len(), 3, "{reconciled:?}");
    assert!(reconciled.reloaded);
    assert!(rig.placed_paths("web").iter().all(|path| rig.there(path)));
    assert_eq!(report.services["web"].boot_step, BootStep::Succeeded);

    let events = rig.placed_log.events();
    let at = |wanted: &Event| {
        events
            .iter()
            .position(|event| event == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is not in {events:?}"))
    };
    let link = events
        .iter()
        .position(|event| matches!(event, Event::Fs(FsWrite::Symlink { .. })))
        .expect("a link was written");
    let reload = at(&Event::Unit(UnitCall::DaemonReload));
    let boot = at(&Event::Mark("additive restore".to_owned()));
    let ready = at(&Event::Mark("ready".to_owned()));
    assert!(link < reload, "{events:?}");
    assert!(reload < boot, "{events:?}");
    assert!(boot < ready, "{events:?}");
    assert_eq!(notifier.inner.sent(), 1);
}

/// Catches a start that leaves a crashed fetch's leftovers, or an update that fetches over them:
/// the start asks for the sweep of every service directory of the packages directory; and, on
/// the real store over real directories, the start removes a `.stage-*` directory and a
/// `.fetch-*` file, and an update removes them again before its fetch (which then fails
/// `fetch_failed` for a channel with nothing in it), leaving `current` alone.
#[test]
fn orphans_are_removed_at_start_and_before_each_fetch() {
    let rig = Rig::new("assemble-sweep-fake");
    rig.add("web", "2.0.0");
    rig.add("api", "1.0.0");
    fs::write(rig.packages.join("not-a-dir"), "x").expect("a file among the services");
    start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    assert_eq!(rig.store.swept(), ["api", "web"]);

    let archive = ArchiveRig::new("assemble-sweep-real");
    let service = archive.packages.join(SERVICE);
    make_dir(&service, 0o755);
    std::os::unix::fs::symlink("1.0.0", service.join("current")).expect("current");
    let leftovers = |when: &str| {
        make_dir(&service.join(".stage-x"), 0o700);
        fs::write(service.join(".fetch-1-1"), "x").expect("a fetch leftover");
        assert!(service.join(".stage-x").exists(), "{when}");
    };
    leftovers("before the start");
    let store = archive.open(Vec::new(), Answer::Confirms).expect("open");
    let rig = Rig::new("assemble-sweep-daemon");
    let ports = Ports {
        packages: Arc::new(store),
        ..rig.ports()
    };
    let settings = Settings {
        packages_dir: archive.packages.clone(),
        ..rig.settings()
    };
    let started = start::startup(&settings, ports, OWN).expect("the daemon starts");
    assert!(!service.join(".stage-x").exists(), "removed at the start");
    assert!(!service.join(".fetch-1-1").exists(), "removed at the start");

    leftovers("before the update");
    let reply = started.daemon().order(&update(SERVICE, VERSION, &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::FetchFailed), "{reply:?}");
    assert!(
        !service.join(".stage-x").exists(),
        "removed before the fetch"
    );
    assert!(
        !service.join(".fetch-1-1").exists(),
        "removed before the fetch"
    );
    assert!(fs::symlink_metadata(service.join("current")).is_ok());
}

/// Catches a start that goes on past something it could not make safe: a path where the
/// registration socket is that cannot be removed stops the start naming the path, with the admin
/// socket never bound.
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
    assert!(!rig.socket.exists());
}

/// Catches a sweep that fails silently, or a packages directory that cannot be listed stopping
/// the start: each is one alert and the start goes on.
#[test]
fn a_sweep_that_fails_or_a_packages_directory_that_cannot_be_listed_is_an_alert() {
    let rig = Rig::new("assemble-sweep-alert");
    rig.add("web", "2.0.0");
    let ports = Ports {
        packages: Arc::new(SweepFails(rig.store.clone())),
        ..rig.ports()
    };
    let daemon = hostkeeper_daemon::Daemon::new(config(), rig.dirs(), ports, OWN);
    let (daemon, log) = alerting(daemon);
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    assert_eq!(
        lines(&log),
        [alert_line(
            ALERT_START,
            "service=web: the package could not be had: the disk"
        )]
    );
    assert_eq!(report.stages, [Stage::BeforeRecords, Stage::Reconcile]);
    assert!(matches!(report.reconciled, Some(Ok(_))), "{report:?}");

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
}

/// Catches a reconciliation that cannot run being swallowed or stopping the start: a placed lock
/// that is held, and a state directory that cannot be listed, each leave the reason in the
/// report and one alert, and the stages still run.
#[test]
fn a_reconciliation_that_cannot_run_is_in_the_report_and_an_alert() {
    let rig = Rig::new("assemble-reconcile-alert");
    let (daemon, log) = alerting(rig.daemon());
    let held = daemon
        .ctx()
        .placer()
        .lock()
        .expect("the lock of placed.json");
    let why = daemon
        .ctx()
        .placer()
        .lock()
        .expect_err("it is held")
        .to_string();
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    drop(held);
    assert_eq!(report.reconciled, Some(Err(why.clone())));
    assert_eq!(
        lines(&log),
        [alert_line(
            ALERT_START,
            &format!("the placed paths could not be reconciled: {why}")
        )]
    );
    assert_eq!(report.stages, [Stage::BeforeRecords, Stage::Reconcile]);

    let rig = Rig::new("assemble-state-alert");
    fs::write(&rig.state, "x").expect("a file where the state goes");
    let (daemon, log) = alerting(rig.daemon());
    let why = daemon
        .switch_intents(false)
        .expect_err("not a directory")
        .to_string();
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    assert_eq!(report.reconciled, Some(Err(why.clone())));
    assert_eq!(
        lines(&log),
        [alert_line(
            ALERT_START,
            &format!("the placed paths could not be reconciled: {why}")
        )]
    );
}

/// Catches the run directory of kbf's agent taken from the wrong directory: `updater` beside the
/// admin socket's directory, and `/updater` for a socket with no directory above it.
#[test]
fn the_registration_socket_directory_is_beside_the_admin_socket_directory() {
    let rig = Rig::new("assemble-run-dir");
    let rows = [
        ("/run/hostkeeper/admin.sock", "/run/updater"),
        ("/a/b/c/s.sock", "/a/b/updater"),
        ("/s.sock", "/updater"),
        ("s.sock", "/updater"),
    ];
    for (socket, expected) in rows {
        let settings = Settings {
            admin_socket: PathBuf::from(socket),
            ..rig.settings()
        };
        assert_eq!(settings.kbf_run_dir(), Path::new(expected), "{socket}");
    }
}

/// Catches ports of the host's build that read the wrong place, or fall back to the unwired ones:
/// with the packages directory there the ports open with the sweep of the real store (a service
/// entry that is a file is an `io_error`), the dead-man poll and the host's directories for
/// placement; without it the start is `Io` naming it; `gh`'s home under the state directory is
/// made mode 0700, and one that cannot be made is `Io` naming it. Catches a launcher left
/// unwired: the ports' launcher is the host's systemd launcher, which refuses a hook that is not
/// there by its path before it spawns anything.
#[test]
fn the_host_ports_are_built_from_flags_and_files_without_the_network() {
    let rig = Rig::new("assemble-host-ports");
    let files = HostFiles {
        ca_bundle: rig.scratch.path().join("ca.pem"),
        gh: rig.scratch.path().join("gh"),
        trusted_root: rig.scratch.path().join("trusted_root.json"),
        pki: rig.scratch.path().join("pki"),
    };
    let error = host_ports(&config(), &rig.settings(), &files, OWN)
        .err()
        .expect("no packages directory");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == rig.packages),
        "{error}"
    );

    let home = rig.state.join(GH_HOME);
    assert_eq!(
        mode_of(&home),
        0o700,
        "gh's home is made even so, root-only"
    );

    let under_a_file = rig.scratch.path().join("a-file");
    fs::write(&under_a_file, "x").expect("a file where the state dir would go");
    let settings = Settings {
        state_dir: under_a_file.join("state"),
        ..rig.settings()
    };
    let error = host_ports(&config(), &settings, &files, OWN)
        .err()
        .expect("gh's home cannot be made");
    let wanted = under_a_file.join("state").join(GH_HOME);
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == wanted),
        "{error}"
    );

    make_dir(&rig.packages, 0o755);
    fs::write(rig.packages.join("web"), "x").expect("a file where a service goes");
    let ports = host_ports(&config(), &rig.settings(), &files, OWN).expect("the ports");
    assert_eq!(ports.deadman_poll_secs, Some(DEADMAN_POLL_SECS));
    assert_eq!(ports.placement.roots.packages().dir(), rig.packages);
    let swept = ports.packages.remove_orphans(&name("web"));
    assert_eq!(
        swept.err().map(|error| error.refusal()),
        Some(Refusal::PackageIoError)
    );

    // The launcher is the host's: a hook that is not there does not start, named by its path
    // (the unwired launcher would answer that it has none), and nothing is spawned for it.
    let missing = rig.scratch.path().join("no-such-hook");
    let unit = "updater-web-prepare-1";
    let call = StepCall {
        service: "web",
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
}

/// Catches a start of the host's build that goes on past ports it could not build: a config that
/// loads and a packages directory that does not exist stop the start with `Io` naming the
/// directory, and a config that does not load stops it as a `Config` error, before any port is
/// built.
#[test]
fn the_start_of_the_host_build_stops_at_the_ports_it_cannot_build() {
    let rig = Rig::new("assemble-start-with");
    let files = HostFiles {
        ca_bundle: rig.scratch.path().join("ca.pem"),
        gh: rig.scratch.path().join("gh"),
        trusted_root: rig.scratch.path().join("trusted_root.json"),
        pki: rig.scratch.path().join("pki"),
    };
    let notifier = FakeNotifier::new(Log::default());
    let error = start_with(&rig.settings(), &files, OWN, &*notifier)
        .err()
        .expect("no packages directory");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == rig.packages),
        "{error}"
    );
    assert_eq!(notifier.sent(), 0);

    let settings = Settings {
        config: rig.scratch.path().join("no-such.toml"),
        ..rig.settings()
    };
    let error = start_with(&settings, &files, OWN, &*notifier)
        .err()
        .expect("no config");
    assert!(matches!(error, StartError::Config { .. }), "{error}");
}

/// Catches the host's files moved without the constants: where a host keeps the CA bundle, the
/// pinned `gh`, the trusted root and the pki directory, and the directory `gh` runs in.
#[test]
fn the_host_files_are_where_the_packaging_puts_them() {
    assert_eq!(
        HostFiles::host(),
        HostFiles {
            ca_bundle: PathBuf::from("/etc/ssl/certs/ca-certificates.crt"),
            gh: PathBuf::from("/usr/lib/hostkeeper/gh"),
            trusted_root: PathBuf::from("/usr/lib/hostkeeper/trusted_root.json"),
            pki: PathBuf::from("/etc/updater/pki"),
        }
    );
    assert_eq!(CA_BUNDLE, "/etc/ssl/certs/ca-certificates.crt");
    assert_eq!(GH_BINARY, "/usr/lib/hostkeeper/gh");
    assert_eq!(
        SIGSTORE_TRUSTED_ROOT,
        "/usr/lib/hostkeeper/trusted_root.json"
    );
    assert_eq!(GH_HOME, ".gh");
    assert_eq!(PKI_DIR, "/etc/updater/pki");
}

/// Catches a CA bundle read wrongly: each certificate section becomes a root, in order; a
/// section that is not base64 and a section of another kind are skipped; a bundle that is not
/// there has no root.
#[test]
fn the_roots_are_the_certificate_sections_of_the_bundle() {
    let rig = Rig::new("assemble-roots");
    let bundle = rig.scratch.path().join("bundle.pem");
    let text = "text between sections\n\
        -----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n\
        -----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n\
        -----BEGIN PRIVATE KEY-----\nCQgH\n-----END PRIVATE KEY-----\n\
        -----BEGIN CERTIFICATE-----\nBAUG\n-----END CERTIFICATE-----\n";
    fs::write(&bundle, text).expect("write the bundle");
    assert_eq!(tls_roots(&bundle), [vec![1, 2, 3], vec![4, 5, 6]]);
    assert!(tls_roots(&rig.scratch.path().join("missing.pem")).is_empty());
}

/// Catches a host verifier that believes a package without `gh`: a ref the trusted signer does
/// not list is rejected before `gh` is looked at, and one it lists, with no `gh` file to trust,
/// is `Unavailable`.
#[test]
fn the_host_verifier_trusts_no_gh_it_cannot_find() {
    let rig = Rig::new("assemble-verifier");
    let clock = rig.clock.clone();
    let verifier = HostVerifier::new(
        rig.scratch.path().join("gh"),
        rig.scratch.path().join("trusted_root.json"),
        rig.scratch.path().join("home"),
        clock.clone(),
    );
    let signers = config().trusted_signers;
    let sha = Sha256Hex::parse(&"ab".repeat(32)).expect("a digest");
    let commit = CommitId::new(&"cd".repeat(20)).expect("a commit");
    let deadline = Stamp::now(&*clock);
    let archive = rig.scratch.path().join("package.tar.zst");
    let bundle = rig.scratch.path().join("package.sigstore.json");
    let verify = |source_ref: &str| {
        let source_ref = SourceRef::new(source_ref).expect("a ref");
        verifier.verify(&Attest {
            archive: &archive,
            bundle: &bundle,
            sha256: &sha,
            commit: &commit,
            source_ref: &source_ref,
            signers: &signers,
            deadline: &deadline,
        })
    };
    assert_eq!(verify("refs/tags/v1.0.0"), Err(VerifyError::Unavailable));
    assert_eq!(
        verify("refs/heads/main"),
        Err(VerifyError::Rejected(
            "the ref refs/heads/main matches none of the trusted signer's refs".to_owned()
        ))
    );
}

/// Catches a start with no notifier changing: `NoNotifier` is not a notifier that fails.
#[test]
fn a_start_with_no_notifier_is_ready() {
    let rig = Rig::new("assemble-no-notifier");
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("started");
    assert_eq!(started.boot_report().ready, Some(Ok(())));
    assert!(NoNotifier.ready().is_ok());
}
