//! `hostkeeper activate` and the state directory's lock: it runs only where no daemon runs, says
//! which process holds the lock when one does, and takes a lock whose holder is gone with no flag.
//! Also the three verbs on the command line, to their exit statuses.

mod support;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_config::names::Version;
use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
use hostkeeper_daemon::cli::{self, Cli, HostFiles};
use hostkeeper_daemon::local::{self, ActivateArgs, EXIT_CONFIG, EXIT_LOCKED, EXIT_REFUSED};
use hostkeeper_daemon::start::{self, LOCK_FILE, StartError};
use hostkeeper_launchd::Platform;
use hostkeeper_observe::fake::FakeCommandRunner;
use support::{OWN, Rig, name};

/// The host's files of the rig, none of them there.
fn files(rig: &Rig) -> HostFiles {
    HostFiles {
        repositories: None,
        os_root: rig.scratch.path().join("os"),
        stores: rig.scratch.path().join("stores.toml"),
        macos_pkg_signers: None,
        pki: rig.scratch.path().join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    }
}

/// The adapters of a Linux host whose `systemctl` is never asked.
fn linux() -> Result<Adapters, StartError> {
    Ok(Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
    ))
}

/// `hostkeeper activate --service web --version 1.0.0` on the rig, with its daemon put together
/// as on a host: its exit status, standard output and standard error.
fn activate(rig: &Rig) -> (u8, String, String) {
    let args = ActivateArgs {
        config: rig.config.clone(),
        state_dir: rig.state.clone(),
        packages_dir: rig.packages.clone(),
        service: name("web"),
        version: Version::new("1.0.0").expect("a version"),
        pin: false,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = local::activate_with(
        &args,
        &rig.settings(),
        &files(rig),
        &linux,
        &mut out,
        &mut err,
    );
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("text");
    (code, text(out), text(err))
}

/// What `activate` says once it holds the lock, on a rig that keeps nothing of `web`.
const NOT_KEPT: &str = "hostkeeper activate: refused: not_kept: web 1.0.0 is not kept on this \
                        host, which keeps none\n";

/// The state directory, mode 0700, as the daemon makes it.
fn state_dir(rig: &Rig) {
    fs::create_dir_all(&rig.state).expect("the state dir");
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
}

/// Catches an activate that runs beside the daemon, or one that does not say who stops it: with
/// a daemon started on the state directory, `activate` exits with the locked status and names the
/// daemon's pid and the lock file; a lock held by a process that wrote no pid is named as another
/// process; once the daemon is gone, `activate` takes the lock and goes on.
#[test]
fn activate_refuses_a_live_lock_holder_and_names_it() {
    let rig = Rig::new("activate-locked");
    let lock = rig.state.join(LOCK_FILE);
    let daemon = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon");
    assert_eq!(
        activate(&rig),
        (
            EXIT_LOCKED,
            String::new(),
            format!(
                "hostkeeper activate: the daemon (pid {}) holds {}: stop the daemon first\n",
                std::process::id(),
                lock.display()
            )
        )
    );
    drop(daemon);
    assert_eq!(
        activate(&rig),
        (EXIT_REFUSED, String::new(), NOT_KEPT.to_owned())
    );

    fs::write(&lock, "").expect("an empty lock file");
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .expect("the lock file");
    held.try_lock().expect("the lock");
    assert_eq!(
        activate(&rig),
        (
            EXIT_LOCKED,
            String::new(),
            format!(
                "hostkeeper activate: another process holds {}: stop the daemon first\n",
                lock.display()
            )
        )
    );
}

/// Catches an activate that needs a flag, or a hand, to take the lock of a daemon that died: the
/// lock file names a pid no process has and nothing holds it; `activate` takes it with no flag,
/// goes on, and the file names the activate's own pid after it.
#[test]
fn a_dead_holders_lock_is_taken_with_no_flag() {
    let rig = Rig::new("activate-dead-holder");
    state_dir(&rig);
    let lock = rig.state.join(LOCK_FILE);
    fs::write(&lock, "4194305\n").expect("a dead daemon's lock file");
    assert_eq!(
        activate(&rig),
        (EXIT_REFUSED, String::new(), NOT_KEPT.to_owned())
    );
    assert_eq!(
        fs::read_to_string(&lock).expect("the lock file"),
        format!("{}\n", std::process::id())
    );
}

/// Catches a daemon config that `activate` does not read as the daemon does: a config that is not
/// there exits with the config status, before the state directory is made or locked.
#[test]
fn activate_with_no_daemon_config_exits_with_the_config_status() {
    let rig = Rig::new("activate-no-config");
    fs::remove_file(&rig.config).expect("no config");
    let (code, out, err) = activate(&rig);
    assert_eq!((code, out.as_str()), (EXIT_CONFIG, ""));
    assert!(
        err.starts_with(&format!("hostkeeper activate: {}: ", rig.config.display())),
        "{err}"
    );
    assert!(!rig.state.exists());
}

/// The exit status of the command line `line`, run as the binary runs it.
fn status(line: &[&str]) -> String {
    format!("{:?}", cli::run(Cli::parse_from(line)))
}

/// Catches a verb the binary does not wire to its code: `activate` with a config that is not
/// there exits with the config status; `pin` of a version the host does not keep exits refused
/// and writes no pin; `unpin` of a service with no pin exits 0.
#[test]
fn the_three_verbs_run_from_the_command_line() {
    let rig = Rig::new("activate-cli");
    let missing = rig.scratch.path().join("missing.toml");
    let paths = [
        "--state-dir",
        rig.state.to_str().expect("a path"),
        "--packages-dir",
        rig.packages.to_str().expect("a path"),
    ];
    let mut activate = vec!["hostkeeper", "activate", "--config"];
    activate.push(missing.to_str().expect("a path"));
    activate.extend(paths);
    activate.extend(["--service", "web", "--version", "1.0.0", "--pin"]);
    assert_eq!(
        status(&activate),
        format!("{:?}", ExitCode::from(EXIT_CONFIG))
    );
    let mut pin = vec!["hostkeeper", "pin"];
    pin.extend(paths);
    pin.extend(["--service", "web", "--version", "1.0.0"]);
    assert_eq!(status(&pin), format!("{:?}", ExitCode::from(EXIT_REFUSED)));
    assert!(!rig.scratch.path().join("pins").exists());
    let unpin = [
        "hostkeeper",
        "unpin",
        "--state-dir",
        rig.state.to_str().expect("a path"),
        "--service",
        "web",
    ];
    assert_eq!(status(&unpin), format!("{:?}", ExitCode::from(0)));
}

/// Catches flags that take what the daemon would refuse, or defaults that are not the daemon's:
/// a service or version that breaks its grammar is a usage error, and the directories default to
/// the daemon's own.
#[test]
fn the_verbs_take_the_daemons_grammars_and_defaults() {
    for line in [
        [
            "hostkeeper",
            "pin",
            "--service",
            "-web",
            "--version",
            "1.0.0",
        ],
        ["hostkeeper", "pin", "--service", "web", "--version", "1/0"],
    ] {
        let refused = Cli::try_parse_from(line).expect_err("a usage error");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
    }
    let parsed = Cli::parse_from([
        "hostkeeper",
        "activate",
        "--service",
        "web",
        "--version",
        "1.0.0",
    ]);
    let cli::Command::Local(local::LocalVerb::Activate(args)) = &parsed.command else {
        panic!("not activate");
    };
    let settings = args.settings();
    assert_eq!(
        (
            settings.config.to_str(),
            settings.state_dir.to_str(),
            settings.packages_dir.to_str(),
            settings.trusted_uid,
            args.pin,
        ),
        (
            Some(cli::DEFAULT_CONFIG),
            Some(cli::DEFAULT_STATE_DIR),
            Some(cli::DEFAULT_PACKAGES_DIR),
            0,
            false
        )
    );
}
