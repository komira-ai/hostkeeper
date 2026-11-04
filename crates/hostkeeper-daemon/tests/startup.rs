//! Start-up: the config before anything else, one daemon per state directory, the admin socket
//! and its modes, the real socket's peer check, and the command line.

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::cli::{
    self, Cli, Command, DEFAULT_ADMIN_SOCKET, DEFAULT_CONFIG, DEFAULT_PACKAGES_DIR,
    DEFAULT_STATE_DIR, DaemonFlags, EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED, UNWIRED, Unwired,
};
use hostkeeper_daemon::packages::{PackageError, Packages};
use hostkeeper_daemon::start::{self, LOCK_FILE, StartError, Started};
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_placed::UnitControl;
use hostkeeper_proto::hostkeeper::local::v1::StatusRequest;
use hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient;
use hostkeeper_steps::{LaunchError, StepLauncher};
use hyper_util::rt::TokioIo;
use support::{CONFIG, OWN, Rig, my_uid, snapshot};
use tonic::transport::{Endpoint, Uri};

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

/// Catches a lock file, a socket or a directory made before the config loaded: a config with a
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
    assert!(!rig.socket.parent().expect("the socket dir").exists());
}

/// Catches a lock that only holds within one daemon, or that the second daemon breaks: a second
/// start on the same state directory is `Locked` (its exit status) and touches nothing of the
/// first, which still answers.
#[test]
fn a_second_daemon_on_the_same_state_dir_exits_locked() {
    let rig = Rig::new("locked");
    let first = start(&rig).expect("the first daemon");
    let socket = fs::symlink_metadata(&rig.socket).expect("the socket").ino();
    let error = start(&rig).err().expect("the second is refused");
    assert!(matches!(error, StartError::Locked { .. }), "{error}");
    assert_eq!(error.exit_code(), EXIT_LOCKED);
    assert!(error.to_string().contains(LOCK_FILE), "{error}");
    assert_eq!(
        fs::symlink_metadata(&rig.socket)
            .expect("still there")
            .ino(),
        socket
    );
    assert!(first.daemon().status().is_ok());
}

/// Catches a lock that is never released: once the first daemon is gone, a third starts, and
/// binds the socket again over the stale one the first left.
#[test]
fn the_lock_is_released_when_the_first_daemon_ends() {
    let rig = Rig::new("released");
    let first = start(&rig).expect("the first daemon");
    drop(first);
    assert!(
        rig.socket.exists(),
        "the first daemon leaves its socket file"
    );
    // Binding over the stale socket file works only because it is removed first.
    let third = start(&rig).expect("a daemon after the first ended");
    assert_eq!(mode(&rig.socket), 0o600);
    drop(third);
}

/// Catches an admin socket or directory others can reach: the socket is 0600 in a 0700
/// directory, and an existing directory of looser mode is closed to 0700.
#[test]
fn the_admin_socket_is_0600_in_a_0700_directory() {
    let rig = Rig::new("modes");
    let dir = rig.socket.parent().expect("the socket dir").to_path_buf();
    fs::create_dir_all(&dir).expect("the dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("loose");
    let started = start(&rig).expect("started");
    assert_eq!(mode(&rig.socket), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&rig.state), 0o700);
    assert_eq!(mode(&rig.state.join(LOCK_FILE)), 0o600);
    drop(started);
}

/// Catches a file that is not a socket removed or overwritten at the socket's path, a socket
/// directory that is a file taken for a directory, and a path the kernel refuses: each stops
/// start-up, and the file in the way is as it was.
#[test]
fn what_is_in_the_way_of_the_socket_stops_start_up_and_is_left_alone() {
    let rig = Rig::new("in-the-way");
    let dir = rig.socket.parent().expect("the socket dir").to_path_buf();
    fs::create_dir_all(&dir).expect("the dir");
    fs::write(&rig.socket, "keep me").expect("a file");
    let error = start(&rig).err().expect("refused");
    assert!(error.to_string().contains("is not a socket"), "{error}");
    assert_eq!(error.exit_code(), EXIT_FAILED);
    assert_eq!(fs::read_to_string(&rig.socket).expect("kept"), "keep me");

    let rig = Rig::new("dir-file");
    fs::write(rig.socket.parent().expect("the socket dir"), "a file").expect("a file");
    let error = start(&rig).err().expect("refused");
    assert!(error.to_string().contains("is not a directory"), "{error}");

    let mut rig = Rig::new("too-long");
    rig.socket = rig.socket.with_file_name("s".repeat(300));
    let error = start(&rig).err().expect("refused");
    assert!(matches!(error, StartError::Io { .. }), "{error}");
}

/// Catches the real socket serving a connection whose peer is not root: a client of the bound
/// socket, connecting as the uid the tests run as (not root), is closed unanswered.
#[tokio::test]
async fn a_real_connection_from_a_non_root_uid_is_closed() {
    let rig = Rig::new("real-sock");
    let started = start(&rig).expect("started");
    let served = tokio::spawn(start::serve(started));
    let path = rig.socket.clone();
    let connector = tower::service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
            tokio::net::UnixStream::connect(path)
                .await
                .map(TokioIo::new)
        }
    });
    let channel =
        Endpoint::from_static("http://admin.invalid").connect_with_connector_lazy(connector);
    let answered = AdminClient::new(channel).status(StatusRequest {}).await;
    assert_eq!(answered.is_ok(), my_uid() == 0, "{answered:?}");
    served.abort();
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
        "--admin-socket".into(),
        rig.socket.clone().into(),
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
    let Command::Daemon(flags) = Cli::parse_from(["hostkeeper", "daemon"]).command;
    assert_eq!(
        flags,
        DaemonFlags {
            config: DEFAULT_CONFIG.into(),
            state_dir: DEFAULT_STATE_DIR.into(),
            packages_dir: DEFAULT_PACKAGES_DIR.into(),
            admin_socket: DEFAULT_ADMIN_SOCKET.into(),
            notify_systemd: false,
        }
    );
    assert_eq!(flags.settings().trusted_uid, 0);
    let readme = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("the README");
    let documented = [
        format!("`--config` | `{DEFAULT_CONFIG}`"),
        format!("`--state-dir` | `{DEFAULT_STATE_DIR}`"),
        format!("`--packages-dir` | `{DEFAULT_PACKAGES_DIR}`"),
        format!("`--admin-socket` | `{DEFAULT_ADMIN_SOCKET}`"),
        "`--notify-systemd` | off".to_owned(),
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

/// Catches a build without a launcher or package store running something: each port refuses,
/// and an order on them ends without running a step.
#[test]
fn the_unwired_ports_refuse_everything() {
    assert!(matches!(Unwired.stop("u"), Err(LaunchError::Launcher(why)) if why == UNWIRED));
    let reload = Unwired.daemon_reload().expect_err("no service manager");
    assert_eq!(
        (reload.action.as_str(), reload.why.as_str()),
        ("daemon-reload", UNWIRED)
    );
    let stop = Unwired
        .stop_unit("web.service")
        .expect_err("no service manager");
    assert_eq!(
        (stop.action.as_str(), stop.why.as_str()),
        ("stop web.service", UNWIRED)
    );
    let refused = Unwired.version_dir(
        &ServiceName::new("web").expect("name"),
        &Version::new("1.0.0").expect("version"),
        &PackageDigest::new(&"ab".repeat(32)).expect("digest"),
    );
    assert_eq!(refused, Err(PackageError::Other(UNWIRED.to_owned())));
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
