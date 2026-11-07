//! The macOS pkg's `postinstall` (`hostkeeper_launchd::postinstall::Postinstall`), run as the
//! installer runs it: an executable file in its own directory, started with the path of the pkg,
//! the install location, the target volume and the root of the volume as arguments, and an empty
//! environment. It runs this binary's `self-update-seed` and `mac-config` and then `launchctl`;
//! every path is in a scratch directory and holds a space, so the script's quoting is exercised
//! by the real shell, and `launchctl` is the test program `hostkeeper-launchctl-fixture`, which
//! logs its calls. On a Mac the pkg names `/usr/local/bin/hostkeeper`, `/var/lib/updater/state`
//! and the managed preferences and config of `Postinstall::pkg`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_daemon::cli::{EXIT_FAILED, EXIT_LOCKED};
use hostkeeper_daemon::mac_config::kept_line;
use hostkeeper_daemon::selfupdate::{binary_dir, protocol_line};
use hostkeeper_daemon::start::LOCK_FILE;
use hostkeeper_launchd::postinstall::Postinstall;
use hostkeeper_testkit::Scratch;

/// What the seed prefixes its error with.
const SEED_ERROR: &str = "hostkeeper self-update-seed: ";

/// The job the script boots out.
const JOB: &str = "system/dev.hostkeeper.hostkeeper";

/// A Mac in a scratch directory: the binary the pkg installed (a copy of this crate's binary), the
/// `launchctl` fixture, and the paths of the daemon's state directory, the managed preferences,
/// the config and the plist, none of which exists until [`Mac::new`] or the script makes it.
struct Mac {
    scratch: Scratch,
    paths: Postinstall,
}

impl Mac {
    fn new(name: &str) -> Mac {
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir");
        let root = scratch.path();
        let bin_dir = root.join("usr local");
        fs::create_dir(&bin_dir).expect("the binary's directory");
        let binary = bin_dir.join("hostkeeper");
        fs::copy(env!("CARGO_BIN_EXE_hostkeeper"), &binary).expect("install the binary");
        let launchctl = bin_dir.join("launchctl");
        fs::copy(
            env!("CARGO_BIN_EXE_hostkeeper-launchctl-fixture"),
            &launchctl,
        )
        .expect("install the fake launchctl");
        let paths = Postinstall {
            binary,
            state_dir: root.join("updater state"),
            managed_config: root
                .join("Managed Preferences")
                .join("dev.hostkeeper.config.plist"),
            config: root.join("etc hostkeeper").join("hostkeeper.toml"),
            launchctl,
            plist: root
                .join("LaunchDaemons")
                .join("dev.hostkeeper.hostkeeper.plist"),
        };
        Mac { scratch, paths }
    }

    /// `current` of the binary directory: the program of the daemon's launchd job.
    fn current(&self) -> PathBuf {
        binary_dir(&self.paths.state_dir).join("current")
    }

    /// Makes the config, in a directory of its own, with `bytes`.
    fn put_config(&self, bytes: &[u8]) {
        let dir = self.paths.config.parent().expect("a parent");
        fs::create_dir_all(dir).expect("the config's directory");
        fs::write(&self.paths.config, bytes).expect("the config");
    }

    /// The lines the fake `launchctl` logged, none when it never ran.
    fn calls(&self) -> Vec<String> {
        let log = self.paths.launchctl.with_file_name("launchctl.calls");
        fs::read_to_string(log)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Makes the fake `launchctl` fail its `verb` with `code`.
    fn fail(&self, verb: &str, code: i32) {
        let file = self
            .paths
            .launchctl
            .with_file_name(format!("launchctl.{verb}.exit"));
        fs::write(file, code.to_string()).expect("the exit file");
    }

    /// The two calls of a load.
    fn load_calls(&self) -> Vec<String> {
        vec![
            format!("bootout {JOB}"),
            format!("bootstrap system {}", self.paths.plist.display()),
        ]
    }

    /// Writes the script into a directory of its own, mode 0755, and runs it as the installer
    /// does.
    fn install(&self) -> Output {
        let scripts = self.scratch.path().join("scripts");
        fs::create_dir_all(&scripts).expect("the scripts directory");
        let script = scripts.join("postinstall");
        fs::write(&script, self.paths.script()).expect("the script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("its mode");
        let pkg = self.scratch.path().join("hostkeeper.pkg");
        Command::new(&script)
            .args([pkg.as_os_str(), "/".as_ref(), "/".as_ref(), "/".as_ref()])
            .current_dir(&scripts)
            .env_clear()
            .output()
            .expect("run the postinstall")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Catches a postinstall that does not run, exits other than 0 on a first install, or leaves the
/// job's program starting nothing or another binary than the one installed; one that overwrites
/// or reads past a config that is there, or does not boot out and load the job; and one that,
/// run again (the pkg installed again), moves `current` or does not load again.
#[test]
fn the_postinstall_seeds_keeps_the_config_and_loads_the_job_and_a_second_run_moves_nothing() {
    let mac = Mac::new("postinstall-first");
    mac.put_config(b"the operator's config");
    assert!(!mac.paths.state_dir.exists());
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), format!("{}\n", kept_line(&mac.paths.config)));
    assert_eq!(
        fs::read(&mac.paths.config).expect("config"),
        b"the operator's config"
    );
    let version = env!("CARGO_PKG_VERSION");
    let current = mac.current();
    assert_eq!(
        fs::read_link(&current).expect("current is a link"),
        Path::new("versions").join(version)
    );
    assert_eq!(
        fs::read(&current).expect("current starts a file"),
        fs::read(&mac.paths.binary).expect("the installed binary")
    );
    // What launchd starts runs: it answers as this binary does.
    let probe = Command::new(&current)
        .arg("self-update-protocol")
        .env_clear()
        .output()
        .expect("run current");
    assert!(probe.status.success(), "{probe:?}");
    assert_eq!(String::from_utf8_lossy(&probe.stdout), protocol_line());
    assert_eq!(mac.calls(), mac.load_calls());

    let again = mac.install();
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        stderr(&again),
        format!("{}\n", kept_line(&mac.paths.config))
    );
    assert_eq!(
        fs::read_link(&current).expect("current is a link"),
        Path::new("versions").join(version)
    );
    let twice: Vec<String> = mac.load_calls().into_iter().cycle().take(4).collect();
    assert_eq!(mac.calls(), twice);
}

/// Catches a postinstall that reports success while a daemon holds the state directory's lock
/// and `current` starts nothing: the install fails with the seed's status and message, nothing is
/// seeded, no config is written and the job is not loaded.
#[test]
fn a_held_lock_fails_the_postinstall_before_the_config_and_the_load() {
    let mac = Mac::new("postinstall-locked");
    let state = &mac.paths.state_dir;
    fs::create_dir(state).expect("the state directory");
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).expect("its mode");
    let lock = fs::File::create(state.join(LOCK_FILE)).expect("the lock file");
    lock.try_lock().expect("hold the lock as a daemon does");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_LOCKED)), "{out:?}");
    assert!(stderr(&out).starts_with(SEED_ERROR), "{}", stderr(&out));
    assert!(fs::symlink_metadata(mac.current()).is_err());
    assert!(!mac.paths.config.exists());
    assert_eq!(mac.calls(), Vec::<String>::new());
}

/// Catches a postinstall that reports success when the seed cannot write the state directory (a
/// file stands in its place): the install fails with the seed's status and message.
#[test]
fn a_state_directory_that_cannot_be_made_fails_the_postinstall() {
    let mac = Mac::new("postinstall-unwritable");
    fs::write(&mac.paths.state_dir, b"a file in the way").expect("a file in the way");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILED)), "{out:?}");
    assert!(stderr(&out).starts_with(SEED_ERROR), "{}", stderr(&out));
    assert_eq!(mac.calls(), Vec::<String>::new());
}

/// Catches a postinstall that loads the job when there is neither a managed config nor a config:
/// the install fails with `mac-config`'s status and message, and `launchctl` never runs.
#[test]
fn neither_a_managed_config_nor_a_config_fails_the_postinstall_before_the_load() {
    let mac = Mac::new("postinstall-neither");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILED)), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "hostkeeper mac-config: neither a managed config ({}) nor a config ({}) is there\n",
            mac.paths.managed_config.display(),
            mac.paths.config.display()
        )
    );
    assert!(!mac.paths.config.exists());
    assert_eq!(mac.calls(), Vec::<String>::new());
}

/// Catches a postinstall that writes a config from a managed file root does not own (the script
/// trusts uid 0, and here the test user owns the file): the install fails with the refusal,
/// nothing is written and `launchctl` never runs.
#[test]
fn a_managed_file_that_root_does_not_own_fails_the_postinstall() {
    let me = rustix::process::geteuid().as_raw();
    if me == 0 {
        eprintln!("skipped: running as root, so the test user's file passes the uid 0 check");
        return;
    }
    let mac = Mac::new("postinstall-untrusted");
    let managed = &mac.paths.managed_config;
    fs::create_dir(managed.parent().expect("a parent")).expect("the directory");
    fs::write(managed, b"not read").expect("the managed file");
    fs::set_permissions(managed, fs::Permissions::from_mode(0o644)).expect("its mode");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILED)), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "hostkeeper mac-config: the managed config {} is refused: owned by uid {me}, not the trusted uid 0\n",
            managed.display()
        )
    );
    assert!(!mac.paths.config.exists());
    assert!(!mac.paths.config.parent().expect("a parent").exists());
    assert_eq!(mac.calls(), Vec::<String>::new());
}

/// Catches a postinstall that stops at the boot-out's failure (which is how a first install
/// looks, nothing being loaded): the bootstrap still runs and the install succeeds.
#[test]
fn a_failing_bootout_does_not_stop_the_bootstrap() {
    let mac = Mac::new("postinstall-bootout-fails");
    mac.put_config(b"config");
    mac.fail("bootout", 3);
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), format!("{}\n", kept_line(&mac.paths.config)));
    assert_eq!(mac.calls(), mac.load_calls());
}

/// Catches a postinstall that reports success when `launchctl bootstrap` fails: the install fails
/// with its status and the message it wrote.
#[test]
fn a_failing_bootstrap_fails_the_postinstall_with_its_status_and_message() {
    let mac = Mac::new("postinstall-bootstrap-fails");
    mac.put_config(b"config");
    mac.fail("bootstrap", 5);
    let out = mac.install();
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "{}\nlaunchctl fixture: bootstrap failed\n",
            kept_line(&mac.paths.config)
        )
    );
    assert_eq!(mac.calls(), mac.load_calls());
}
