//! The tests of `xcode_clt`: the four programs as scripted ports that count their calls, and a
//! scratch marker file; the exact arguments of each step, the label chosen and the errors.

use std::os::unix::fs::symlink;
use std::sync::{Arc, Mutex};

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;

use super::*;

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

const INFO: &str =
    "package-id: com.apple.pkg.CLTools_Executables\nversion: 15.3.0.0.1.1708646388\nvolume: /\n";

/// How a [`Hook`] answers: a function of the arguments.
type Answer = Box<dyn Fn(&[String]) -> Result<LaunchctlOutput, LaunchctlError> + Send + Sync>;

/// A program port that answers by a function of its arguments.
struct Hook(Answer);

impl Launchctl for Hook {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        (self.0)(args)
    }
}

/// The Command Line Tools over scripted programs, and the fakes behind them.
struct Rig {
    tools: CommandLineTools,
    pkgutil: FakeLaunchctl,
    select: FakeLaunchctl,
    list: FakeLaunchctl,
    install: FakeLaunchctl,
    marker: PathBuf,
    log: RecordingLog,
    _scratch: Scratch,
}

fn rig(name: &str) -> Rig {
    let scratch = scratch(name);
    let marker = scratch.path().join("marker");
    let pkgutil = FakeLaunchctl::new();
    let select = FakeLaunchctl::new();
    let list = FakeLaunchctl::new();
    let install = FakeLaunchctl::new();
    Rig {
        tools: CommandLineTools::new(
            Box::new(pkgutil.clone()),
            Box::new(select.clone()),
            Box::new(list.clone()),
            Box::new(install.clone()),
            marker.clone(),
        ),
        pkgutil,
        select,
        list,
        install,
        marker,
        log: RecordingLog::default(),
        _scratch: scratch,
    }
}

impl Rig {
    /// The package is installed, with a developer directory.
    fn present(&self) {
        self.pkgutil
            .always(&self.info(), Ok(LaunchctlOutput::success(INFO)));
        self.select.always(
            &words(&["-p"]),
            Ok(LaunchctlOutput::success(
                "/Library/Developer/CommandLineTools\n",
            )),
        );
    }

    fn info(&self) -> Vec<String> {
        words(&["--pkg-info=com.apple.pkg.CLTools_Executables"])
    }

    /// `softwareupdate --list` says `listing`; the install of any label ends with `code`.
    fn offers(&self, listing: &str) {
        self.list
            .always(&words(&["--list"]), Ok(LaunchctlOutput::success(listing)));
    }

    fn installs(&self, label: &str) {
        self.install.always(
            &words(&["--install", label]),
            Ok(LaunchctlOutput::success("Done.\n")),
        );
    }

    fn installed(&self) -> Result<String, CommandLineToolsError> {
        self.tools.install(&self.log)
    }
}

/// Proves a Mac whose package is installed and whose developer directory is selected is looked at
/// with `pkgutil` and `xcode-select -p` alone, and the version is the `version:` line's.
#[test]
fn an_installed_package_with_a_developer_directory_is_present() {
    let rig = rig("clt-present");
    rig.present();
    assert_eq!(
        rig.tools.check(),
        Ok(Some("15.3.0.0.1.1708646388".to_owned()))
    );
    assert_eq!(rig.pkgutil.calls(), [rig.info()]);
    assert_eq!(rig.select.calls(), [words(&["-p"])]);
    assert!(rig.list.calls().is_empty() && rig.install.calls().is_empty());
}

/// Proves a package whose `pkg-info` has no `version:` line is present with no version.
#[test]
fn a_package_that_names_no_version_is_present_with_none() {
    let rig = rig("clt-no-version");
    rig.present();
    rig.pkgutil
        .always(&rig.info(), Ok(LaunchctlOutput::success("package-id: x\n")));
    assert_eq!(rig.tools.check(), Ok(Some(String::new())));
}

/// Proves a package that `pkgutil` does not know is absent, whatever its exit code, and
/// `xcode-select` is not asked.
#[test]
fn a_package_pkgutil_does_not_know_is_absent() {
    let rig = rig("clt-absent");
    rig.pkgutil.always(
        &rig.info(),
        Ok(LaunchctlOutput::exit(
            1,
            "No receipt for 'com.apple.pkg.CLTools_Executables' found",
        )),
    );
    assert_eq!(rig.tools.check(), Ok(None));
    assert!(rig.select.calls().is_empty());
    rig.pkgutil.always(
        &rig.info(),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    assert_eq!(rig.tools.check(), Ok(None));
}

/// Proves an installed package with no developer directory selected has it selected with
/// `xcode-select --switch` and is then present.
#[test]
fn a_package_with_no_developer_directory_is_switched_to_it() {
    let rig = rig("clt-switch");
    rig.present();
    rig.select.then(
        &words(&["-p"]),
        Ok(LaunchctlOutput::exit(2, "no developer directory")),
    );
    rig.select.always(
        &words(&["--switch", DEVELOPER_DIR]),
        Ok(LaunchctlOutput::success("")),
    );
    assert_eq!(
        rig.tools.check(),
        Ok(Some("15.3.0.0.1.1708646388".to_owned()))
    );
    assert_eq!(
        rig.select.calls(),
        [
            words(&["-p"]),
            words(&["--switch", DEVELOPER_DIR]),
            words(&["-p"])
        ]
    );
}

/// Proves a switch that fails is an error naming the step, its exit code and the first line of its
/// standard error.
#[test]
fn a_switch_that_fails_is_an_error_with_its_first_line() {
    let rig = rig("clt-switch-fails");
    rig.present();
    rig.select
        .then(&words(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
    rig.select.always(
        &words(&["--switch", DEVELOPER_DIR]),
        Ok(LaunchctlOutput::exit(
            1,
            "xcode-select: error: boom\nmore\n",
        )),
    );
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "xcode-select --switch failed with exit code 1: xcode-select: error: boom"
    );
}

/// Proves a developer directory that is still not selected after the switch is an error.
#[test]
fn a_developer_directory_still_missing_after_the_switch_is_an_error() {
    let rig = rig("clt-switch-no-effect");
    rig.present();
    rig.select
        .always(&words(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
    rig.select.always(
        &words(&["--switch", DEVELOPER_DIR]),
        Ok(LaunchctlOutput::success("")),
    );
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "xcode-select -p names no developer directory after its switch"
    );
}

/// Proves a program that cannot be started, or that ran past its deadline, is an error naming the
/// step, at each of the three steps of a look.
#[test]
fn a_program_that_does_not_run_is_an_error_naming_its_step() {
    let rig = rig("clt-no-run");
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "pkgutil --pkg-info could not be run: the fake has no answer"
    );
    rig.pkgutil
        .always(&rig.info(), Ok(LaunchctlOutput::success(INFO)));
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "xcode-select -p could not be run: the fake has no answer"
    );
    rig.select
        .always(&words(&["-p"]), Ok(LaunchctlOutput::exit(1, "")));
    rig.select.always(
        &words(&["--switch", DEVELOPER_DIR]),
        Err(LaunchctlError::TimedOut {
            args: "--switch".to_owned(),
        }),
    );
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "xcode-select --switch did not finish within its deadline"
    );
}

/// Proves the install makes the marker, lists, installs the label, removes the marker and looks
/// again, each program asked once, and says the version it then found.
#[test]
fn an_install_lists_installs_and_looks_again() {
    let rig = rig("clt-install");
    rig.present();
    rig.offers("Software Update Tool\n\n* Label: Command Line Tools for Xcode-15.3\n\tTitle: Command Line Tools for Xcode, Version: 15.3, Size: 700000KiB, Recommended: YES,\n");
    rig.installs("Command Line Tools for Xcode-15.3");
    assert_eq!(rig.installed(), Ok("15.3.0.0.1.1708646388".to_owned()));
    assert_eq!(rig.list.calls(), [words(&["--list"])]);
    assert_eq!(
        rig.install.calls(),
        [words(&["--install", "Command Line Tools for Xcode-15.3"])]
    );
    assert_eq!(rig.pkgutil.calls(), [rig.info()]);
    assert!(!rig.marker.exists());
    assert!(rig.log.lines().is_empty());
}

/// Proves the label of the highest version is installed, versions compared by number part by part
/// (15.10 above 15.9, 15.3.1 above 15.3 in either order), a part that is not a number compared as
/// text, labels of other software ignored, and the first of equal versions taken.
#[test]
fn the_highest_version_among_the_labels_is_installed() {
    let cases: [(&[&str], &str); 5] = [
        (&["15.3", "16.0", "15.10"], "16.0"),
        (&["15.9", "15.10"], "15.10"),
        (&["15.3.1", "15.3"], "15.3.1"),
        (&["15.3", "15.3.1"], "15.3.1"),
        (&["16.0", "16.0b2"], "16.0b2"),
    ];
    for (versions, chosen) in cases {
        let mut listing = String::from("* Label: macOS Sonoma 14.4-23E214\n  Title: macOS\n");
        for version in versions {
            listing.push_str(&format!(
                "* Label: Command Line Tools for Xcode-{version}\n"
            ));
        }
        listing.push_str("Label: Command Line Tools for Xcode-99.0\n");
        assert_eq!(
            highest_label(&listing),
            Some(format!("Command Line Tools for Xcode-{chosen}").as_str())
        );
    }
    let same = "* Label: Command Line Tools for Xcode-15.3\n* Label:  Command Line Tools for Xcode-15.3 \n";
    assert_eq!(
        highest_label(same),
        Some("Command Line Tools for Xcode-15.3")
    );
    assert_eq!(highest_label(""), None);
}

/// Proves a listing with no label of the tools is an error, `softwareupdate --install` is never
/// run, and the marker is gone.
#[test]
fn a_listing_with_no_tools_fails_the_install() {
    let rig = rig("clt-no-label");
    rig.offers("No new software available.\n");
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list offered no Command Line Tools for Xcode"
    );
    assert!(rig.install.calls().is_empty());
    assert!(!rig.marker.exists());
}

/// Proves a list that exits non-zero and an install that exits non-zero are errors naming the step
/// and the first line of its standard error, and the marker is removed in both.
#[test]
fn a_failing_list_or_install_is_an_error_and_the_marker_is_removed() {
    let rig = rig("clt-fails");
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput::exit(1, "Software Update: no network\n")),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list failed with exit code 1: Software Update: no network"
    );
    assert!(!rig.marker.exists());
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    rig.install.always(
        &words(&["--install", "Command Line Tools for Xcode-15.3"]),
        Ok(LaunchctlOutput::exit(3, "Install failed\n")),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --install failed with exit code 3: Install failed"
    );
    assert!(!rig.marker.exists());
}

/// Proves a package that is still not installed after `softwareupdate --install` exited 0 is an
/// error.
#[test]
fn tools_still_missing_after_the_install_are_an_error() {
    let rig = rig("clt-still-missing");
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    rig.installs("Command Line Tools for Xcode-15.3");
    rig.pkgutil
        .always(&rig.info(), Ok(LaunchctlOutput::exit(1, "")));
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "the Command Line Tools are not installed after softwareupdate"
    );
}

/// Proves a marker that is a link is refused: nothing is listed or installed, the link and what it
/// points at stay as they were.
#[test]
fn a_marker_that_is_a_link_is_refused() {
    let rig = rig("clt-link");
    let target = rig.marker.with_file_name("target");
    fs::write(&target, "keep").expect("the target");
    symlink(&target, &rig.marker).expect("the link");
    let error = rig.installed().expect_err("refused").to_string();
    assert!(error.starts_with("the install marker "), "{error}");
    assert!(rig.list.calls().is_empty() && rig.install.calls().is_empty());
    assert_eq!(fs::read_to_string(&target).expect("the target"), "keep");
    assert!(
        fs::symlink_metadata(&rig.marker)
            .expect("the link")
            .file_type()
            .is_symlink()
    );
}

/// Proves a marker that is a hard link to another file, as a local user may put one in `/tmp`,
/// changes nothing of that file: it is opened, never truncated or written, and the install's
/// removal takes away the marker's name alone.
#[test]
fn a_marker_that_is_a_hard_link_leaves_the_linked_file_as_it_was() {
    let rig = rig("clt-hard-link");
    rig.present();
    let other = rig.marker.with_file_name("other");
    fs::write(&other, "keep these bytes").expect("the other file");
    fs::hard_link(&other, &rig.marker).expect("the hard link");
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    rig.installs("Command Line Tools for Xcode-15.3");
    assert!(rig.installed().is_ok());
    assert!(!rig.marker.exists());
    assert_eq!(
        fs::read_to_string(&other).expect("the other file"),
        "keep these bytes"
    );
}

/// Proves a marker path that holds a directory is refused: nothing is listed or installed.
#[test]
fn a_marker_that_is_a_directory_is_refused() {
    let rig = rig("clt-not-a-file");
    fs::create_dir(&rig.marker).expect("a directory");
    let error = rig.installed().expect_err("refused").to_string();
    assert!(
        error.starts_with(&format!(
            "the install marker {} could not be made: ",
            rig.marker.display()
        )),
        "{error}"
    );
    assert!(rig.list.calls().is_empty() && rig.install.calls().is_empty());
}

/// Proves a marker left by an earlier run is reused and removed, and `softwareupdate` having
/// removed the marker itself is no error.
#[test]
fn a_left_over_marker_is_reused_and_one_already_gone_is_no_error() {
    let rig = rig("clt-leftover");
    rig.present();
    fs::write(&rig.marker, "old").expect("the leftover");
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    rig.installs("Command Line Tools for Xcode-15.3");
    assert!(rig.installed().is_ok());
    assert!(!rig.marker.exists());
    let marker = rig.marker.clone();
    let gone = CommandLineTools::new(
        Box::new(rig.pkgutil.clone()),
        Box::new(rig.select.clone()),
        Box::new(rig.list.clone()),
        Box::new(Hook(Box::new(move |_| {
            fs::remove_file(&marker).expect("the marker is there while softwareupdate runs");
            Ok(LaunchctlOutput::success(""))
        }))),
        rig.marker.clone(),
    );
    assert!(gone.install(&rig.log).is_ok());
    assert!(rig.log.lines().is_empty());
}

/// Proves a marker that cannot be removed is logged and does not fail an install that worked.
#[test]
fn a_marker_that_cannot_be_removed_is_logged_not_fatal() {
    let rig = rig("clt-remove-fails");
    rig.present();
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    let marker = rig.marker.clone();
    let seen = Arc::new(Mutex::new(false));
    let note = Arc::clone(&seen);
    let tools = CommandLineTools::new(
        Box::new(rig.pkgutil.clone()),
        Box::new(rig.select.clone()),
        Box::new(rig.list.clone()),
        Box::new(Hook(Box::new(move |_| {
            fs::remove_file(&marker).expect("the marker");
            fs::create_dir(&marker).expect("a directory in its place");
            *note.lock().expect("the note") = true;
            Ok(LaunchctlOutput::success(""))
        }))),
        rig.marker.clone(),
    );
    assert_eq!(
        tools.install(&rig.log),
        Ok("15.3.0.0.1.1708646388".to_owned())
    );
    assert!(*seen.lock().expect("the note"));
    let lines = rig.log.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with(
            "error: host prerequisites: the install marker of the Command Line Tools could not be removed: "
        ),
        "{lines:?}"
    );
}

/// Proves the production steps are built over the system programs without running any of them.
#[test]
fn the_host_steps_are_built_over_the_system_programs() {
    let tools = CommandLineTools::at_host();
    assert_eq!(tools.marker, Path::new(INSTALL_MARKER));
}

/// Proves a look after the switch that cannot run, and a look after the install that cannot run,
/// are errors naming the step; the marker is gone after the second.
#[test]
fn a_look_that_cannot_run_after_a_switch_or_an_install_is_an_error() {
    let rig = rig("clt-look-after");
    rig.pkgutil
        .always(&rig.info(), Ok(LaunchctlOutput::success(INFO)));
    rig.select
        .then(&words(&["-p"]), Ok(LaunchctlOutput::exit(1, "")));
    rig.select.always(
        &words(&["--switch", DEVELOPER_DIR]),
        Ok(LaunchctlOutput::success("")),
    );
    assert_eq!(
        rig.tools.check().expect_err("fails").to_string(),
        "xcode-select -p after its switch could not be run: the fake has no answer"
    );
    let rig = self::rig("clt-look-after-install");
    rig.offers("* Label: Command Line Tools for Xcode-15.3\n");
    rig.installs("Command Line Tools for Xcode-15.3");
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "pkgutil --pkg-info could not be run: the fake has no answer"
    );
    assert!(!rig.marker.exists());
}
