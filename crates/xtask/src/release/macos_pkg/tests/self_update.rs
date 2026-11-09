//! The pkg as hostkeeper's own update on a Mac relies on it (design sections 2.1 and 6.6), held to
//! the daemon's own code: its plist starts the binary the pkg installs with `daemon`, one of the
//! paths the daemon's start knows as packaged (`hostkeeper_daemon::packaged`), which seeds the
//! binary directory the daemon derives from its state directory
//! (`hostkeeper_daemon::selfupdate::binary_dir`) and execs `current` of it; and the postinstall
//! runs no seed.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;

use clap::Parser;
use hostkeeper_daemon::cli::{Cli, Command, DEFAULT_STATE_DIR};
use hostkeeper_daemon::packaged::{PACKAGED_BINARIES, PackagedError, start};
use hostkeeper_daemon::selfupdate::binary_dir;
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

/// The binary directory the plist the pkg ships makes the daemon exec `current` of: the one the
/// daemon derives from the state directory of the `daemon` command the plist runs.
fn started_dir() -> PathBuf {
    let args = program_arguments(&plist_text().expect("the plist"));
    let Command::Daemon(flags) = parsed(&args);
    binary_dir(&flags.state_dir)
}

#[test]
fn the_shipped_plist_starts_the_installed_binary_the_daemons_start_knows_as_packaged() {
    // The design's named test for the macOS start path. Catches: a job that starts `current` of
    // the binary directory itself (what the release before this one shipped), so the first start
    // finds no binary before a seed; a job whose program is none of the packaged paths, so the
    // start neither seeds nor execs `current` and the update falls back at every release; a job
    // whose program is not the path the payload installs the binary at; and a job whose
    // arguments the daemon would not run as the daemon with its default state directory.
    let args = program_arguments(&plist_text().expect("the plist"));
    let Command::Daemon(flags) = parsed(&args);
    assert_eq!(flags.state_dir, Path::new(DEFAULT_STATE_DIR));
    assert_eq!(DAEMON_STATE_DIR, DEFAULT_STATE_DIR);
    assert_eq!(args[0], INSTALLED_BINARY);
    assert_eq!(shown(&binary_path()), INSTALLED_BINARY);
    assert!(PACKAGED_BINARIES.contains(&args[0].as_str()));
}

#[test]
fn the_job_programs_start_seeds_and_runs_current_of_the_directory_of_its_state_dir() {
    // Catches: a packaged start that does not give the job's first start a binary (the seed puts
    // the image in as `current`), that execs anything but `current` of the binary directory of
    // the state directory with the job's own arguments, or that execs a second time once the
    // binary runs from `current`. The image is a scratch file declared packaged, as the pkg's
    // binary is on a Mac, and the exec is a fake that records its call.
    let dir = scratch("macos-pkg-packaged-start");
    let state = dir.join("state");
    let image = dir.join("hostkeeper");
    std::fs::write(&image, b"the binary the pkg installed").expect("the binary");
    std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    let args: Vec<OsString> = ["daemon", "--state-dir"]
        .iter()
        .map(OsString::from)
        .chain([state.clone().into_os_string()])
        .collect();
    let called = RefCell::new(Vec::new());
    let exec = |path: &Path, args: &[OsString]| {
        called
            .borrow_mut()
            .push((path.to_path_buf(), args.to_vec()));
        std::io::Error::other("fake exec")
    };
    let error =
        start(&image, &[image.as_path()], &state, "0.1.5", &args, &exec).expect("a packaged start");
    let current = binary_dir(&state).join("current");
    assert!(matches!(error, PackagedError::Exec { ref path, .. } if *path == current));
    assert_eq!(*called.borrow(), [(current.clone(), args.clone())]);
    assert_eq!(
        std::fs::read(&current).expect("current"),
        b"the binary the pkg installed"
    );
    // The same state directory as the job's, derived the way the job's plist derives it.
    assert_eq!(binary_dir(Path::new(DEFAULT_STATE_DIR)), started_dir());
    // Run from `current`, the binary is none of the packaged paths: nothing is exec'd.
    called.borrow_mut().clear();
    assert!(start(&current, &[image.as_path()], &state, "0.1.5", &args, &exec).is_none());
    assert!(called.borrow().is_empty());
}

#[test]
fn the_postinstall_runs_no_seed_and_no_mac_config_of_the_installed_binary() {
    // Catches: a postinstall that still runs the installed binary at install (the seed and the
    // mac-config the daemon's start does now), which would run before the managed preferences are
    // there and fail the install, or one that names the installed binary or a state directory.
    let text = postinstall_text();
    for word in [
        "self-update-seed",
        "mac-config",
        INSTALLED_BINARY,
        DEFAULT_STATE_DIR,
    ] {
        assert!(!text.contains(word), "{word} in {text}");
    }
    let code: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    assert_eq!(code.len(), 2, "{code:?}");
    assert!(code[0].contains("bootout"));
    assert!(code[1].starts_with("exec ") && code[1].contains("bootstrap system"));
    assert!(code[1].contains(&shown(&plist_path())));
}
