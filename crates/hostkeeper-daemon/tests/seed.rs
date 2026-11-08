//! The first binary of the binary directory the daemon's unit starts `current` of: given by the
//! start of `hostkeeper daemon` from the packaged binary, which then execs `current`
//! ([`hostkeeper_daemon::packaged`], docs/design/two-programs.md, section 2.1), and by the verb
//! `hostkeeper self-update-seed` the units and the pkg still run. Either puts it in before the
//! first start and never over a `current` that starts a binary.

mod support;

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::cli::{self, Cli, EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED, SystemdVars};
use hostkeeper_daemon::packaged::{self, PACKAGED_BINARIES, PackagedError};
use hostkeeper_daemon::seed::{SeedError, Seeded, seed};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start::LOCK_FILE;
use hostkeeper_steps::Layout;
use support::{Scratch, snapshot};

/// The version the start puts the packaged binary in as: this build's.
const OWN: &str = env!("CARGO_PKG_VERSION");

/// What the exec of a test answers in place of running `current`: an exec returns only when it
/// failed.
const NOT_RUN: &str = "the test does not run current";

/// The arguments of the daemon the start is given, after the program's name.
fn daemon_args(state: &Path) -> Vec<OsString> {
    ["daemon", "--state-dir"]
        .map(OsString::from)
        .into_iter()
        .chain([state.as_os_str().to_owned(), "--notify-systemd".into()])
        .collect()
}

/// A scratch host: its state directory, and the binary its package installed, mode 0755, which
/// is the image of the packaged start.
struct Host {
    scratch: Scratch,
    state: PathBuf,
    binary: PathBuf,
}

/// How a start from the packaged binary ended: what it returned, and every exec it asked for.
struct Started {
    error: Option<PackagedError>,
    execs: Vec<(PathBuf, Vec<OsString>)>,
}

impl Started {
    /// The start reached `current`: it asked for one exec of it, with the daemon's arguments,
    /// and returned only what that exec answered.
    fn execed(&self, host: &Host) -> bool {
        let exec = matches!(&self.error, Some(PackagedError::Exec { path, source })
            if (*path == host.layout().current_path()) & (source.to_string() == NOT_RUN));
        exec & (self.execs == [(host.layout().current_path(), daemon_args(&host.state))])
    }
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

    /// `hostkeeper daemon` started from the packaged binary: the host's binary is the image and
    /// the one packaged path, and the exec is recorded and answers [`NOT_RUN`].
    fn start(&self) -> Started {
        let execs = RefCell::new(Vec::new());
        let exec = |path: &Path, args: &[OsString]| {
            execs.borrow_mut().push((path.to_path_buf(), args.to_vec()));
            io::Error::other(NOT_RUN)
        };
        let error = packaged::start(
            &self.binary,
            &[&self.binary],
            &self.state,
            OWN,
            &daemon_args(&self.state),
            None,
            &exec,
        );
        Started {
            error,
            execs: execs.into_inner(),
        }
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
/// other accounts, a start that writes again (or moves `current`) on every later start, and one
/// that runs as the daemon itself instead of handing over to `current`: the first start puts the
/// installed binary in as this version, executable, `current` naming it, in directories of mode
/// 0700, and execs `current` with its own arguments; the second finds `current` starting it,
/// writes nothing (in place) and execs it again.
#[test]
fn the_first_start_puts_the_installed_binary_in_current_and_the_next_writes_nothing() {
    let host = Host::new("seed-first");
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
    let layout = host.layout();
    assert_eq!(layout.current().expect("current").as_deref(), Some(OWN));
    assert_eq!(layout.previous().expect("previous"), None);
    assert_eq!(host.started(), b"the installed binary");
    assert_eq!(mode(&layout.version_path(OWN)), 0o755);
    assert_eq!(mode(layout.dir()), 0o700);
    assert_eq!(mode(&host.state.join("hostkeeper")), 0o700);

    let before = snapshot(host.scratch.path());
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
    assert_eq!(snapshot(host.scratch.path()), before);
}

/// Catches a start that takes the binary directory back from hostkeeper's own update: once an
/// update switched `current` to another version, the start from the installed (older) binary
/// writes nothing and execs `current`, which still starts the update's binary.
#[test]
fn the_start_never_moves_a_current_the_update_switched() {
    let host = Host::new("seed-updated");
    host.start();
    host.switched("2.0.0", b"the updated binary");
    let before = snapshot(host.scratch.path());
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
    assert_eq!(snapshot(host.scratch.path()), before);
    assert_eq!(host.started(), b"the updated binary");
}

/// Catches a binary directory left with a `current` that starts nothing (its version's file
/// gone, or not executable), which would fail every start: the start removes it, puts the
/// installed binary in, whatever version the broken link named, its own version's included, and
/// execs it; a `previous` is left as it is.
#[test]
fn the_start_repairs_a_dangling_current() {
    for (case, named) in [("other", "9.9.9"), ("own", OWN)] {
        let host = Host::new(&format!("seed-dangling-{case}"));
        host.switched(named, b"a binary that went");
        fs::remove_file(host.layout().version_path(named)).expect("its file is gone");
        let started = host.start();
        assert!(started.execed(&host), "{case}: {:?}", started.error);
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
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
    assert_eq!(host.started(), b"the installed binary");
    assert_eq!(
        layout.previous().expect("previous").as_deref(),
        Some("9.9.9")
    );
}

/// Catches a start that a crash between staging and the switch leaves unfinished for good: with
/// the version staged and no `current`, the next start points `current` at it and execs it.
#[test]
fn a_start_cut_short_after_staging_is_finished_by_the_next() {
    let host = Host::new("seed-crash");
    let layout = host.layout();
    fs::create_dir_all(host.state.join("hostkeeper")).expect("the state of hostkeeper");
    layout.stage(OWN, &host.binary).expect("staged");
    assert_eq!(layout.current().expect("current"), None);
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
    assert_eq!(layout.current().expect("current").as_deref(), Some(OWN));
    assert_eq!(host.started(), b"the installed binary");
}

/// Catches a start that writes under a running daemon, or that cannot say why it started
/// nothing, or that execs a `current` it could not repair: with the state directory locked, it
/// writes nothing, execs nothing and ends with the status of a locked start; with no binary to
/// copy, or a `previous` that names this version with its file gone (which staging refuses to
/// write over), it fails naming the path and execs nothing. The verb, which the unit still runs,
/// answers each with the same status.
#[test]
fn a_start_that_cannot_seed_writes_nothing_execs_nothing_and_says_why() {
    let host = Host::new("seed-locked");
    fs::create_dir_all(&host.state).expect("the state directory");
    let held = File::create(host.state.join(LOCK_FILE)).expect("the lock file");
    held.try_lock().expect("hold the lock");
    let started = host.start();
    let error = started.error.expect("locked");
    assert!(
        matches!(error, PackagedError::Seed(SeedError::Lock(_))),
        "{error:?}"
    );
    assert_eq!(error.exit_code(), EXIT_LOCKED);
    assert!(started.execs.is_empty());
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
    let started = host.start();
    let error = started.error.expect("nothing to copy");
    assert_eq!(error.exit_code(), EXIT_FAILED);
    assert!(
        error
            .to_string()
            .starts_with(&format!("{}: ", host.binary.display())),
        "{error}"
    );
    assert!(started.execs.is_empty());
    assert_eq!(host.layout().current().expect("current"), None);
    assert!(!binary_dir(&host.state).exists(), "nothing was made");
    assert_eq!(run(&host), format!("{:?}", ExitCode::from(EXIT_FAILED)));

    let host = Host::new("seed-previous-gone");
    host.switched(OWN, b"the old binary");
    host.switched("2.0.0", b"the updated binary");
    let layout = host.layout();
    fs::remove_file(layout.version_path("2.0.0")).expect("current's file is gone");
    fs::remove_file(layout.version_path(OWN)).expect("previous's file is gone");
    let started = host.start();
    let error = started.error.expect("previous names this version");
    assert!(
        matches!(error, PackagedError::Seed(SeedError::Io { .. })),
        "{error:?}"
    );
    assert_eq!(error.exit_code(), EXIT_FAILED);
    assert!(started.execs.is_empty());
    assert_eq!(
        layout.current().expect("current"),
        None,
        "the broken link is gone"
    );
}

/// Catches a start that execs `current` from a binary that is not the packaged one, which would
/// undo a self-update's hand-over or exec in a loop: a binary run from `versions/<version>` (or
/// any other path) is none of the packaged paths, so the start seeds nothing and execs nothing
/// and the daemon starts in this process; here it then stops at its missing config, exit 3.
#[test]
fn a_binary_that_is_not_the_packaged_one_never_execs_current() {
    let host = Host::new("seed-not-packaged");
    host.switched(OWN, b"the binary of the update");
    let image = host.layout().version_path(OWN);
    // The binary directory: a Mac's daemon also adds its start to the start log beside it.
    let before = snapshot(host.layout().dir());
    let execs = RefCell::new(Vec::new());
    let exec = |path: &Path, _: &[OsString]| {
        execs.borrow_mut().push(path.to_path_buf());
        io::Error::other(NOT_RUN)
    };
    let args = daemon_args(&host.state);
    assert!(
        packaged::start(
            &image,
            &[&host.binary],
            &host.state,
            OWN,
            &args,
            None,
            &exec
        )
        .is_none(),
        "not packaged"
    );
    assert!(
        packaged::start(
            &image,
            &packaged::host_binaries(),
            &host.state,
            OWN,
            &args,
            None,
            &exec
        )
        .is_none()
    );
    let config = host.scratch.path().join("no-config.toml");
    let command_line: Vec<OsString> = ["hostkeeper", "daemon", "--config"]
        .map(OsString::from)
        .into_iter()
        .chain([config.into_os_string(), "--state-dir".into()])
        .chain([host.state.as_os_str().to_owned()])
        .collect();
    let code = cli::run_from(
        command_line,
        &image,
        &[&host.binary],
        SystemdVars::default(),
        None,
        &exec,
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(EXIT_CONFIG))
    );
    assert!(execs.borrow().is_empty());
    assert_eq!(snapshot(host.layout().dir()), before);
}

/// Catches the command line not reaching the packaged start, or another argument list or exit
/// status than the start's: `hostkeeper daemon` whose image is a packaged path seeds and execs
/// `current` with exactly the arguments after the program's name, and ends with the status of
/// what stopped it (an exec that returned is the failed start's); the packaged paths are the
/// deb's and rpm's and the pkg's. Any other verb from the packaged binary runs as it is.
#[test]
fn the_command_line_of_the_packaged_binary_reaches_the_start() {
    assert_eq!(
        PACKAGED_BINARIES,
        ["/usr/bin/hostkeeper", "/usr/local/bin/hostkeeper"]
    );
    let host = Host::new("seed-command-line");
    let execs = RefCell::new(Vec::new());
    let exec = |path: &Path, args: &[OsString]| {
        execs.borrow_mut().push((path.to_path_buf(), args.to_vec()));
        io::Error::other(NOT_RUN)
    };
    let command_line: Vec<OsString> = std::iter::once(OsString::from("hostkeeper"))
        .chain(daemon_args(&host.state))
        .collect();
    let code = cli::run_from(
        command_line,
        &host.binary,
        &[&host.binary],
        SystemdVars::default(),
        None,
        &exec,
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(EXIT_FAILED))
    );
    assert_eq!(
        execs.take(),
        [(host.layout().current_path(), daemon_args(&host.state))]
    );
    assert_eq!(host.started(), b"the installed binary");

    let seed_verb: Vec<OsString> = ["hostkeeper", "self-update-seed", "--state-dir"]
        .map(OsString::from)
        .into_iter()
        .chain([host.state.as_os_str().to_owned(), "--binary".into()])
        .chain([host.binary.as_os_str().to_owned()])
        .collect();
    let code = cli::run_from(
        seed_verb,
        &host.binary,
        &[&host.binary],
        SystemdVars::default(),
        None,
        &exec,
    );
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    assert!(execs.borrow().is_empty());
}

/// Catches an exec of the process that hides its failure: a `current` that cannot be run returns
/// the error of the exec, and the start ends with it, naming `current`.
#[test]
fn an_exec_that_fails_returns_its_error_and_the_start_names_current() {
    let host = Host::new("seed-exec-fails");
    let missing = host.scratch.path().join("no-such-binary");
    let error = packaged::exec(&missing, &[]);
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");

    let current = host.layout().current_path();
    host.switched(OWN, b"#!/no/such/interpreter\n");
    let error = packaged::start(
        &host.binary,
        &[&host.binary],
        &host.state,
        OWN,
        &[],
        None,
        &packaged::exec,
    )
    .expect("the exec returned");
    assert_eq!(error.exit_code(), EXIT_FAILED);
    let PackagedError::Exec { path, .. } = &error else {
        panic!("not the exec's error: {error:?}");
    };
    assert_eq!(*path, current);
    assert!(
        error.to_string().starts_with(&format!(
            "{} could not be run in place of the packaged binary: ",
            current.display()
        )),
        "{error}"
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
    assert_eq!(
        seed(&host.state, &host.binary, OWN).expect("in place"),
        Seeded::InPlace
    );
    let started = host.start();
    assert!(started.execed(&host), "{:?}", started.error);
}
