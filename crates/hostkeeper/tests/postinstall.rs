//! The macOS pkg's `postinstall` (`hostkeeper_launchd::postinstall`), run as the installer runs
//! it: an executable file in its own directory, started with the path of the pkg, the install
//! location, the target volume and the root of the volume as arguments, and an empty
//! environment. It runs this binary's `self-update-seed`, here with a binary and a state
//! directory in a scratch directory, each path holding a space so the script's quoting is
//! exercised by the real shell; on a Mac the pkg names `/usr/local/bin/hostkeeper` and
//! `/var/lib/updater/state`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_daemon::cli::{EXIT_FAILED, EXIT_LOCKED};
use hostkeeper_daemon::selfupdate::{binary_dir, protocol_line};
use hostkeeper_daemon::start::LOCK_FILE;
use hostkeeper_launchd::postinstall::postinstall;
use hostkeeper_testkit::Scratch;

/// What the seed prefixes its error with.
const SEED_ERROR: &str = "hostkeeper self-update-seed: ";

/// A Mac in a scratch directory: the binary the pkg installed (a copy of this crate's binary)
/// and the daemon's state directory, neither of which exists until [`Mac::new`] or the script
/// makes it.
struct Mac {
    scratch: Scratch,
    binary: PathBuf,
    state: PathBuf,
}

impl Mac {
    fn new(name: &str) -> Mac {
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir");
        let bin_dir = scratch.path().join("usr local");
        fs::create_dir(&bin_dir).expect("the binary's directory");
        let binary = bin_dir.join("hostkeeper");
        fs::copy(env!("CARGO_BIN_EXE_hostkeeper"), &binary).expect("install the binary");
        let state = scratch.path().join("updater state");
        Mac {
            scratch,
            binary,
            state,
        }
    }

    /// `current` of the binary directory: the program of the daemon's launchd job.
    fn current(&self) -> PathBuf {
        binary_dir(&self.state).join("current")
    }

    /// Writes the script into a directory of its own, mode 0755, and runs it as the installer
    /// does.
    fn install(&self) -> Output {
        let scripts = self.scratch.path().join("scripts");
        fs::create_dir_all(&scripts).expect("the scripts directory");
        let script = scripts.join("postinstall");
        fs::write(&script, postinstall(&self.binary, &self.state)).expect("the script");
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

/// Catches a postinstall that does not run, exits other than 0 on a first install, or leaves
/// the job's program starting nothing or another binary than the one installed; and one that,
/// run again (the pkg installed again), moves `current`.
#[test]
fn the_postinstall_points_current_at_the_installed_binary_and_a_second_run_moves_nothing() {
    let mac = Mac::new("postinstall-first");
    assert!(!mac.state.exists());
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    let version = env!("CARGO_PKG_VERSION");
    let current = mac.current();
    assert_eq!(
        fs::read_link(&current).expect("current is a link"),
        Path::new("versions").join(version)
    );
    assert_eq!(
        fs::read(&current).expect("current starts a file"),
        fs::read(&mac.binary).expect("the installed binary")
    );
    // What launchd starts runs: it answers as this binary does.
    let probe = Command::new(&current)
        .arg("self-update-protocol")
        .env_clear()
        .output()
        .expect("run current");
    assert!(probe.status.success(), "{probe:?}");
    assert_eq!(String::from_utf8_lossy(&probe.stdout), protocol_line());

    let again = mac.install();
    assert!(again.status.success(), "{again:?}");
    assert_eq!(stderr(&again), "");
    assert_eq!(
        fs::read_link(&current).expect("current is a link"),
        Path::new("versions").join(version)
    );
}

/// Catches a postinstall that reports success while a daemon holds the state directory's lock
/// and `current` starts nothing: the install fails with the seed's status and message, and
/// nothing is seeded.
#[test]
fn a_held_lock_fails_the_postinstall_and_seeds_nothing() {
    let mac = Mac::new("postinstall-locked");
    fs::create_dir(&mac.state).expect("the state directory");
    fs::set_permissions(&mac.state, fs::Permissions::from_mode(0o700)).expect("its mode");
    let lock = fs::File::create(mac.state.join(LOCK_FILE)).expect("the lock file");
    lock.try_lock().expect("hold the lock as a daemon does");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_LOCKED)), "{out:?}");
    assert!(stderr(&out).starts_with(SEED_ERROR), "{}", stderr(&out));
    assert!(fs::symlink_metadata(mac.current()).is_err());
}

/// Catches a postinstall that reports success when the seed cannot write the state directory
/// (a file stands in its place): the install fails with the seed's status and message.
#[test]
fn a_state_directory_that_cannot_be_made_fails_the_postinstall() {
    let mac = Mac::new("postinstall-unwritable");
    fs::write(&mac.state, b"a file in the way").expect("a file in the way");
    let out = mac.install();
    assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILED)), "{out:?}");
    assert!(stderr(&out).starts_with(SEED_ERROR), "{}", stderr(&out));
}
