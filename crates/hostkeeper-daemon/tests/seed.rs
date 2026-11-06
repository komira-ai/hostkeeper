//! `hostkeeper self-update-seed`: the first binary of the binary directory the daemon's unit
//! starts `current` of, put in before the first start and never over a `current` that starts a
//! binary.

mod support;

use std::ffi::OsStr;
use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::cli::{self, Cli, EXIT_FAILED, EXIT_LOCKED};
use hostkeeper_daemon::seed::{SeedError, Seeded, seed};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start::LOCK_FILE;
use hostkeeper_steps::Layout;
use support::{Scratch, snapshot};

/// The version the seed is asked to put the binary in as.
const OWN: &str = "1.0.0";

/// A scratch host: its state directory, and the binary its package installed, mode 0755.
struct Host {
    scratch: Scratch,
    state: PathBuf,
    binary: PathBuf,
}

impl Host {
    fn new(name: &str) -> Host {
        let scratch = Scratch::new(name);
        let state = scratch.path().join("state");
        let binary = scratch.path().join("hostkeeper");
        fs::write(&binary, b"the installed binary").expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
        Host {
            scratch,
            state,
            binary,
        }
    }

    fn layout(&self) -> Layout {
        Layout::new(&binary_dir(&self.state))
    }

    fn seed(&self) -> Result<Seeded, SeedError> {
        seed(&self.state, &self.binary, OWN)
    }

    /// The bytes `current` starts.
    fn started(&self) -> Vec<u8> {
        fs::read(self.layout().current_path()).expect("current starts a file")
    }

    /// Puts `bytes` in as version `version`, executable, and points `current` at it.
    fn switched(&self, version: &str, bytes: &[u8]) {
        let source = self.scratch.path().join(format!("source-{version}"));
        fs::write(&source, bytes).expect("a binary");
        let layout = self.layout();
        layout.stage(version, &source).expect("staged");
        layout.switch(version).expect("switched");
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// Catches a first start with nothing to start, a seed that leaves the binary directory open to
/// other accounts, and one that writes again (or moves `current`) on every later start: the first
/// seed puts the installed binary in as this version, executable, `current` naming it, in
/// directories of mode 0700; the second finds `current` starting it and writes nothing.
#[test]
fn the_first_seed_puts_the_installed_binary_in_current_and_the_next_writes_nothing() {
    let host = Host::new("seed-first");
    assert_eq!(host.seed().expect("seeded"), Seeded::Seeded);
    let layout = host.layout();
    assert_eq!(layout.current().expect("current").as_deref(), Some(OWN));
    assert_eq!(layout.previous().expect("previous"), None);
    assert_eq!(host.started(), b"the installed binary");
    assert_eq!(mode(&layout.version_path(OWN)), 0o755);
    assert_eq!(mode(layout.dir()), 0o700);
    assert_eq!(mode(&host.state.join("hostkeeper")), 0o700);

    let before = snapshot(host.scratch.path());
    assert_eq!(host.seed().expect("in place"), Seeded::InPlace);
    assert_eq!(snapshot(host.scratch.path()), before);
}

/// Catches a seed that takes the binary directory back from hostkeeper's own update: once an
/// update switched `current` to another version, the seed with the installed (older) binary
/// writes nothing and `current` still starts the update's binary.
#[test]
fn the_seed_never_moves_a_current_the_update_switched() {
    let host = Host::new("seed-updated");
    host.seed().expect("seeded");
    host.switched("2.0.0", b"the updated binary");
    let before = snapshot(host.scratch.path());
    assert_eq!(host.seed().expect("in place"), Seeded::InPlace);
    assert_eq!(snapshot(host.scratch.path()), before);
    assert_eq!(host.started(), b"the updated binary");
}

/// Catches a binary directory left with a `current` that starts nothing (its version's file
/// gone, or not executable), which would fail every start: the seed removes it and puts the
/// installed binary in, whatever version the broken link named, its own version's included; a
/// `previous` is left as it is.
#[test]
fn the_seed_repairs_a_current_that_starts_nothing() {
    for (case, named) in [("other", "9.9.9"), ("own", OWN)] {
        let host = Host::new(&format!("seed-dangling-{case}"));
        host.switched(named, b"a binary that went");
        fs::remove_file(host.layout().version_path(named)).expect("its file is gone");
        assert_eq!(host.seed().expect("repaired"), Seeded::Seeded, "{case}");
        assert_eq!(host.started(), b"the installed binary", "{case}");
    }

    let host = Host::new("seed-not-executable");
    host.switched("9.9.9", b"not a program");
    let layout = host.layout();
    fs::set_permissions(
        layout.version_path("9.9.9"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("no execute bit");
    host.switched("8.8.8", b"the previous binary");
    fs::set_permissions(
        layout.version_path("8.8.8"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("no execute bit");
    assert_eq!(host.seed().expect("repaired"), Seeded::Seeded);
    assert_eq!(host.started(), b"the installed binary");
    assert_eq!(
        layout.previous().expect("previous").as_deref(),
        Some("9.9.9")
    );
}

/// Catches a seed that a crash between staging and the switch leaves unfinished for good: with
/// the version staged and no `current`, the next seed points `current` at it.
#[test]
fn a_seed_cut_short_after_staging_is_finished_by_the_next() {
    let host = Host::new("seed-crash");
    let layout = host.layout();
    fs::create_dir_all(host.state.join("hostkeeper")).expect("the state of hostkeeper");
    layout.stage(OWN, &host.binary).expect("staged");
    assert_eq!(layout.current().expect("current"), None);
    assert_eq!(host.seed().expect("finished"), Seeded::Seeded);
    assert_eq!(layout.current().expect("current").as_deref(), Some(OWN));
    assert_eq!(host.started(), b"the installed binary");
}

/// Catches a seed that writes under a running daemon, or that cannot say why it seeded nothing:
/// with the state directory locked, it writes nothing and exits with the status of a locked
/// start; with no binary to copy, or a `previous` that names this version with its file gone
/// (which staging refuses to write over), it fails naming the path; and on the command line each
/// is the exit status the unit sees.
#[test]
fn a_seed_that_cannot_run_writes_nothing_and_says_why() {
    let host = Host::new("seed-locked");
    fs::create_dir_all(&host.state).expect("the state directory");
    let held = File::create(host.state.join(LOCK_FILE)).expect("the lock file");
    held.try_lock().expect("hold the lock");
    let error = host.seed().expect_err("locked");
    assert!(matches!(error, SeedError::Lock(_)), "{error:?}");
    assert_eq!(error.exit_code(), EXIT_LOCKED);
    assert!(!host.state.join("hostkeeper").exists());
    let run = |host: &Host| {
        let args: [&OsStr; 6] = [
            "hostkeeper".as_ref(),
            "self-update-seed".as_ref(),
            "--state-dir".as_ref(),
            host.state.as_os_str(),
            "--binary".as_ref(),
            host.binary.as_os_str(),
        ];
        format!("{:?}", cli::run(Cli::parse_from(args)))
    };
    assert_eq!(run(&host), format!("{:?}", ExitCode::from(EXIT_LOCKED)));
    drop(held);

    let host = Host::new("seed-no-binary");
    fs::remove_file(&host.binary).expect("no binary");
    let error = host.seed().expect_err("nothing to copy");
    assert_eq!(error.exit_code(), EXIT_FAILED);
    let staged = host.layout().version_path(OWN);
    assert!(
        error
            .to_string()
            .starts_with(&format!("{}: ", staged.display())),
        "{error}"
    );
    assert_eq!(host.layout().current().expect("current"), None);
    assert_eq!(run(&host), format!("{:?}", ExitCode::from(EXIT_FAILED)));

    let host = Host::new("seed-previous-gone");
    host.switched(OWN, b"the old binary");
    host.switched("2.0.0", b"the updated binary");
    let layout = host.layout();
    fs::remove_file(layout.version_path("2.0.0")).expect("current's file is gone");
    fs::remove_file(layout.version_path(OWN)).expect("previous's file is gone");
    let error = host.seed().expect_err("previous names this version");
    assert!(matches!(error, SeedError::Io { .. }), "{error:?}");
    assert_eq!(
        layout.current().expect("current"),
        None,
        "the broken link is gone"
    );
}

/// Catches the verb not reaching the seed: on the command line it seeds and exits 0, and a
/// symlinked `current` that starts the binary already is left.
#[test]
fn the_verb_seeds_and_exits_zero() {
    let host = Host::new("seed-verb");
    let args: [&OsStr; 6] = [
        "hostkeeper".as_ref(),
        "self-update-seed".as_ref(),
        "--state-dir".as_ref(),
        host.state.as_os_str(),
        "--binary".as_ref(),
        host.binary.as_os_str(),
    ];
    let code = cli::run(Cli::parse_from(args));
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    assert_eq!(host.started(), b"the installed binary");
    // A version named by a link of its own to an executable file starts too.
    let other = host.scratch.path().join("elsewhere");
    fs::write(&other, b"x").expect("a file");
    fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).expect("its mode");
    let layout = host.layout();
    fs::remove_file(layout.current_path()).expect("unlink current");
    symlink(&other, layout.current_path()).expect("link current elsewhere");
    assert_eq!(host.seed().expect("in place"), Seeded::InPlace);
}
