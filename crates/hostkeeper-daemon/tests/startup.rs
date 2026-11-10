//! Start-up: the config before anything else, one daemon per state directory, the modes of the
//! state directory and the lock, and the command line.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_daemon::cli::{
    self, Cli, Command, DEFAULT_CONFIG, DEFAULT_PACKAGES_DIR, DEFAULT_STATE_DIR, DaemonFlags,
    EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED, UNWIRED, Unwired,
};
use hostkeeper_daemon::start::{self, LOCK_FILE, StartError, Started};
use hostkeeper_link::{DEFAULT_HEARTBEAT_SECS, LoopFlags};
use hostkeeper_placed::{UnitControl, UnitControlError};
use hostkeeper_steps::{LaunchError, StepLauncher};
use support::{CONFIG, OWN, Rig, snapshot};
use support_host::NativeHost;

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

/// The unwired launcher for everything but the tools of the repository installer, which the
/// rig's launcher answers: the version is staged from the repository, and each step of it
/// meets the unwired launcher.
struct ToolsOnly(Arc<hostkeeper_daemon::testing::ScriptedLauncher>);

impl StepLauncher for ToolsOnly {
    fn run(
        &self,
        call: &hostkeeper_steps::StepCall<'_>,
    ) -> Result<hostkeeper_steps::StepExit, LaunchError> {
        Unwired.run(call)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        Unwired.stop(unit)
    }

    fn run_tool(
        &self,
        call: &hostkeeper_steps::ToolCall<'_>,
    ) -> Result<hostkeeper_steps::ToolExit, LaunchError> {
        self.0.run_tool(call)
    }
}

fn start(rig: &Rig) -> Result<Started, StartError> {
    start::startup(&rig.settings(), rig.ports(), OWN)
}

/// Catches a lock file or a directory made before the config loaded: a config with a
/// key no table has, and one with a bad `ca_sha256`, each stop start-up with an error naming the
/// key and the exit status of a config error, and the scratch directory is as it was.
#[test]
fn a_config_that_fails_to_load_stops_the_daemon_naming_the_key_and_changes_nothing() {
    let rig = Rig::new("bad-config");
    let bad_hash = CONFIG.replace(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "not-a-hash",
    );
    let rows = [
        (format!("colour = \"blue\"\n{CONFIG}"), "\"colour\""),
        (bad_hash, "ca_sha256"),
    ];
    for (text, key) in rows {
        fs::write(&rig.config, text).expect("write the config");
        let before = snapshot(rig.scratch.path());
        let error = start(&rig).err().expect("refused");
        let message = error.to_string();
        assert!(matches!(error, StartError::Config { .. }), "{message}");
        assert!(message.contains(key), "{message}");
        assert!(
            message.starts_with(&rig.config.display().to_string()),
            "{message}"
        );
        assert_eq!(error.exit_code(), EXIT_CONFIG);
        assert_eq!(snapshot(rig.scratch.path()), before);
    }
    assert!(!rig.state.exists());
    assert!(!rig.run_dir.exists());
}

/// Catches a lock that only holds within one daemon, or that the second daemon breaks: a second
/// start on the same state directory is `Locked` (its exit status) and touches nothing of the
/// first, which still reports its status.
#[test]
fn a_second_daemon_on_the_same_state_dir_exits_locked() {
    let rig = Rig::new("locked");
    let first = start(&rig).expect("the first daemon");
    let lock = fs::symlink_metadata(rig.state.join(LOCK_FILE))
        .expect("the lock file")
        .ino();
    let error = start(&rig).err().expect("the second is refused");
    assert!(matches!(error, StartError::Locked { .. }), "{error}");
    assert_eq!(error.exit_code(), EXIT_LOCKED);
    assert!(error.to_string().contains(LOCK_FILE), "{error}");
    assert_eq!(
        fs::symlink_metadata(rig.state.join(LOCK_FILE))
            .expect("still there")
            .ino(),
        lock
    );
    assert!(first.daemon().status().is_ok());
}

/// Catches a lock that is never released: once the first daemon is gone, a third starts on the
/// same state directory, and the lock file is as private as before.
#[test]
fn the_lock_is_released_when_the_first_daemon_ends() {
    let rig = Rig::new("released");
    let first = start(&rig).expect("the first daemon");
    drop(first);
    let third = start(&rig).expect("a daemon after the first ended");
    assert_eq!(mode(&rig.state.join(LOCK_FILE)), 0o600);
    drop(third);
}

/// Catches a state directory or lock file others can reach: the state directory is 0700 and the
/// lock file 0600, and a state directory of looser mode is closed to 0700.
#[test]
fn the_state_dir_is_0700_and_the_lock_0600() {
    let rig = Rig::new("modes");
    fs::create_dir_all(&rig.state).expect("the dir");
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o755)).expect("loose");
    let started = start(&rig).expect("started");
    assert_eq!(mode(&rig.state), 0o700);
    assert_eq!(mode(&rig.state.join(LOCK_FILE)), 0o600);
    drop(started);
}

/// Catches a start-up error of the command line that exits 0 or with another error's status:
/// `hostkeeper daemon` with a config that is not there exits with the config error's status.
#[test]
fn the_command_line_exits_with_the_config_errors_status() {
    let rig = Rig::new("cli");
    let missing = rig.scratch.path().join("missing.toml");
    let args: Vec<std::ffi::OsString> = vec![
        "hostkeeper".into(),
        "daemon".into(),
        "--config".into(),
        missing.into(),
        "--state-dir".into(),
        rig.state.clone().into(),
        "--packages-dir".into(),
        rig.packages.clone().into(),
    ];
    let code = cli::run(Cli::parse_from(args));
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(EXIT_CONFIG))
    );
    assert!(!rig.state.exists());
}

/// Catches a default that drifts from the documented one, or a flag read from somewhere else:
/// `hostkeeper daemon` alone has the four defaults and no readiness notice, the daemon config is
/// trusted when root owns it, and the README states each default and each exit status.
#[test]
fn daemon_flag_defaults_are_the_documented_ones() {
    let Command::Daemon(flags) = Cli::parse_from(["hostkeeper", "daemon"]).command else {
        panic!("`daemon` parses as the daemon verb");
    };
    assert_eq!(
        flags,
        DaemonFlags {
            config: DEFAULT_CONFIG.into(),
            state_dir: DEFAULT_STATE_DIR.into(),
            packages_dir: DEFAULT_PACKAGES_DIR.into(),
            notify_systemd: false,
            ready_file: None,
            link: LoopFlags {
                heartbeat_secs: DEFAULT_HEARTBEAT_SECS,
            },
        }
    );
    assert_eq!(flags.settings().trusted_uid, 0);
    let readme = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("the README");
    let documented = [
        format!("`--config` | `{DEFAULT_CONFIG}`"),
        format!("`--state-dir` | `{DEFAULT_STATE_DIR}`"),
        format!("`--packages-dir` | `{DEFAULT_PACKAGES_DIR}`"),
        "`--notify-systemd` | off".to_owned(),
        format!("`--heartbeat-secs` | `{DEFAULT_HEARTBEAT_SECS}`"),
        format!("| {EXIT_FAILED} |"),
        "| 2 |".to_owned(),
        format!("| {EXIT_CONFIG} |"),
        format!("| {EXIT_LOCKED} |"),
    ];
    for line in documented {
        assert!(readme.contains(&line), "the README does not state {line}");
    }
}

/// Catches the config and lock errors sharing an exit status with each other or with a usage
/// error (2), or another error taking theirs.
#[test]
fn the_locked_and_config_errors_exit_with_distinct_codes() {
    let codes = [EXIT_FAILED, 2, EXIT_CONFIG, EXIT_LOCKED];
    for (n, a) in codes.iter().enumerate() {
        assert!(codes[n + 1..].iter().all(|b| b != a), "{codes:?}");
    }
    let runtime = StartError::Runtime(std::io::Error::other("x"));
    assert_eq!(runtime.exit_code(), EXIT_FAILED);
    assert_eq!(
        runtime.to_string(),
        "the async runtime could not be made: x"
    );
    let own = hostkeeper_config::manifest::HostkeeperVersion::parse("1.x").expect_err("bad");
    let own = StartError::OwnVersion(own);
    assert_eq!(own.exit_code(), EXIT_FAILED);
    assert!(
        own.to_string()
            .starts_with("this daemon's own version is not major.minor.patch: "),
        "{own}"
    );
}

/// Catches a build without a launcher or service manager running something: each
/// port refuses, naming what it was asked, and an order on them ends without running a step.
#[test]
fn the_unwired_ports_refuse_everything() {
    assert!(matches!(Unwired.stop("u"), Err(LaunchError::Launcher(why)) if why == UNWIRED));
    let unwired = |action: &str| UnitControlError {
        action: action.to_owned(),
        why: UNWIRED.to_owned(),
    };
    assert_eq!(Unwired.daemon_reload(), Err(unwired("daemon-reload")));
    assert_eq!(Unwired.stop_unit("u"), Err(unwired("stop u")));
    assert_eq!(Unwired.start_unit("u"), Err(unwired("start u")));
    assert_eq!(Unwired.is_active("u"), Err(unwired("is-active u")));
    let rig = NativeHost::new("unwired");
    rig.add_staged("web", "2.0.0");
    let order = rig.update("web", "2.0.0", 1);
    let mut ports = rig.ports();
    ports.launcher = Arc::new(ToolsOnly(Arc::clone(&rig.launcher)));
    let daemon = hostkeeper_daemon::Daemon::new(support::config(), rig.dirs(), ports, OWN);
    let reply = daemon.order(&order);
    assert!(
        matches!(reply, hostkeeper_daemon::Reply::Finished { .. }),
        "{reply:?}"
    );
    assert!(rig.launcher.runs().is_empty());
}

/// Catches the flag of the trusted root's report still being taken: the daemon checks no
/// attestation, so `--trusted-root-max-age-days` is a flag it does not have, a usage error.
#[test]
fn the_trusted_root_age_limit_flag_is_a_usage_error() {
    let parsed = Cli::try_parse_from(["hostkeeper", "daemon", "--trusted-root-max-age-days", "30"]);
    assert_eq!(
        parsed.map(|_| ()).map_err(|error| error.kind()),
        Err(clap::error::ErrorKind::UnknownArgument)
    );
}
