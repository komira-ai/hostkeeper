//! The daemon serves nothing on the host (docs/design/two-programs.md, K3): it binds no socket,
//! `hostkeeper admin` is a usage error (as is `--admin-socket`, `cli_removed_verbs.rs`), and the
//! daemon starts the same from the arguments of the package unit and of the bootstrap unit.
//! Everything is on scratch directories: the tests look at the scratch tree, never at the
//! host's.

mod support;

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use hostkeeper_daemon::Stage;
use hostkeeper_daemon::cli::{
    Cli, Command, DEFAULT_CONFIG, DEFAULT_PACKAGES_DIR, DEFAULT_STATE_DIR, DaemonFlags,
};
use hostkeeper_daemon::start::{self, Settings, Started};
use hostkeeper_daemon::testing::{FakeNotifier, Log};
use hostkeeper_link::DEFAULT_HEARTBEAT_SECS;
use support::{OWN, Rig, idle_links};

/// Every unix socket under `dir`, however deep, links not followed.
fn sockets_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).expect("list a directory") {
            let path = entry.expect("an entry").path();
            let kind = fs::symlink_metadata(&path).expect("metadata").file_type();
            if kind.is_socket() {
                found.push(path);
            } else if kind.is_dir() {
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The words of a command line after its program.
fn arguments_of(line: &str) -> Vec<String> {
    line.split_whitespace().skip(1).map(str::to_owned).collect()
}

/// The arguments of the `ExecStart=` line of the unit at `path` in the repository.
fn arguments_in(path: &str) -> Vec<String> {
    let at = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    let unit = fs::read_to_string(&at).unwrap_or_else(|e| panic!("{}: {e}", at.display()));
    let line = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .unwrap_or_else(|| panic!("{} has no ExecStart=", at.display()));
    arguments_of(line)
}

/// The arguments of the package unit.
fn package_arguments() -> Vec<String> {
    arguments_in("packaging/linux/units/hostkeeper.service")
}

/// The arguments of the bootstrap unit, which starts the packaged binary.
fn bootstrap_arguments() -> Vec<String> {
    arguments_in("packaging/linux/hostkeeper.service")
}

fn parse(arguments: &[String]) -> DaemonFlags {
    let argv = std::iter::once("hostkeeper".to_owned()).chain(arguments.iter().cloned());
    match Cli::try_parse_from(argv) {
        Ok(Cli {
            command: Command::Daemon(flags),
        }) => flags,
        other => panic!("the unit's arguments are not the daemon's: {other:?}"),
    }
}

/// The settings with the daemon's directories moved into the rig's scratch directory and the uid
/// that owns the config the user running the tests.
fn rerooted(rig: &Rig) -> Settings {
    Settings {
        config: rig.config.clone(),
        state_dir: rig.state.clone(),
        packages_dir: rig.packages.clone(),
        facts_file: rig.packages.with_file_name("facts.json"),
        run_dir: rig.run_dir.clone(),
        // The scratch config is owned by the user running the tests, not by root.
        trusted_uid: support::my_uid(),
        package_lock_wait_secs: hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    }
}

/// Starts the daemon on the rig's directories (it tells a fake notifier it is ready), and returns
/// it once the boot work is over.
fn start_as(rig: &Rig) -> Started {
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rerooted(rig), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert!(
        started
            .boot_report()
            .stages
            .contains(&Stage::UnlinkRegistrationSocket),
        "the boot work ran"
    );
    assert_eq!(
        notifier.sent(),
        1,
        "the daemon said it was ready once the boot work was over"
    );
    started
}

/// Serves `started` until it is asked to restart, as the daemon does after its own update, and
/// returns when it has ended.
async fn serve_until_restart(started: Started, period: std::num::NonZeroU64) {
    let daemon = std::sync::Arc::clone(started.daemon());
    let links = hostkeeper_daemon::links::Links {
        period,
        ..idle_links()
    };
    let served = tokio::spawn(start::serve(started, links));
    daemon.ctx().restart().request();
    served.await.expect("the server task");
}

/// Catches a daemon that binds a socket, started or serving: started on the rig, then served
/// until it is asked to restart, no entry of the whole scratch tree is a unix socket at any
/// point, and the daemon makes no directory of its own under the run directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_started_and_serving_daemon_binds_no_socket() {
    let rig = Rig::new("serves-nothing");
    let flags = parse(&package_arguments());

    let started = start_as(&rig);
    assert_eq!(sockets_under(rig.scratch.path()), Vec::<PathBuf>::new());
    assert!(!rig.run_dir.join("hostkeeper").exists());

    serve_until_restart(started, flags.link.heartbeat_secs).await;
    assert_eq!(sockets_under(rig.scratch.path()), Vec::<PathBuf>::new());
}

/// Catches `hostkeeper admin` still a command: `admin status` and a bare `admin` each fail to
/// parse with clap's usage error, whose exit code is 2 (an unrecognized subcommand).
#[test]
fn admin_is_a_usage_error() {
    for argv in [
        &["hostkeeper", "admin", "status"][..],
        &["hostkeeper", "admin"][..],
    ] {
        let error = Cli::try_parse_from(argv).expect_err("`admin` is not a command");
        assert_eq!(error.exit_code(), 2, "{argv:?}");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{argv:?}");
    }
}

/// Catches help that still advertises the admin socket: `hostkeeper --help` lists no `admin`
/// command, and `hostkeeper daemon --help` shows no `--admin-socket`.
#[test]
fn help_lists_no_admin_command_and_no_admin_socket_flag() {
    let mut cli = Cli::command();
    assert!(
        cli.get_subcommands()
            .all(|command| command.get_name() != "admin"),
        "no subcommand is named admin"
    );
    let top = cli.render_long_help().to_string();
    assert!(!top.contains("admin"), "{top}");
    let daemon = cli
        .find_subcommand_mut("daemon")
        .expect("the daemon command")
        .render_long_help()
        .to_string();
    assert!(!daemon.contains("admin"), "{daemon}");
    assert!(daemon.contains("--config"), "{daemon}");
}

/// Catches the arguments of the package unit or of the bootstrap unit parsing differently, or to
/// other settings than the documented ones: both are the daemon command and give the same
/// settings, the config, state and packages directories and the flags of the units, the
/// readiness notice on and the documented heartbeat and trusted root limit.
#[test]
fn the_package_unit_and_the_bootstrap_unit_give_the_same_settings() {
    let package = parse(&package_arguments());
    let bootstrap = parse(&bootstrap_arguments());
    assert_eq!(package, bootstrap);
    assert_eq!(package.settings(), bootstrap.settings());
    for flags in [&package, &bootstrap] {
        assert_eq!(flags.config, Path::new(DEFAULT_CONFIG));
        assert_eq!(flags.state_dir, Path::new(DEFAULT_STATE_DIR));
        assert_eq!(flags.packages_dir, Path::new(DEFAULT_PACKAGES_DIR));
        assert!(flags.notify_systemd);
        assert_eq!(flags.link.heartbeat_secs, DEFAULT_HEARTBEAT_SECS);
        let settings = flags.settings();
        assert_eq!(settings.trusted_uid, 0);
        assert_eq!(settings.config, Path::new(DEFAULT_CONFIG));
    }
}

/// Catches a daemon that does not start from the arguments of the package unit or of the
/// bootstrap unit, or that binds a socket from either: each is started on a rig of its own with its
/// directories moved into scratch and the rest as parsed (the readiness notice, the heartbeat),
/// the boot work is over and it says it is ready, and then served until asked to restart, no
/// entry of the scratch tree is a unix socket.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_starts_from_the_package_unit_and_from_the_bootstrap_unit_and_binds_no_socket() {
    for (name, arguments) in [
        ("serves-package", package_arguments()),
        ("serves-bootstrap", bootstrap_arguments()),
    ] {
        let rig = Rig::new(name);
        let flags = parse(&arguments);
        assert!(flags.notify_systemd, "{name}");
        let started = start_as(&rig);
        assert_eq!(
            sockets_under(rig.scratch.path()),
            Vec::<PathBuf>::new(),
            "{name}"
        );
        serve_until_restart(started, flags.link.heartbeat_secs).await;
        assert_eq!(
            sockets_under(rig.scratch.path()),
            Vec::<PathBuf>::new(),
            "{name}"
        );
    }
}
