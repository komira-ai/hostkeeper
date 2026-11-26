//! A Mac's own automatic install of macOS updates is turned off at the start of its daemon
//! ([`hostkeeper_daemon::auto_install`]), through a scripted `defaults`.

use std::cell::RefCell;

use hostkeeper_daemon::auto_install::{self, Change, at_host_start, at_start, line};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput, PlatformKind};

const DOMAIN: &str = "/Library/Preferences/com.apple.SoftwareUpdate";
const KEY: &str = "AutomaticallyInstallMacOSUpdates";

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn read() -> Vec<String> {
    strings(&["read", DOMAIN, KEY])
}

fn write() -> Vec<String> {
    strings(&["write", DOMAIN, KEY, "-bool", "false"])
}

/// Runs [`at_start`] and returns what it returned and the lines it said.
fn start(kind: PlatformKind, defaults: &FakeLaunchctl) -> (Option<Change>, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let change = at_start(kind, defaults, &|line| {
        said.borrow_mut().push(line.to_owned());
    });
    (change, said.into_inner())
}

fn change(from: &str) -> Option<Change> {
    Some(Change {
        from: from.to_owned(),
    })
}

fn killed(args: &[String]) -> LaunchctlError {
    LaunchctlError::TimedOut {
        args: args.join(" "),
    }
}

fn spawn_failed(args: &[String]) -> LaunchctlError {
    LaunchctlError::Spawn {
        args: args.join(" "),
        why: "No such file or directory".to_owned(),
    }
}

/// The tool, the domain, the key and the arguments are those of the Mac's own Software Update
/// preference.
#[test]
fn the_constants_and_arguments_name_the_local_software_update_key() {
    assert_eq!(auto_install::DEFAULTS, "/usr/bin/defaults");
    assert_eq!(auto_install::DOMAIN, DOMAIN);
    assert_eq!(auto_install::KEY, KEY);
    assert_eq!(auto_install::DEFAULTS_DEADLINE.as_secs(), 30);
    assert_eq!(auto_install::read_args(), read());
    assert_eq!(auto_install::write_args(), write());
}

/// A host that is not a Mac runs no `defaults` and says nothing.
#[test]
fn a_linux_host_runs_nothing() {
    let defaults = FakeLaunchctl::new();
    let (changed, said) = start(PlatformKind::Linux, &defaults);
    assert_eq!(changed, None);
    assert_eq!(defaults.calls(), Vec::<Vec<String>>::new());
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(at_host_start(PlatformKind::Linux), None);
}

/// A key that reads `0` is only read: no write, nothing said.
#[test]
fn a_key_that_is_off_is_only_read() {
    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("0\n")));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, None);
    assert_eq!(defaults.calls(), vec![read()]);
    assert_eq!(said, Vec::<String>::new());
}

/// A key that reads `1` is written false with exactly those arguments, and the line names the
/// value it replaced.
#[test]
fn a_key_that_is_on_is_written_false_and_logged() {
    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("1\n")));
    defaults.always(&write(), Ok(LaunchctlOutput::success("")));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, change("1"));
    assert_eq!(defaults.calls(), vec![read(), write()]);
    assert_eq!(
        said,
        vec!["level=info auto_install: AutomaticallyInstallMacOSUpdates was 1, set to 0"]
    );
    assert_eq!(line(&change("1").expect("a change")), said[0]);
}

/// A key that does not exist (`defaults read` exits 1) is written and logged as unset.
#[test]
fn a_key_that_is_absent_is_written_and_logged_as_unset() {
    let defaults = FakeLaunchctl::new();
    defaults.always(
        &read(),
        Ok(LaunchctlOutput::exit(
            1,
            "The domain/default pair does not exist\n",
        )),
    );
    defaults.always(&write(), Ok(LaunchctlOutput::success("")));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, change("unset"));
    assert_eq!(defaults.calls(), vec![read(), write()]);
    assert_eq!(
        said,
        vec!["level=info auto_install: AutomaticallyInstallMacOSUpdates was unset, set to 0"]
    );
}

/// Any other value, a boolean's spelling included, is written over.
#[test]
fn another_value_is_written_over() {
    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("true\n")));
    defaults.always(&write(), Ok(LaunchctlOutput::success("")));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, change("true"));
    assert_eq!(defaults.calls(), vec![read(), write()]);
    assert_eq!(
        said,
        vec!["level=info auto_install: AutomaticallyInstallMacOSUpdates was true, set to 0"]
    );
}

/// A write that exits non-zero says the error with the exit code and the error output, and the
/// start reports no change.
#[test]
fn a_write_that_fails_says_an_error_and_no_change() {
    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("1\n")));
    defaults.always(&write(), Ok(LaunchctlOutput::exit(77, "Must be root\n")));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, None);
    assert_eq!(defaults.calls(), vec![read(), write()]);
    assert_eq!(
        said,
        vec![format!(
            "level=error auto_install: defaults {} ended with Some(77): Must be root",
            write().join(" ")
        )]
    );
}

/// A write that cannot be started, and one killed at its deadline, say so.
#[test]
fn a_write_that_does_not_run_says_why() {
    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("1\n")));
    defaults.always(&write(), Err(spawn_failed(&write())));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, None);
    assert_eq!(
        said,
        vec![format!(
            "level=error auto_install: defaults {} did not run: No such file or directory",
            write().join(" ")
        )]
    );

    let defaults = FakeLaunchctl::new();
    defaults.always(&read(), Ok(LaunchctlOutput::success("1\n")));
    defaults.always(&write(), Err(killed(&write())));
    let (changed, said) = start(PlatformKind::Macos, &defaults);
    assert_eq!(changed, None);
    assert_eq!(
        said,
        vec![format!(
            "level=error auto_install: defaults {} did not run: killed at its deadline of 30 seconds",
            write().join(" ")
        )]
    );
}

/// A read that cannot run says so and the write is still attempted, the state being unknown.
#[test]
fn a_read_that_does_not_run_says_why_and_still_writes() {
    for (error, why) in [
        (spawn_failed(&read()), "No such file or directory"),
        (killed(&read()), "killed at its deadline of 30 seconds"),
    ] {
        let defaults = FakeLaunchctl::new();
        defaults.always(&read(), Err(error));
        defaults.always(&write(), Ok(LaunchctlOutput::success("")));
        let (changed, said) = start(PlatformKind::Macos, &defaults);
        assert_eq!(changed, change("unset"));
        assert_eq!(defaults.calls(), vec![read(), write()]);
        assert_eq!(
            said,
            vec![
                format!(
                    "level=error auto_install: defaults {} did not run: {why}",
                    read().join(" ")
                ),
                "level=info auto_install: AutomaticallyInstallMacOSUpdates was unset, set to 0"
                    .to_owned(),
            ]
        );
    }
}
