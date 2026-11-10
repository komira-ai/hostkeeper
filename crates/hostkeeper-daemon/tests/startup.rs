//! Start-up: the config before anything else, one daemon per state directory, the modes of the
//! state directory and the lock, and the command line.

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_daemon::cli::{
    self, Cli, Command, DEFAULT_CONFIG, DEFAULT_FACTS_FILE, DEFAULT_PACKAGES_DIR,
    DEFAULT_STATE_DIR, DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS, DaemonFlags, EXIT_CONFIG, EXIT_FAILED,
    EXIT_LOCKED, UNWIRED, Unwired,
};
use hostkeeper_daemon::start::{self, LOCK_FILE, StartError, Started};
use hostkeeper_link::{DEFAULT_HEARTBEAT_SECS, LoopFlags};
use hostkeeper_placed::{UnitControl, UnitControlError};
use hostkeeper_steps::{LaunchError, StepLauncher};
use support::{CONFIG, OWN, Rig, snapshot};

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
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
            facts_file: DEFAULT_FACTS_FILE.into(),
            notify_systemd: false,
            ready_file: None,
            trusted_root_max_age_days: DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS,
            link: LoopFlags {
                heartbeat_secs: DEFAULT_HEARTBEAT_SECS,
            },
        }
    );
    assert_eq!(flags.settings().trusted_uid, 0);
    assert_eq!(
        flags.settings().trusted_root_max_age_days,
        DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS
    );
    let readme = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("the README");
    let documented = [
        format!("`--config` | `{DEFAULT_CONFIG}`"),
        format!("`--state-dir` | `{DEFAULT_STATE_DIR}`"),
        format!("`--packages-dir` | `{DEFAULT_PACKAGES_DIR}`"),
        format!("`--facts-file` | `{DEFAULT_FACTS_FILE}`"),
        "`--notify-systemd` | off".to_owned(),
        format!("`--heartbeat-secs` | `{DEFAULT_HEARTBEAT_SECS}`"),
        format!("`--trusted-root-max-age-days` | `{DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS}`"),
        format!("| {EXIT_FAILED} |"),
        "| 2 |".to_owned(),
        format!("| {EXIT_CONFIG} |"),
        format!("| {EXIT_LOCKED} |"),
    ];
    for line in documented {
        assert!(readme.contains(&line), "the README does not state {line}");
    }
}

/// Proves `--facts-file` reaches the settings, and from them the daemon's directories, as given.
#[test]
fn the_facts_file_flag_reaches_the_settings() {
    let parsed = Cli::try_parse_from(["hostkeeper", "daemon", "--facts-file", "/srv/facts.json"]);
    let Ok(Cli {
        command: Command::Daemon(flags),
    }) = parsed
    else {
        panic!("the flag parses: {parsed:?}");
    };
    assert_eq!(
        flags.settings().facts_file,
        std::path::PathBuf::from("/srv/facts.json")
    );
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
    let rig = Rig::new("unwired");
    rig.add("web", "2.0.0");
    let mut ports = rig.ports();
    ports.launcher = Arc::new(Unwired);
    let daemon = hostkeeper_daemon::Daemon::new(support::config(), rig.dirs(), ports, OWN);
    let reply = daemon.order(&support::update("web", "2.0.0", &support::d(), 1));
    assert!(
        matches!(reply, hostkeeper_daemon::Reply::Finished { .. }),
        "{reply:?}"
    );
    assert!(rig.launcher.runs().is_empty());
}

/// Catches `--trusted-root-max-age-days` not reaching the daemon's settings, or a limit of 0
/// accepted (every root would be reported stale the second after it is written): 30 is the
/// settings' limit, and 0 is a usage error.
#[test]
fn the_trusted_root_age_limit_flag_reaches_the_settings_and_is_at_least_one() {
    let parsed = Cli::try_parse_from(["hostkeeper", "daemon", "--trusted-root-max-age-days", "30"]);
    let Ok(Cli {
        command: Command::Daemon(flags),
    }) = parsed
    else {
        panic!("the flag parses: {parsed:?}");
    };
    assert_eq!(flags.settings().trusted_root_max_age_days.get(), 30);
    let zero = Cli::try_parse_from(["hostkeeper", "daemon", "--trusted-root-max-age-days", "0"]);
    assert_eq!(
        zero.map(|_| ()).map_err(|error| error.kind()),
        Err(clap::error::ErrorKind::ValueValidation)
    );
}
