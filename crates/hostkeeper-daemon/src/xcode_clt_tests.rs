//! The tests of `xcode_clt`: the programs as scripted ports that count their calls, a scripted
//! disk behind the file port, and a scratch marker file; the exact arguments of each step, the
//! label chosen, the look's three ways to find the tools and the errors, reasons by equality.

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

/// What `sw_vers` writes on the Macs of these tests.
const SW_VERS_OUT: &str =
    "ProductName:\t\tmacOS\nProductVersion:\t\t15.1\nBuildVersion:\t\t24B83\n";

/// The developer directory of a full Xcode, as `xcode-select -p` names it.
const XCODE_DIR: &str = "/Applications/Xcode.app/Contents/Developer";

/// How a [`Hook`] answers: a function of the arguments.
type Answer = Box<dyn Fn(&[String]) -> Result<LaunchctlOutput, LaunchctlError> + Send + Sync>;

/// A program port that answers by a function of its arguments.
struct Hook(Answer);

impl Launchctl for Hook {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        (self.0)(args)
    }
}

/// A scripted disk: the paths that are files, and every path asked about, in order.
#[derive(Clone, Default)]
struct Disk {
    files: Arc<Mutex<Vec<PathBuf>>>,
    asked: Arc<Mutex<Vec<PathBuf>>>,
}

impl Disk {
    fn put(&self, path: &str) {
        self.files
            .lock()
            .expect("the disk")
            .push(PathBuf::from(path));
    }

    fn asked(&self) -> Vec<PathBuf> {
        self.asked.lock().expect("the disk").clone()
    }
}

impl FileCheck for Disk {
    fn is_file(&self, path: &Path) -> bool {
        self.asked.lock().expect("the disk").push(path.to_owned());
        self.files
            .lock()
            .expect("the disk")
            .iter()
            .any(|file| file == path)
    }
}

/// The scripted programs and disk of a Mac, from which the tools are built.
struct Rig {
    pkgutil: FakeLaunchctl,
    select: FakeLaunchctl,
    xcodebuild: FakeLaunchctl,
    sw_vers: FakeLaunchctl,
    list: FakeLaunchctl,
    install: FakeLaunchctl,
    disk: Disk,
    marker: PathBuf,
    log: RecordingLog,
    _scratch: Scratch,
}

fn rig(name: &str) -> Rig {
    let scratch = scratch(name);
    let marker = scratch.path().join("marker");
    let sw_vers = FakeLaunchctl::new();
    sw_vers.always(&[], Ok(LaunchctlOutput::success(SW_VERS_OUT)));
    Rig {
        pkgutil: FakeLaunchctl::new(),
        select: FakeLaunchctl::new(),
        xcodebuild: FakeLaunchctl::new(),
        sw_vers,
        list: FakeLaunchctl::new(),
        install: FakeLaunchctl::new(),
        disk: Disk::default(),
        marker,
        log: RecordingLog::default(),
        _scratch: scratch,
    }
}

impl Rig {
    /// The tools over the rig's programs, disk and marker, with `install` for
    /// `softwareupdate --install`.
    fn tools_with(&self, install: Box<dyn Launchctl>) -> CommandLineTools {
        CommandLineTools::new(Ports {
            pkgutil: Box::new(self.pkgutil.clone()),
            xcode_select: Box::new(self.select.clone()),
            xcodebuild: Box::new(self.xcodebuild.clone()),
            sw_vers: Box::new(self.sw_vers.clone()),
            list: Box::new(self.list.clone()),
            install,
            files: Box::new(self.disk.clone()),
            marker: self.marker.clone(),
        })
    }

    /// The tools over the rig's programs, disk and marker.
    fn tools(&self) -> CommandLineTools {
        self.tools_with(Box::new(self.install.clone()))
    }

    /// No receipt: `pkgutil` does not know the package.
    fn no_receipt(&self) {
        self.pkgutil.always(
            &self.info(),
            Ok(LaunchctlOutput::exit(
                1,
                "No receipt for 'com.apple.pkg.CLTools_Executables' found",
            )),
        );
    }

    /// `xcode-select -p` exits 0 and names `directory`.
    fn selects(&self, directory: &str) {
        self.select.always(
            &words(&["-p"]),
            Ok(LaunchctlOutput::success(&format!("{directory}\n"))),
        );
    }

    /// `xcodebuild` answers its licence check, first-launch check and version with these codes.
    fn xcode(&self, licence: i32, first_launch: i32, version: i32) {
        let answer = |code: i32, stdout: &str| {
            Ok(LaunchctlOutput {
                code: Some(code),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        };
        self.xcodebuild
            .always(&words(&["-license", "check"]), answer(licence, ""));
        self.xcodebuild.always(
            &words(&["-checkFirstLaunchStatus"]),
            answer(first_launch, ""),
        );
        self.xcodebuild.always(
            &words(&["-version"]),
            answer(version, "Xcode 16.1\nBuild version 16B40\n"),
        );
    }

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
        self.tools().install(&self.log)
    }
}

/// Proves a Mac whose package is installed and whose developer directory is selected is looked at
/// with `pkgutil` and `xcode-select -p` alone, and the version is the `version:` line's.
#[test]
fn an_installed_package_with_a_developer_directory_is_present() {
    let rig = rig("clt-present");
    rig.present();
    assert_eq!(
        rig.tools().check(),
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
    assert_eq!(rig.tools().check(), Ok(Some(String::new())));
}

/// Proves a package that `pkgutil` does not know, whatever its exit code, with no compiler on disk
/// and no developer directory selected, is absent: `xcode-select -p` is asked once, and neither
/// `xcode-select --switch` nor `xcodebuild` is run.
#[test]
fn a_package_pkgutil_does_not_know_is_absent() {
    let rig = rig("clt-absent");
    rig.no_receipt();
    rig.select
        .always(&words(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
    assert_eq!(rig.tools().check(), Ok(None));
    assert_eq!(rig.select.calls(), [words(&["-p"])]);
    assert_eq!(rig.disk.asked(), [PathBuf::from(CLANG)]);
    assert!(rig.xcodebuild.calls().is_empty());
    rig.pkgutil.always(
        &rig.info(),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    assert_eq!(rig.tools().check(), Ok(None));
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
        rig.tools().check(),
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
        rig.tools().check().expect_err("fails").to_string(),
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
        rig.tools().check().expect_err("fails").to_string(),
        "xcode-select -p names no developer directory after its switch"
    );
}

/// Proves a program that cannot be started, or that ran past its deadline, is an error naming the
/// step, at each of the three steps of a look.
#[test]
fn a_program_that_does_not_run_is_an_error_naming_its_step() {
    let rig = rig("clt-no-run");
    assert_eq!(
        rig.tools().check().expect_err("fails").to_string(),
        "pkgutil --pkg-info could not be run: the fake has no answer"
    );
    rig.pkgutil
        .always(&rig.info(), Ok(LaunchctlOutput::success(INFO)));
    assert_eq!(
        rig.tools().check().expect_err("fails").to_string(),
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
        rig.tools().check().expect_err("fails").to_string(),
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
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: No new software available.; stderr empty"
    );
    assert!(rig.install.calls().is_empty());
    assert!(!rig.marker.exists());
}

/// Proves the label of a current catalog, `Command Line Tools for Xcode <version>-<version>`, is
/// installed as it is listed, with the listing's other lines and labels of other software around it.
/// Catches a parser that knows only the `Command Line Tools for Xcode-<version>` form.
#[test]
fn a_label_of_the_current_form_is_installed() {
    let rig = rig("clt-current-form");
    rig.present();
    rig.offers(concat!(
        "Software Update Tool\n\nFinding available software\n",
        "Software Update found the following new or updated software:\n",
        "* Label: macOS Tahoe 26.1-25B78\n",
        "\tTitle: macOS Tahoe 26.1, Version: 26.1, Size: 7000000KiB, Recommended: YES, Action: restart,\n",
        "* Label: Command Line Tools for Xcode 26.4-26.4.1\n",
        "\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4.1, Size: 900000KiB, Recommended: YES,\n",
    ));
    rig.installs("Command Line Tools for Xcode 26.4-26.4.1");
    assert_eq!(rig.installed(), Ok("15.3.0.0.1.1708646388".to_owned()));
    assert_eq!(
        rig.install.calls(),
        [words(&[
            "--install",
            "Command Line Tools for Xcode 26.4-26.4.1"
        ])]
    );
    assert!(!rig.marker.exists());
}

/// Proves both forms of the label are compared by the tools' version (the last of
/// `<version>-<version>`, all of the old form), and that a label with no `-` or space after the name, one with no
/// version, and a beta's label are no label of the tools.
#[test]
fn both_label_forms_are_compared_and_near_misses_are_ignored() {
    let listing = concat!(
        "* Label: Command Line Tools for Xcode-16.0\n",
        "* Label: Command Line Tools for Xcode 26.1-26.1\n",
        "* Label: Command Line Tools for Xcode 26.4-26.4.1\n",
        "* Label: Command Line Tools for Xcode 26.4-26.4\n",
    );
    assert_eq!(
        highest_label(listing),
        Some("Command Line Tools for Xcode 26.4-26.4.1")
    );
    assert_eq!(
        highest_label(
            "* Label: Command Line Tools for Xcode-16.0\n* Label: Command Line Tools for Xcode 15.4-15.4\n"
        ),
        Some("Command Line Tools for Xcode-16.0")
    );
    for miss in [
        "* Label: Command Line Tools for Xcode26.0\n",
        "* Label: Command Line Tools for Xcode 26.0-\n",
        "* Label: Command Line Tools for Xcode-\n",
        "* Label: Command Line Tools beta 2 for Xcode 26.0-26.0\n",
    ] {
        assert_eq!(highest_label(miss), None, "{miss}");
    }
    assert_eq!(
        tools_version("Command Line Tools for Xcode 26.4-26.4.1"),
        Some("26.4.1")
    );
    assert_eq!(
        tools_version("Command Line Tools for Xcode-15.3"),
        Some("15.3")
    );
    assert_eq!(
        tools_version("Command Line Tools for Xcode-16.0-beta1"),
        Some("16.0-beta1")
    );
}

/// Proves a listing with labels but none that holds `Command Line Tools` fails with a reason that
/// names every label it listed, in its order (a macOS update offered with no tools), and
/// `softwareupdate --install` is never run.
#[test]
fn no_tools_among_listed_labels_names_the_labels() {
    let rig = rig("clt-other-labels");
    rig.offers("* Label: macOS Tahoe 26.1-25B78\n* Label: Safari 26.1\n");
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: * Label: macOS Tahoe 26.1-25B78; stderr empty; it listed: macOS Tahoe 26.1-25B78; Safari 26.1"
    );
    assert!(rig.install.calls().is_empty());
    assert!(!rig.marker.exists());
}

/// Proves a listing with no label at all fails with a reason that names the first line of each of
/// `softwareupdate`'s streams that is not blank, where it says that nothing is offered.
#[test]
fn no_label_at_all_names_the_first_line_of_standard_error() {
    let rig = rig("clt-stderr");
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: b"Software Update Tool\n\nFinding available software\n".to_vec(),
            stderr: b"\n  \nNo new software available.\nsecond line\n".to_vec(),
        }),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: Software Update Tool; stderr: No new software available."
    );
    assert!(rig.install.calls().is_empty());
    assert!(!rig.marker.exists());
}

/// Proves a list ended by a signal is an error that says so, with what the Mac is and was offered,
/// and the marker is removed.
#[test]
fn a_list_ended_by_a_signal_is_an_error() {
    let rig = rig("clt-list-signal");
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list was ended by a signal; macOS 15.1 24B83; stdout empty; stderr empty"
    );
    assert!(rig.install.calls().is_empty());
    assert!(!rig.marker.exists());
}

/// Proves a list that exits non-zero and an install that exits non-zero are errors naming the step,
/// its exit code and the first line of its standard error (the list's with the macOS version and
/// its standard output too), and the marker is removed in both.
#[test]
fn a_failing_list_or_install_is_an_error_and_the_marker_is_removed() {
    let rig = rig("clt-fails");
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput::exit(1, "Software Update: no network\n")),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list failed with exit code 1; macOS 15.1 24B83; stdout empty; stderr: Software Update: no network"
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
    rig.select
        .always(&words(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
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
    let gone = rig.tools_with(Box::new(Hook(Box::new(move |_| {
        fs::remove_file(&marker).expect("the marker is there while softwareupdate runs");
        Ok(LaunchctlOutput::success(""))
    }))));
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
    let tools = rig.tools_with(Box::new(Hook(Box::new(move |_| {
        fs::remove_file(&marker).expect("the marker");
        fs::create_dir(&marker).expect("a directory in its place");
        *note.lock().expect("the note") = true;
        Ok(LaunchctlOutput::success(""))
    }))));
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
        rig.tools().check().expect_err("fails").to_string(),
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

/// Proves tools on disk with no receipt, the compiler there and `xcode-select -p` exiting 0, are
/// present with no version, and nothing is listed, installed or asked of `xcodebuild`. Catches a
/// look that knows only the receipt, which sends such a Mac to `softwareupdate` for nothing.
#[test]
fn tools_on_disk_without_a_receipt_are_present_and_nothing_is_installed() {
    let rig = rig("clt-on-disk");
    rig.no_receipt();
    rig.disk.put(CLANG);
    rig.selects(DEVELOPER_DIR);
    assert_eq!(rig.tools().check(), Ok(Some(String::new())));
    assert_eq!(rig.disk.asked(), [PathBuf::from(CLANG)]);
    assert_eq!(rig.select.calls(), [words(&["-p"])]);
    assert!(rig.xcodebuild.calls().is_empty());
    assert!(rig.list.calls().is_empty() && rig.install.calls().is_empty());
}

/// Proves the compiler on disk with no receipt is absent when `xcode-select -p` does not exit 0,
/// and no switch is made: the receipt alone earns the switch.
#[test]
fn tools_on_disk_with_no_developer_directory_are_absent() {
    let rig = rig("clt-on-disk-unselected");
    rig.no_receipt();
    rig.disk.put(CLANG);
    rig.select
        .always(&words(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
    assert_eq!(rig.tools().check(), Ok(None));
    assert_eq!(rig.select.calls(), [words(&["-p"])]);
}

/// Proves a selected Xcode whose licence is accepted and whose first launch ran is present with the
/// first line of `xcodebuild -version` as the version, with no receipt and no compiler of the tools
/// on disk; the three `xcodebuild` steps run in order and nothing is listed or installed.
#[test]
fn a_ready_selected_xcode_is_present() {
    let rig = rig("clt-xcode");
    rig.no_receipt();
    rig.selects(XCODE_DIR);
    rig.xcode(0, 0, 0);
    assert_eq!(rig.tools().check(), Ok(Some("Xcode 16.1".to_owned())));
    assert_eq!(
        rig.xcodebuild.calls(),
        [
            words(&["-license", "check"]),
            words(&["-checkFirstLaunchStatus"]),
            words(&["-version"]),
        ]
    );
    assert!(rig.list.calls().is_empty() && rig.install.calls().is_empty());
}

/// Proves a selected Xcode is absent when its licence is not accepted, when its first launch did not
/// run, or when it gives no version, and each `xcodebuild` step after the one that failed is not
/// run. Catches a look that counts any selected Xcode.
#[test]
fn an_xcode_not_ready_is_absent() {
    let cases: [((i32, i32, i32), usize); 3] = [((1, 0, 0), 1), ((0, 69, 0), 2), ((0, 0, 1), 3)];
    for ((licence, first_launch, version), steps) in cases {
        let rig = rig(&format!("clt-xcode-not-ready-{steps}"));
        rig.no_receipt();
        rig.selects(&format!("{XCODE_DIR}/"));
        rig.xcode(licence, first_launch, version);
        assert_eq!(
            rig.tools().check(),
            Ok(None),
            "{licence} {first_launch} {version}"
        );
        assert_eq!(rig.xcodebuild.calls().len(), steps);
    }
}

/// Proves a selected directory that is no Xcode's, with no receipt and no compiler there, is
/// absent and `xcodebuild` is never asked: the tools' own directory, an empty answer and a
/// directory that only holds the word.
#[test]
fn a_selected_directory_that_is_no_xcode_is_absent() {
    for (n, directory) in [
        DEVELOPER_DIR,
        "",
        "/Applications/Xcode.app/Contents",
        "/opt/Xcode.app/Contents/Developer/bin",
    ]
    .into_iter()
    .enumerate()
    {
        let rig = rig(&format!("clt-not-xcode-{n}"));
        rig.no_receipt();
        rig.selects(directory);
        rig.xcode(0, 0, 0);
        assert_eq!(rig.tools().check(), Ok(None), "{directory}");
        assert!(rig.xcodebuild.calls().is_empty(), "{directory}");
    }
}

/// Proves the steps of the look with no receipt that cannot run are errors naming the step:
/// `xcode-select -p` and `xcodebuild`.
#[test]
fn a_look_without_a_receipt_that_cannot_run_is_an_error_naming_its_step() {
    let rig = rig("clt-no-receipt-no-run");
    rig.no_receipt();
    assert_eq!(
        rig.tools().check().expect_err("fails").to_string(),
        "xcode-select -p could not be run: the fake has no answer"
    );
    rig.selects(XCODE_DIR);
    assert_eq!(
        rig.tools().check().expect_err("fails").to_string(),
        "xcodebuild -license check could not be run: the fake has no answer"
    );
}

/// Proves the reason of a listing with no tools names the macOS version and build, the first line
/// of each stream that is not blank, and every label that holds `Command Line Tools` in any case,
/// in its order, and no other label, by equality on a fixture of a listing whose label of the
/// tools is of a spelling the parser does not take.
#[test]
fn a_failed_listing_names_the_mac_both_streams_and_the_tools_labels() {
    let rig = rig("clt-reason-fixture");
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: concat!(
                "\nSoftware Update Tool\n\nFinding available software\n",
                "Software Update found the following new or updated software:\n",
                "* Label: macOS Sequoia 15.7.2-24G325\n",
                "\tTitle: macOS Sequoia 15.7.2, Version: 15.7.2, Size: 5000000KiB, Recommended: YES, Action: restart,\n",
                "* Label: Command Line Tools beta 2 for Xcode 26.0-26.0\n",
                "* Label: command line tools for Xcode (26.1)\n",
            )
            .as_bytes()
            .to_vec(),
            stderr: b"  \nscan done\nsecond line\n".to_vec(),
        }),
    );
    assert_eq!(
        rig.installed().expect_err("fails").to_string(),
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: Software Update Tool; stderr: scan done; Command Line Tools labels: Command Line Tools beta 2 for Xcode 26.0-26.0; command line tools for Xcode (26.1)"
    );
    assert!(rig.install.calls().is_empty());
    assert_eq!(rig.sw_vers.calls(), [Vec::<String>::new()]);
}

/// Proves the reason of a huge listing stays within the bound the coordinator reads and ends at a
/// character boundary: each stream's first line is cut to its own bound, and labels follow while
/// the bound allows.
#[test]
fn the_reason_of_a_huge_listing_stays_within_its_bound() {
    let rig = rig("clt-reason-bound");
    let long = "é".repeat(500);
    let mut listing = format!("{long}\n");
    for n in 0..5_000 {
        listing.push_str(&format!("* Label: Command Line Tools {n} {long}\n"));
    }
    rig.list.always(
        &words(&["--list"]),
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: listing.into_bytes(),
            stderr: format!("{long}\n").into_bytes(),
        }),
    );
    let reason = rig.installed().expect_err("fails").to_string();
    let line = "é".repeat(LINE_MAX / 2);
    let head = format!(
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: {line}; stderr: {line}; Command Line Tools labels: Command Line Tools 0 éé"
    );
    assert!(reason.starts_with(&head), "{reason}");
    assert!(
        reason.len() <= MAX_PREREQUISITE_REASON && reason.len() >= MAX_PREREQUISITE_REASON - 1,
        "{}",
        reason.len()
    );
}

/// Proves a `sw_vers` that does not run, exits non-zero, or names no version or build is said as
/// such in the reason, and the reason is still given.
#[test]
fn a_macos_version_that_cannot_be_read_is_said_so() {
    let rig = rig("clt-sw-vers");
    rig.offers("");
    let reason = |rig: &Rig| rig.installed().expect_err("fails").to_string();
    let tail = "; stdout empty; stderr empty";
    for answer in [
        Err(LaunchctlError::TimedOut {
            args: String::new(),
        }),
        Ok(LaunchctlOutput::exit(1, "")),
    ] {
        rig.sw_vers.always(&[], answer);
        assert_eq!(reason(&rig), format!("{NONE_OFFERED}; macOS unknown{tail}"));
    }
    rig.sw_vers.always(
        &[],
        Ok(LaunchctlOutput::success(
            "ProductName: macOS\nBuildVersion:\n",
        )),
    );
    assert_eq!(reason(&rig), format!("{NONE_OFFERED}; macOS ? ?{tail}"));
}

/// Proves the host's file port says a regular file is one, and a directory or a missing path is
/// not.
#[test]
fn the_host_file_port_tells_a_file_from_anything_else() {
    let scratch = scratch("clt-host-files");
    let file = scratch.path().join("clang");
    fs::write(&file, "").expect("the file");
    assert!(HostFiles.is_file(&file));
    assert!(!HostFiles.is_file(scratch.path()));
    assert!(!HostFiles.is_file(&scratch.path().join("missing")));
}

/// Proves a cut ends at a character boundary at or below its bound, and leaves a shorter text whole.
#[test]
fn a_cut_ends_at_a_character_boundary() {
    assert_eq!(cut("éé", 3), "é");
    assert_eq!(cut("ab", 9), "ab");
    assert_eq!(cut("abc", 2), "ab");
}
