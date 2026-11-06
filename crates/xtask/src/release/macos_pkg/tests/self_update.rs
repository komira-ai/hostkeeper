//! The pkg as hostkeeper's own update on a Mac relies on it (design section 6.6), held to the
//! daemon's own code: its plist starts `current` of the binary directory the daemon derives from
//! its state directory (`hostkeeper_daemon::selfupdate::binary_dir`), the directory a self-update
//! switches; the fallback job the daemon writes names that same directory; and the postinstall's
//! seed fills it from the binary the pkg installs, so the job's first start finds a binary.

use std::os::unix::fs::PermissionsExt;

use clap::Parser;
use hostkeeper_daemon::cli::{Cli, Command, DEFAULT_STATE_DIR};
use hostkeeper_daemon::seed::{Seeded, seed};
use hostkeeper_daemon::selfunits::unit_files;
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_launchd::plist::{DAEMON_STATE_DIR, INSTALLED_BINARY};

use super::*;

/// The `ProgramArguments` of the launchd plist `text`.
fn program_arguments(text: &str) -> Vec<String> {
    let value = plist::Value::from_reader_xml(text.as_bytes()).expect("the plist parses");
    value
        .as_dictionary()
        .and_then(|dict| dict.get("ProgramArguments"))
        .and_then(plist::Value::as_array)
        .expect("ProgramArguments")
        .iter()
        .map(|arg| arg.as_string().expect("a string").to_owned())
        .collect()
}

/// A command line as the binary parses it, its own name in place of the program's path.
fn parsed(words: &[String]) -> Command {
    let args = std::iter::once("hostkeeper").chain(words.iter().skip(1).map(String::as_str));
    Cli::try_parse_from(args)
        .expect("the binary parses it")
        .command
}

/// The binary directory the plist the pkg ships starts `current` of: the directory of its
/// program, checked to be named `current`.
fn started_dir() -> PathBuf {
    let args = program_arguments(&plist_text().expect("the plist"));
    let program = Path::new(&args[0]);
    assert_eq!(program.file_name(), Some(OsStr::new("current")));
    program.parent().expect("a directory").to_path_buf()
}

#[test]
fn the_shipped_plist_starts_current_of_the_binary_directory_the_daemon_derives() {
    // The design's named test for the macOS start path. Catches: a job that starts the binary the
    // pkg installs (`/usr/local/bin/hostkeeper`, what every release before this one shipped), so
    // the clean exit after a self-update starts the binary that ran and the update falls back at
    // every release; a job whose program is `current` of another state directory than the one the
    // daemon it runs keeps; and a job whose arguments the daemon would not run as the daemon.
    let args = program_arguments(&plist_text().expect("the plist"));
    let Command::Daemon(flags) = parsed(&args) else {
        panic!("the plist runs {args:?}, not the daemon");
    };
    assert_eq!(flags.state_dir, Path::new(DEFAULT_STATE_DIR));
    assert_eq!(DAEMON_STATE_DIR, DEFAULT_STATE_DIR);
    assert_eq!(
        Path::new(&args[0]),
        binary_dir(&flags.state_dir).join("current")
    );
    assert_ne!(args[0], INSTALLED_BINARY);
}

#[test]
fn the_fallback_job_runs_previous_of_the_directory_the_daemon_job_starts_current_of() {
    // Catches: the daemon's fallback job and the pkg's job disagreeing on the binary directory, so
    // the fallback flips back a `current` the daemon's job does not start, or runs a `previous`
    // of a directory no self-update ever wrote.
    let dir = started_dir();
    let mac = Launchd::macos();
    let files = unit_files(
        PlatformKind::Macos,
        &mac.unit_dir,
        Path::new(DEFAULT_STATE_DIR),
    );
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].dir, mac.unit_dir);
    let shown = dir.display().to_string();
    assert_eq!(
        program_arguments(&files[0].text),
        [
            format!("{shown}/previous"),
            "self-update-fallback".to_owned(),
            "--dir".to_owned(),
            shown,
        ]
    );
    let Command::SelfUpdateFallback(flags) = parsed(&program_arguments(&files[0].text)) else {
        panic!("the fallback job does not run self-update-fallback");
    };
    assert_eq!(flags.dir, dir);
}

/// The words of the one command of the postinstall, `exec` dropped.
fn postinstall_words() -> Vec<String> {
    let text = postinstall_text();
    let line = text.lines().last().expect("a command");
    let line = line
        .strip_prefix("exec ")
        .expect("the script execs the seed");
    line.split(' ').map(str::to_owned).collect()
}

#[test]
fn the_postinstall_seeds_the_directory_the_job_starts_current_of_from_the_installed_binary() {
    // Catches: a postinstall that seeds another state directory than the daemon's (the job would
    // find no `current` and launchd could not start it), that is run by or seeds another binary
    // than the one the payload installs, or whose line the binary would not run as the seed.
    let words = postinstall_words();
    assert_eq!(words[0], INSTALLED_BINARY);
    assert_eq!(shown(&binary_path()), INSTALLED_BINARY);
    let Command::SelfUpdateSeed(flags) = parsed(&words) else {
        panic!("the postinstall runs {words:?}, not the seed");
    };
    assert_eq!(flags.binary, Path::new(INSTALLED_BINARY));
    assert_eq!(binary_dir(&flags.state_dir), started_dir());
}

#[test]
fn the_postinstalls_seed_puts_in_the_binary_the_job_starts_and_a_reinstall_keeps_a_self_update() {
    // Catches: a first install after which the job's program starts nothing or another binary
    // than the one installed, and a reinstall of the pkg that puts its binary back over the one a
    // self-update switched to. The seed runs as the postinstall runs it, its state directory
    // moved under a scratch directory.
    let dir = scratch("macos-pkg-postinstall-seed");
    let state = dir.join("state");
    let installed = dir.join("hostkeeper");
    std::fs::write(&installed, b"the binary the pkg installed").expect("the binary");
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    let program = binary_dir(&state).join("current");
    assert!(!program.exists());
    assert_eq!(
        seed(&state, &installed, "0.1.5").expect("seed"),
        Seeded::Seeded
    );
    assert_eq!(
        std::fs::read(&program).expect("the job's program"),
        b"the binary the pkg installed"
    );
    let mode = std::fs::metadata(&program)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "executable: {mode:o}");
    // A self-update stages its binary and switches `current` to it.
    let layout = hostkeeper_steps::Layout::new(&binary_dir(&state));
    let update = dir.join("update");
    std::fs::write(&update, b"the binary of 0.1.6").expect("the update");
    std::fs::set_permissions(&update, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    layout.stage("0.1.6", &update).expect("stage");
    layout.switch("0.1.6").expect("switch");
    // The pkg installed again, of its older version: the job still starts the update's binary.
    assert_eq!(
        seed(&state, &installed, "0.1.5").expect("seed"),
        Seeded::InPlace
    );
    assert_eq!(
        std::fs::read(&program).expect("the job's program"),
        b"the binary of 0.1.6"
    );
}
