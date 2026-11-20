//! The tests of `maintenance_xcode`: the collector over a scratch directory standing for a Mac's
//! files with fixture version lists copied in and a scripted `xcode-select` that records its
//! calls; the sections, the reasons and the calls are asserted by equality.

use std::collections::BTreeSet;
use std::fs;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;
use plist::Dictionary;

use super::*;
use crate::maintenance_collect::LinuxScope;

const V26: &[u8] = include_bytes!("../tests/fixtures/maintenance/xcode/version-26.3.plist");
const V27: &[u8] = include_bytes!("../tests/fixtures/maintenance/xcode/version-27.2.plist");
const NO_BUILD: &[u8] =
    include_bytes!("../tests/fixtures/maintenance/xcode/version-no-build.plist");
const INTEGER: &[u8] = include_bytes!("../tests/fixtures/maintenance/xcode/version-integer.plist");
const GARBAGE: &[u8] = include_bytes!("../tests/fixtures/maintenance/xcode/garbage.bin");

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A Mac whose files are the Xcodes put in, and a scripted `xcode-select`.
struct Mac {
    root: Scratch,
    xcode_select: FakeLaunchctl,
}

impl Mac {
    fn new(name: &str) -> Mac {
        Mac {
            root: scratch(name),
            xcode_select: FakeLaunchctl::new(),
        }
    }

    /// An Xcode at the host path `dir` whose version list holds `bytes`.
    fn xcode(&self, dir: &str, bytes: &[u8]) {
        let contents = self
            .root
            .path()
            .join(dir.trim_start_matches('/'))
            .join("Contents");
        fs::create_dir_all(&contents).expect("the directory");
        fs::write(contents.join("version.plist"), bytes).expect("the file");
    }

    /// `xcode-select -p` answers with the developer directory `dir`.
    fn selects(&self, dir: &str) {
        self.xcode_select.always(
            &args(&["-p"]),
            Ok(LaunchctlOutput::success(&format!("{dir}\n"))),
        );
    }

    fn collector(&self, plists: Box<dyn Plists>) -> Xcodes {
        Xcodes::new(
            Box::new(self.xcode_select.clone()),
            plists,
            HostRoot::at(self.root.path()),
        )
    }

    fn collect_with(&self, plists: Box<dyn Plists>) -> Result<Section, String> {
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        self.collector(plists).collect(&look)
    }

    fn collect(&self) -> Result<Section, String> {
        self.collect_with(Box::new(HostPlists::new(HostRoot::at(self.root.path()))))
    }
}

/// A port that records the paths asked and finds no file.
#[derive(Clone, Default)]
struct RecordingPlists(Arc<Mutex<Vec<String>>>);

impl Plists for RecordingPlists {
    fn dictionary(&self, path: &str) -> Result<Option<Dictionary>, String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_owned());
        Ok(None)
    }
}

fn xcode(
    version: &str,
    build: &str,
    path: &str,
    selected: bool,
    foreign: bool,
) -> pb::InstalledXcode {
    pb::InstalledXcode {
        version: version.to_owned(),
        build: build.to_owned(),
        path: path.to_owned(),
        selected,
        foreign,
    }
}

fn section(xcodes: Vec<pb::InstalledXcode>) -> Result<Section, String> {
    Ok(Section {
        xcodes,
        ..Section::default()
    })
}

const DEVELOPER_27: &str = "/Applications/Xcode-27.2.app/Contents/Developer";

/// Proves the collector's name, which a failed look's reason starts with, and that the one over
/// the host's own program, lists and files is built.
#[test]
fn the_collector_is_named_xcode_and_is_built_over_the_host() {
    assert_eq!(Xcodes::at_host().name(), "xcode");
}

/// Proves two Xcodes are reported in name order with their version and build, the one
/// `xcode-select -p` names is the selected one, and `-p` is the only call made.
#[test]
fn two_xcodes_are_reported_and_the_selected_one_is_flagged() {
    let mac = Mac::new("xcode-two");
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    mac.xcode("/Applications/Xcode-26.3.app", V26);
    mac.selects(DEVELOPER_27);
    assert_eq!(
        mac.collect(),
        section(vec![
            xcode(
                "26.3",
                "17C52",
                "/Applications/Xcode-26.3.app",
                false,
                false
            ),
            xcode("27.2", "27B52", "/Applications/Xcode-27.2.app", true, false),
        ])
    );
    assert_eq!(mac.xcode_select.calls(), vec![args(&["-p"])]);
}

/// Proves a directory not named `Xcode-<version>.app` is reported and flagged foreign, whether it
/// holds the version of another directory's name or not, while the one named for its version is
/// not.
#[test]
fn a_directory_not_named_for_its_version_is_foreign() {
    let mac = Mac::new("xcode-foreign");
    mac.xcode("/Applications/Xcode.app", V27);
    mac.xcode("/Applications/Xcode-beta.app", V27);
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    mac.xcode_select
        .always(&args(&["-p"]), Ok(LaunchctlOutput::exit(2, "")));
    assert_eq!(
        mac.collect(),
        section(vec![
            xcode(
                "27.2",
                "27B52",
                "/Applications/Xcode-27.2.app",
                false,
                false
            ),
            xcode("27.2", "27B52", "/Applications/Xcode-beta.app", false, true),
            xcode("27.2", "27B52", "/Applications/Xcode.app", false, true),
        ])
    );
}

/// Proves a Mac with no Xcode, with no `/Applications`, or with only a file named like one has
/// none to report and `xcode-select` is not run.
#[test]
fn a_mac_with_no_xcode_reports_none_and_runs_nothing() {
    let mac = Mac::new("xcode-none");
    assert_eq!(mac.collect(), Ok(Section::default()));
    fs::create_dir_all(mac.root.path().join("Applications")).expect("the directory");
    fs::write(mac.root.path().join("Applications/Xcode-1.app"), "").expect("the file");
    assert_eq!(mac.collect(), Ok(Section::default()));
    assert_eq!(mac.xcode_select.calls(), Vec::<Vec<String>>::new());
}

/// Proves `xcode-select -p` exiting 2 (no developer directory is set) selects none.
#[test]
fn no_developer_directory_selects_none() {
    let mac = Mac::new("xcode-exit2");
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    mac.xcode_select.always(
        &args(&["-p"]),
        Ok(LaunchctlOutput::exit(
            2,
            "xcode-select: error: no developer tools\n",
        )),
    );
    assert_eq!(
        mac.collect(),
        section(vec![xcode(
            "27.2",
            "27B52",
            "/Applications/Xcode-27.2.app",
            false,
            false
        )])
    );
}

/// Proves the Command Line Tools' directory, and an empty answer, select no Xcode.
#[test]
fn the_command_line_tools_select_none() {
    let mac = Mac::new("xcode-clt");
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    let none = section(vec![xcode(
        "27.2",
        "27B52",
        "/Applications/Xcode-27.2.app",
        false,
        false,
    )]);
    mac.selects("/Library/Developer/CommandLineTools");
    assert_eq!(mac.collect(), none);
    mac.xcode_select
        .always(&args(&["-p"]), Ok(LaunchctlOutput::success("")));
    assert_eq!(mac.collect(), none);
}

/// Proves a developer directory is under an Xcode only past a `/`: the directory of
/// `/Applications/Xcode-27.2.app.old` does not select `/Applications/Xcode-27.2.app`, and the
/// Xcode's own directory does.
#[test]
fn a_directory_that_only_starts_with_the_path_does_not_select_it() {
    let mac = Mac::new("xcode-prefix");
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    mac.selects("/Applications/Xcode-27.2.app.old/Contents/Developer");
    let not_selected = xcode(
        "27.2",
        "27B52",
        "/Applications/Xcode-27.2.app",
        false,
        false,
    );
    assert_eq!(mac.collect(), section(vec![not_selected]));
    mac.selects("/Applications/Xcode-27.2.app");
    assert_eq!(
        mac.collect(),
        section(vec![xcode(
            "27.2",
            "27B52",
            "/Applications/Xcode-27.2.app",
            true,
            false
        )])
    );
}

/// Proves `xcode-select` exiting with another code, or not starting, fails the collector with the
/// step's reason.
#[test]
fn a_failing_xcode_select_fails_the_collector() {
    let mac = Mac::new("xcode-select-fails");
    mac.xcode("/Applications/Xcode-27.2.app", V27);
    mac.xcode_select.then(
        &args(&["-p"]),
        Ok(LaunchctlOutput::exit(1, "xcode-select: bad\nmore\n")),
    );
    assert_eq!(
        mac.collect(),
        Err("xcode-select -p failed with exit code 1: xcode-select: bad".to_owned())
    );
    mac.xcode_select.then(
        &args(&["-p"]),
        Err(LaunchctlError::Spawn {
            args: "-p".to_owned(),
            why: "no such file".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("xcode-select -p could not be run: no such file".to_owned())
    );
}

/// Proves a version list that is not a property list fails the collector with the port's reason,
/// never a clean answer.
#[test]
fn an_unreadable_version_list_fails_the_collector() {
    let mac = Mac::new("xcode-garbage");
    mac.xcode("/Applications/Xcode-27.2.app", GARBAGE);
    mac.selects(DEVELOPER_27);
    assert_eq!(
        mac.collect(),
        Err(
            "/Applications/Xcode-27.2.app/Contents/version.plist is not a property list".to_owned()
        )
    );
}

/// Proves an Xcode with no version list fails the collector naming the file.
#[test]
fn a_missing_version_list_fails_the_collector() {
    let mac = Mac::new("xcode-missing");
    fs::create_dir_all(mac.root.path().join("Applications/Xcode-27.2.app")).expect("the directory");
    assert_eq!(
        mac.collect(),
        Err("/Applications/Xcode-27.2.app/Contents/version.plist is missing".to_owned())
    );
}

/// Proves a version list with no build, and one whose version is not a string, fail the collector
/// naming the file and the key.
#[test]
fn a_version_list_without_a_text_key_fails_the_collector() {
    let mac = Mac::new("xcode-keys");
    mac.xcode("/Applications/Xcode-27.2.app", NO_BUILD);
    assert_eq!(
        mac.collect(),
        Err(
            "/Applications/Xcode-27.2.app/Contents/version.plist has no ProductBuildVersion"
                .to_owned()
        )
    );
    mac.xcode("/Applications/Xcode-27.2.app", INTEGER);
    assert_eq!(
        mac.collect(),
        Err(
            "/Applications/Xcode-27.2.app/Contents/version.plist has no CFBundleShortVersionString"
                .to_owned()
        )
    );
}

/// Proves more Xcodes than a report holds fail the collector before any list is read and before
/// `xcode-select` is run.
#[test]
fn seven_xcodes_fail_the_collector_before_reading_anything() {
    let mac = Mac::new("xcode-seven");
    for n in 1..=7 {
        fs::create_dir_all(mac.root.path().join(format!("Applications/Xcode-{n}.app")))
            .expect("the directory");
    }
    let plists = RecordingPlists::default();
    assert_eq!(
        mac.collect_with(Box::new(plists.clone())),
        Err("7 Xcodes are in /Applications, more than 6".to_owned())
    );
    assert_eq!(
        *plists.0.lock().unwrap_or_else(PoisonError::into_inner),
        Vec::<String>::new()
    );
    assert_eq!(mac.xcode_select.calls(), Vec::<Vec<String>>::new());
}

/// Proves six Xcodes, as many as a report holds, are all reported, each read and flagged foreign
/// when its directory is not named for its version.
#[test]
fn six_xcodes_are_all_reported() {
    let mac = Mac::new("xcode-six");
    for n in 1..=6 {
        mac.xcode(&format!("/Applications/Xcode-{n}.app"), V27);
    }
    mac.selects("/Library/Developer/CommandLineTools");
    let expected = (1..=6)
        .map(|n| {
            let path = format!("/Applications/Xcode-{n}.app");
            xcode("27.2", "27B52", &path, false, true)
        })
        .collect();
    assert_eq!(mac.collect(), section(expected));
    assert_eq!(mac.xcode_select.calls(), vec![args(&["-p"])]);
}

/// Proves an `/Applications` that cannot be listed fails the collector naming it, and
/// `xcode-select` is not run.
#[test]
fn an_applications_that_cannot_be_listed_fails_the_collector() {
    let mac = Mac::new("xcode-applications-file");
    fs::write(mac.root.path().join("Applications"), "").expect("the file");
    assert_eq!(
        mac.collect(),
        Err(format!(
            "/Applications could not be read: {}",
            std::io::Error::from_raw_os_error(libc::ENOTDIR)
        ))
    );
    assert_eq!(mac.xcode_select.calls(), Vec::<Vec<String>>::new());
}
