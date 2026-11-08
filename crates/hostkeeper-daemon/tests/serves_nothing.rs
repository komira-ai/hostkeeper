//! The daemon serves nothing on the host (docs/design/two-programs.md, K3): it binds no socket,
//! `hostkeeper admin` is a usage error, and `--admin-socket`, which every package unit before
//! stage 2 passes, is accepted and ignored, so a daemon started by today's unit and by the
//! stage-2 unit (which drops the flag) starts the same and binds nothing. Everything is on
//! scratch directories: the tests look at the scratch tree, never at the host's.

mod support;

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use hostkeeper_daemon::Stage;
use hostkeeper_daemon::cli::{
    Cli, Command, DEFAULT_CONFIG, DEFAULT_PACKAGES_DIR, DEFAULT_STATE_DIR,
    DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS, DaemonFlags,
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

/// The `ExecStart=` line of the package unit of every release before stage 2 of
/// docs/design/two-programs.md: the unit the farm's hosts run today, until a stage-2 release
/// reaches them. The repository's units are stage 2's now, so it is kept here, word for word.
const RELEASED_EXEC_START: &str = "/var/lib/updater/state/hostkeeper/bin/current daemon \
    --config /etc/hostkeeper/hostkeeper.toml --state-dir /var/lib/updater/state \
    --packages-dir /var/lib/hostkeeper/packages --admin-socket /run/hostkeeper/admin.sock \
    --notify-systemd";

/// The words of a command line after its program.
fn arguments_of(line: &str) -> Vec<String> {
    line.split_whitespace().skip(1).map(str::to_owned).collect()
}

/// The arguments the daemon is started with today: [`RELEASED_EXEC_START`]'s.
fn unit_arguments() -> Vec<String> {
    arguments_of(RELEASED_EXEC_START)
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

/// The arguments of stage 2's package unit (design section 4.1, which drops `--admin-socket`).
fn stage_two_arguments() -> Vec<String> {
    arguments_in("packaging/linux/units/hostkeeper.service")
}

/// The arguments of stage 2's bootstrap unit, which starts the packaged binary.
fn bootstrap_arguments() -> Vec<String> {
    arguments_in("packaging/linux/hostkeeper.service")
}

/// Catches a pin of today's line that has drifted from the units it came from: without its
/// `--admin-socket` pair, today's arguments are stage 2's package unit's and bootstrap unit's,
/// word for word, so the two lists the tests below start differ by that pair alone.
#[test]
fn today_s_arguments_are_stage_two_s_with_the_admin_socket_pair() {
    let mut today = unit_arguments();
    let at = today
        .iter()
        .position(|word| word == "--admin-socket")
        .expect("today's unit passes --admin-socket");
    let pair: Vec<String> = today.drain(at..at + 2).collect();
    assert_eq!(pair, ["--admin-socket", "/run/hostkeeper/admin.sock"]);
    assert_eq!(today, stage_two_arguments());
    assert_eq!(today, bootstrap_arguments());
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

/// The settings of `flags` with the daemon's directories moved into the rig's scratch directory
/// and everything else as parsed but the uid that owns the config.
fn rerooted(rig: &Rig, flags: &DaemonFlags) -> Settings {
    Settings {
        config: rig.config.clone(),
        state_dir: rig.state.clone(),
        packages_dir: rig.packages.clone(),
        run_dir: rig.run_dir.clone(),
        // The scratch config is owned by the user running the tests, not by root.
        trusted_uid: support::my_uid(),
        ..flags.settings()
    }
}

/// Starts the daemon on `settings` as the unit's flags say (it tells a fake notifier it is ready
/// when `flags.notify_systemd`), and returns it once the boot work is over.
fn start_as(rig: &Rig, flags: &DaemonFlags) -> Started {
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rerooted(rig, flags), rig.ports(), OWN, &*notifier)
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
/// point, and the path given as `--admin-socket` does not exist, nor does its directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_started_and_serving_daemon_binds_no_socket() {
    let rig = Rig::new("serves-nothing");
    let given = rig.scratch.path().join("admin").join("admin.sock");
    let mut arguments = stage_two_arguments();
    arguments.extend(["--admin-socket".to_owned(), given.display().to_string()]);
    let flags = parse(&arguments);
    assert_eq!(flags.admin_socket.as_deref(), Some(given.as_path()));

    let started = start_as(&rig, &flags);
    assert_eq!(sockets_under(rig.scratch.path()), Vec::<PathBuf>::new());
    assert!(!given.exists(), "the flag's path is not bound");
    assert!(!given.parent().expect("its directory").exists());
    assert!(!rig.run_dir.join("hostkeeper").exists());

    serve_until_restart(started, flags.link.heartbeat_secs).await;
    assert_eq!(sockets_under(rig.scratch.path()), Vec::<PathBuf>::new());
    assert!(!given.exists());
    assert!(!given.parent().expect("its directory").exists());
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
/// command, and `hostkeeper daemon --help` shows no `--admin-socket` (the flag is hidden, but
/// accepted).
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

/// Catches the arguments of today's unit or of the stage-2 unit parsing differently, or to other
/// settings than the documented ones: both are the daemon command, both give the same settings
/// (no admin socket in either), the config, state and packages directories and the flags of the
/// package's unit, the readiness notice on and the documented heartbeat and trusted root limit.
#[test]
fn today_s_unit_and_the_stage_two_unit_give_the_same_settings() {
    let today = parse(&unit_arguments());
    let stage_two = parse(&stage_two_arguments());
    assert_eq!(parse(&bootstrap_arguments()), stage_two);
    assert_eq!(today.settings(), stage_two.settings());
    assert_eq!(
        (&today.admin_socket, &stage_two.admin_socket),
        (&Some(PathBuf::from("/run/hostkeeper/admin.sock")), &None)
    );
    for flags in [&today, &stage_two] {
        assert_eq!(flags.config, Path::new(DEFAULT_CONFIG));
        assert_eq!(flags.state_dir, Path::new(DEFAULT_STATE_DIR));
        assert_eq!(flags.packages_dir, Path::new(DEFAULT_PACKAGES_DIR));
        assert!(flags.notify_systemd);
        assert_eq!(flags.link.heartbeat_secs, DEFAULT_HEARTBEAT_SECS);
        assert_eq!(
            flags.trusted_root_max_age_days,
            DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS
        );
        let settings = flags.settings();
        assert_eq!(settings.trusted_uid, 0);
        assert_eq!(settings.config, Path::new(DEFAULT_CONFIG));
    }
    // Without the pair, the flags are the ones parsed with the admin socket cleared.
    assert_eq!(
        DaemonFlags {
            admin_socket: None,
            ..today
        },
        stage_two
    );
}

/// Catches a daemon that does not start from the arguments of today's unit or of the stage-2
/// unit, or that binds a socket from either: each is started on a rig of its own with its
/// directories moved into scratch and the rest as parsed (the readiness notice, the heartbeat),
/// the boot work is over and it says it is ready, and then served until asked to restart, no
/// entry of the scratch tree is a unix socket.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_starts_from_today_s_unit_and_from_the_stage_two_unit_and_binds_no_socket() {
    for (name, arguments) in [
        ("serves-today", unit_arguments()),
        ("serves-stage-two", stage_two_arguments()),
        ("serves-bootstrap", bootstrap_arguments()),
    ] {
        let rig = Rig::new(name);
        let flags = parse(&arguments);
        assert!(flags.notify_systemd, "{name}");
        let started = start_as(&rig, &flags);
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

/// Catches `--admin-socket` read: a path whose directory does not exist, one whose directory is a
/// regular file, one too long for a socket address and one that is a regular file somebody left
/// each start the daemon, and nothing at the path is made, removed or changed.
#[test]
fn an_admin_socket_flag_with_a_path_that_could_not_be_bound_still_starts() {
    let rig = Rig::new("serves-ignored");
    let root = rig.scratch.path();
    fs::write(root.join("a-file"), "not a directory").expect("a file");
    fs::write(root.join("left"), "keep me").expect("a file");
    let paths = [
        root.join("missing").join("deeper").join("admin.sock"),
        root.join("a-file").join("admin.sock"),
        root.join("s".repeat(300)),
        root.join("left"),
    ];
    for path in &paths {
        let rig = Rig::new("serves-ignored-start");
        let mut arguments = stage_two_arguments();
        arguments.extend(["--admin-socket".to_owned(), path.display().to_string()]);
        let started = start_as(&rig, &parse(&arguments));
        assert!(started.daemon().status().is_ok(), "{}", path.display());
        assert_eq!(sockets_under(rig.scratch.path()), Vec::<PathBuf>::new());
    }
    assert!(!root.join("missing").exists());
    assert_eq!(
        fs::read_to_string(root.join("a-file")).expect("kept"),
        "not a directory"
    );
    assert_eq!(
        fs::read_to_string(root.join("left")).expect("kept"),
        "keep me"
    );
    assert!(!root.join("s".repeat(300)).exists());
}
