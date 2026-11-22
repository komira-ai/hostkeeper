//! The tests of the apply of the `xcode` kind, over a scratch directory standing for the Mac's
//! files and fake programs (each Xcode's `xcodebuild`, `xcode-select`) that record each run and
//! answer as the test scripts them, one able to change the files as it runs. Each failure is
//! asserted by equality of its reason and its whole detail, each run of a program by equality of
//! the whole list, and the files left by listing `/Applications` whole.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintenanceKind as WireKind};
use hostkeeper_testkit::Scratch;

use super::*;

#[path = "removal_tests.rs"]
mod removal;
#[path = "verify_tests.rs"]
mod verify;

const GARBAGE: &[u8] = include_bytes!("../../../tests/fixtures/maintenance/xcode/garbage.bin");
/// What a version list of [`GARBAGE`] fails with, for the app `<app>`.
const GARBAGE_DETAIL: &str = "<app>/Contents/version.plist is not a property list";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The version list of Xcode `version` (`build`).
fn plist(version: &str, build: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
         \t<key>ProductBuildVersion</key>\n\t<string>{build}</string>\n</dict>\n</plist>\n"
    )
}

/// The order's entry of Xcode `version` (`build`): its SHA-256 made of its version's digits.
fn entry(version: &str, build: &str) -> XcodeEntry {
    let digits: String = version.chars().filter(char::is_ascii_digit).collect();
    let sha256 = format!("{digits:0>64}");
    XcodeEntry {
        version: version.to_owned(),
        build: build.to_owned(),
        size: 1,
        sha256,
    }
}

/// The parameters of a run of `xcode` keeping `xcodes`, removing `remove` and selecting
/// `default`.
fn params(xcodes: &[XcodeEntry], remove: &[&str], default: &str) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: "run-7".to_owned(),
        kinds: vec![WireKind::Xcode.into()],
        xcodes: xcodes.to_vec(),
        xcode_remove: remove.iter().map(|version| (*version).to_owned()).collect(),
        xcode_default: default.to_owned(),
        ..Maintain::default()
    })
    .expect("valid parameters")
}

/// What a fake program does when it runs: its answer, after any change it makes to the files
/// under the root it is given.
type Answer = Arc<dyn Fn(&Path) -> Result<LaunchctlOutput, LaunchctlError> + Send + Sync>;

/// The Mac's programs: each run recorded as `<program> <args>` in `log`, with its deadline in
/// `deadlines`, and answered by its scripted answer, or exit 0 saying nothing.
#[derive(Clone, Default)]
struct FakePrograms {
    root: PathBuf,
    answers: Arc<Mutex<BTreeMap<String, Answer>>>,
    log: Arc<Mutex<Vec<String>>>,
    deadlines: Arc<Mutex<Vec<Duration>>>,
}

impl Programs for FakePrograms {
    fn tool(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(FakeTool {
            programs: self.clone(),
            program: program.display().to_string(),
            deadline,
        })
    }
}

struct FakeTool {
    programs: FakePrograms,
    program: String,
    deadline: Duration,
}

impl Launchctl for FakeTool {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let line = format!("{} {}", self.program, args.join(" "));
        lock(&self.programs.log).push(line.clone());
        lock(&self.programs.deadlines).push(self.deadline);
        let answer = lock(&self.programs.answers).get(&line).cloned();
        match answer {
            Some(answer) => answer(&self.programs.root),
            None => Ok(LaunchctlOutput::success("")),
        }
    }
}

/// What a case of a test does to its Mac before the step runs.
type Setup<'a> = Box<dyn Fn(&Mac) + 'a>;

/// A Mac with its files under a scratch directory and its fake programs.
struct Mac {
    root: Scratch,
    programs: FakePrograms,
}

impl Mac {
    /// A Mac with an empty `/Applications`.
    fn new(name: &str) -> Mac {
        let root = scratch(name);
        fs::create_dir(root.path().join("Applications")).expect("the Applications directory");
        let programs = FakePrograms {
            root: root.path().to_owned(),
            ..FakePrograms::default()
        };
        Mac { root, programs }
    }

    fn apply(&self) -> Apply {
        Apply {
            programs: Box::new(self.programs.clone()),
            plists: Box::new(HostPlists::new(HostRoot::at(self.root.path()))),
            root: HostRoot::at(self.root.path()),
        }
    }

    /// The host's `path` under the scratch root.
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path.trim_start_matches('/'))
    }

    /// The host's file `path`, with `bytes`.
    fn file(&self, path: &str, bytes: &[u8]) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        fs::write(path, bytes).expect("the file");
    }

    /// The app at the host's path `app`, its version list naming `version` (`build`), with its
    /// own `xcodebuild`: the test's user's, each directory down to it mode 0755, whatever the
    /// umask.
    fn app(&self, app: &str, version: &str, build: &str) {
        self.file(
            &format!("{app}/Contents/version.plist"),
            plist(version, build).as_bytes(),
        );
        self.file(&format!("{app}/{XCODEBUILD}"), b"an xcodebuild");
        let mut path = app.to_owned();
        self.mode(&path, 0o755);
        for part in XCODEBUILD.split('/') {
            path = format!("{path}/{part}");
            self.mode(&path, 0o755);
        }
    }

    /// Xcode `version` (`build`) installed at `/Applications/Xcode-<version>.app`.
    fn installed(&self, version: &str, build: &str) {
        self.app(&app_path(version), version, build);
    }

    /// The staging directory as the stage makes it: the test's user's, mode 0700.
    fn trusted_staging(&self) {
        if !self.path(STAGING).exists() {
            DirBuilder::new()
                .mode(0o700)
                .create(self.path(STAGING))
                .expect("the staging directory");
        }
    }

    /// `entry` staged and verified, as the stage leaves it, its app's version list naming
    /// `version` (`build`).
    fn staged_as(&self, entry: &XcodeEntry, version: &str, build: &str) {
        self.trusted_staging();
        self.app(&staged_app(entry), version, build);
    }

    /// `entry` staged and verified, as the stage leaves it.
    fn staged(&self, entry: &XcodeEntry) {
        self.staged_as(entry, &entry.version, &entry.build);
    }

    /// The program's answer to the run `line`.
    fn answers(&self, line: &str, answer: Answer) {
        lock(&self.programs.answers).insert(line.to_owned(), answer);
    }

    /// The runs of programs so far, each `<program> <args>`.
    fn ran(&self) -> Vec<String> {
        lock(&self.programs.log).clone()
    }

    /// The deadlines of the runs so far.
    fn deadlines(&self) -> Vec<Duration> {
        lock(&self.programs.deadlines).clone()
    }

    /// The names in the host's directory `path`, sorted; none when it is not there.
    fn names(&self, path: &str) -> Vec<String> {
        let Ok(listed) = fs::read_dir(self.path(path)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = listed
            .map(|item| {
                item.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The host's paths of what `/Applications` holds, sorted.
    fn apps(&self) -> Vec<String> {
        self.names("/Applications")
            .into_iter()
            .map(|name| format!("/Applications/{name}"))
            .collect()
    }

    /// The version list of the app at the host's path `app`, as text.
    fn version_list(&self, app: &str) -> String {
        fs::read_to_string(self.path(&format!("{app}/Contents/version.plist")))
            .expect("a version list")
    }

    /// Sets the mode of the host's `path`.
    fn mode(&self, path: &str, mode: u32) {
        fs::set_permissions(self.path(path), fs::Permissions::from_mode(mode)).expect("the mode");
    }
}

/// Where the stage leaves the verified app of `entry`.
fn staged_app(entry: &XcodeEntry) -> String {
    format!("{STAGING}/{}/{STAGED_APP}", entry.sha256)
}

/// The run of `xcodebuild` of Xcode `version` with `args`.
fn xcodebuild(version: &str, args: &str) -> String {
    format!("{}/{XCODEBUILD} {args}", app_path(version))
}

/// The licence and the first launch of Xcode `version`, in that order.
fn licence_and_first_launch(version: &str) -> [String; 2] {
    [
        xcodebuild(version, "-license accept"),
        xcodebuild(version, "-runFirstLaunch"),
    ]
}

/// A program that exits `code` saying `stderr`.
fn exits(code: i32, stderr: &str) -> Answer {
    let stderr = stderr.to_owned();
    Arc::new(move |_| Ok(LaunchctlOutput::exit(code, &stderr)))
}

/// A program that cannot be started.
fn cannot_start() -> Answer {
    Arc::new(|_| {
        Err(LaunchctlError::Spawn {
            args: String::new(),
            why: "No such file or directory (os error 2)".to_owned(),
        })
    })
}

fn failure(reason: &'static str, detail: &str) -> Result<Cutover, KindError> {
    Err(KindError::Failed {
        reason,
        detail: detail.to_owned(),
    })
}

fn applied(version: &str) -> Result<Cutover, KindError> {
    Ok(Cutover::Applied {
        version: version.to_owned(),
    })
}

/// The Mac of the usual run: Xcode 26.3 kept and 25.4 to remove installed, 27.2 staged.
fn usual_mac(name: &str) -> (Mac, [XcodeEntry; 2]) {
    let mac = Mac::new(name);
    let kept = entry("26.3", "26D10");
    let new = entry("27.2", "27B52");
    mac.installed("26.3", "26D10");
    mac.installed("25.4", "25E5");
    mac.staged(&new);
    (mac, [kept, new])
}

/// Proves: the staged Xcode is moved to `/Applications/Xcode-<version>.app` beside the kept one,
/// each Xcode of the order then has its licence accepted and its first launch run by its own
/// `xcodebuild`, `xcode-select -s` names the default, in exactly that order and with each
/// program's deadline, and only then is the older Xcode the order names removed; the kind is
/// applied under the default's version. The entry's staging directory is gone after the move.
///
/// Catches: the app copied rather than renamed, or left in staging; the programs in another order
/// (a select before the licence, a first launch before the licence); the `/usr/bin` shim run in
/// place of the Xcode's own `xcodebuild`; the kept Xcode removed, or the old one kept.
#[test]
fn a_staged_xcode_is_moved_then_licensed_launched_selected_and_the_old_one_removed() {
    let (mac, xcodes) = usual_mac("xcode-apply-usual");
    let ended = mac.apply().cutover(&params(&xcodes, &["25.4"], "27.2"));
    assert_eq!(ended, applied("27.2"));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-26.3.app",
            "/Applications/Xcode-27.2.app"
        ]
    );
    assert_eq!(
        mac.version_list("/Applications/Xcode-27.2.app"),
        plist("27.2", "27B52")
    );
    assert_eq!(
        mac.version_list("/Applications/Xcode-26.3.app"),
        plist("26.3", "26D10")
    );
    assert_eq!(mac.names(STAGING), Vec::<String>::new());
    let mut expected = Vec::new();
    expected.extend(licence_and_first_launch("26.3"));
    expected.extend(licence_and_first_launch("27.2"));
    expected.push("/usr/bin/xcode-select -s /Applications/Xcode-27.2.app".to_owned());
    assert_eq!(mac.ran(), expected);
    assert_eq!(
        mac.deadlines(),
        [
            LICENSE_DEADLINE,
            FIRST_LAUNCH_DEADLINE,
            LICENSE_DEADLINE,
            FIRST_LAUNCH_DEADLINE,
            SELECT_DEADLINE
        ]
    );
}

/// Proves: a program runs only once every Xcode of the order is in place: the licence of the
/// first Xcode is accepted with the second already moved, and the removal waits for the select.
///
/// Catches: an Xcode moved after the programs of another began, or a removal before the default
/// is selected (so a failing select would leave the Mac without its old default).
#[test]
fn every_xcode_is_in_place_before_the_first_program_and_the_removal_follows_the_select() {
    let (mac, xcodes) = usual_mac("xcode-apply-in-place");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let noted = |what: &'static str, seen: &Arc<Mutex<Vec<String>>>| -> Answer {
        let seen = Arc::clone(seen);
        Arc::new(move |root: &Path| {
            lock(&seen).push(format!(
                "{what}: 27.2 {}, 25.4 {}",
                root.join("Applications/Xcode-27.2.app").exists(),
                root.join("Applications/Xcode-25.4.app").exists()
            ));
            Ok(LaunchctlOutput::success(""))
        })
    };
    mac.answers(
        &xcodebuild("26.3", "-license accept"),
        noted("licence", &seen),
    );
    mac.answers(
        "/usr/bin/xcode-select -s /Applications/Xcode-27.2.app",
        noted("select", &seen),
    );
    let ended = mac.apply().cutover(&params(&xcodes, &["25.4"], "27.2"));
    assert_eq!(ended, applied("27.2"));
    assert_eq!(
        *lock(&seen),
        [
            "licence: 27.2 true, 25.4 true",
            "select: 27.2 true, 25.4 true"
        ]
    );
}

/// Proves: an Xcode already installed at its path with its build (a cutover tried again after a
/// failure, or one already there) is not moved, and still has its licence accepted and its first
/// launch run; an order with no default selects nothing, removes nothing, and is applied under
/// its newest Xcode's version.
///
/// Catches: a second cutover refusing an Xcode the first moved, or skipping its licence; a select
/// of an empty default.
#[test]
fn an_installed_xcode_is_not_moved_and_an_order_with_no_default_selects_nothing() {
    let mac = Mac::new("xcode-apply-installed");
    mac.installed("27.2", "27B52");
    mac.installed("26.3", "26D10");
    let xcodes = [entry("27.2", "27B52"), entry("26.3", "26D10")];
    let ended = mac.apply().cutover(&params(&xcodes, &[], ""));
    assert_eq!(ended, applied("27.2"));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/Xcode-26.3.app",
            "/Applications/Xcode-27.2.app"
        ]
    );
    let mut expected = Vec::new();
    expected.extend(licence_and_first_launch("27.2"));
    expected.extend(licence_and_first_launch("26.3"));
    assert_eq!(mac.ran(), expected);
}

/// Proves: an order with no Xcode, no removal and no default reads nothing, runs nothing and is
/// applied under no version.
///
/// Catches: a cutover that runs `xcode-select` or reads `/Applications` with nothing to do.
#[test]
fn an_order_with_nothing_for_xcode_does_nothing() {
    let mac = Mac::new("xcode-apply-nothing");
    fs::remove_dir(mac.path("/Applications")).expect("no Applications directory");
    assert_eq!(mac.apply().cutover(&params(&[], &[], "")), applied(""));
    assert_eq!(mac.ran(), Vec::<String>::new());
}

/// Proves: an Xcode is moved only out of a staging directory only root can write, and only an app
/// the stage finished verifying (`Xcode.app`, not an expansion, not a link) whose version list
/// still names the order's version and build; any other fails `xcode_not_staged` naming what was
/// found, and nothing is moved, run or removed.
///
/// Catches: an app an admin of the Mac put in an open staging directory installed as Xcode; an
/// expansion `codesign` never accepted installed; a staged app of another build installed under
/// the order's version.
#[test]
fn only_a_verified_app_in_a_trusted_staging_directory_is_moved() {
    let new = entry("27.2", "27B52");
    let not_staged = |why: &str| format!("Xcode 27.2 (27B52) is not staged: {why}");
    let untrusted = "/Applications/.hostkeeper-staging is not a directory only this process's \
                     user can write";
    let staged = staged_app(&new);
    let missing = not_staged(&format!("{staged} is not a directory"));
    let cases: [(&str, Setup<'_>, String); 9] = [
        ("none", Box::new(|_| {}), not_staged(untrusted)),
        (
            "open",
            Box::new(|mac| {
                mac.staged(&new);
                mac.mode(STAGING, 0o755);
            }),
            not_staged(untrusted),
        ),
        (
            "empty",
            Box::new(|mac| mac.trusted_staging()),
            missing.clone(),
        ),
        (
            "link",
            Box::new(|mac| {
                mac.app("/elsewhere/Xcode.app", "27.2", "27B52");
                mac.trusted_staging();
                fs::create_dir(mac.path(&entry_host_dir(&new))).expect("the entry's directory");
                symlink(mac.path("/elsewhere/Xcode.app"), mac.path(&staged)).expect("a link");
            }),
            missing.clone(),
        ),
        (
            "expansion",
            Box::new(|mac| {
                mac.trusted_staging();
                mac.app(
                    &format!("{}/expand/Xcode.app", entry_host_dir(&new)),
                    "27.2",
                    "27B52",
                );
            }),
            missing.clone(),
        ),
        (
            "build",
            Box::new(|mac| mac.staged_as(&new, "27.2", "27B51")),
            not_staged(&format!("{staged} is Xcode 27.2 (27B51)")),
        ),
        (
            "version",
            Box::new(|mac| mac.staged_as(&new, "27.2.1", "27B52")),
            not_staged(&format!("{staged} is Xcode 27.2.1 (27B52)")),
        ),
        (
            "list",
            Box::new(|mac| {
                mac.trusted_staging();
                fs::create_dir_all(mac.path(&format!("{staged}/Contents"))).expect("the app");
            }),
            not_staged(&format!("{staged} has no version list")),
        ),
        (
            "garbage",
            Box::new(|mac| {
                mac.trusted_staging();
                mac.file(&format!("{staged}/Contents/version.plist"), GARBAGE);
            }),
            GARBAGE_DETAIL.replace("<app>", &staged),
        ),
    ];
    for (name, setup, detail) in cases {
        let mac = Mac::new(&format!("xcode-apply-unstaged-{name}"));
        mac.installed("25.4", "25E5");
        setup(&mac);
        let before = mac.apps();
        let ended = mac
            .apply()
            .cutover(&params(std::slice::from_ref(&new), &["25.4"], "27.2"));
        assert_eq!(ended, failure(REASON_NOT_STAGED, &detail), "{name}");
        assert_eq!(mac.ran(), Vec::<String>::new(), "{name}");
        assert_eq!(mac.apps(), before, "{name}");
        assert!(!mac.path("/Applications/Xcode-27.2.app").exists(), "{name}");
    }
}

/// Proves: every Xcode of the order is found before the first is moved: with the first staged
/// and the second not, the first stays staged and nothing runs.
///
/// Catches: an Xcode moved into place by a cutover that then fails on the next, leaving a Mac
/// with an Xcode whose licence and first launch never ran.
#[test]
fn no_xcode_is_moved_until_every_one_is_found() {
    let mac = Mac::new("xcode-apply-all-found");
    let kept = entry("26.3", "26D10");
    let new = entry("27.2", "27B52");
    mac.staged(&kept);
    let ended = mac
        .apply()
        .cutover(&params(&[kept.clone(), new], &[], "27.2"));
    assert_eq!(
        ended,
        failure(
            REASON_NOT_STAGED,
            &format!(
                "Xcode 27.2 (27B52) is not staged: {STAGING}/{}/{STAGED_APP} is not a directory",
                entry("27.2", "27B52").sha256
            )
        )
    );
    assert_eq!(mac.apps(), ["/Applications/.hostkeeper-staging"]);
    assert_eq!(mac.names(&entry_host_dir(&kept)), [STAGED_APP]);
    assert_eq!(mac.ran(), Vec::<String>::new());
}

/// Proves: the path of an Xcode of the order that holds anything but that Xcode (another build, a
/// directory with no version list, a link, a file) fails `xcode_install_taken` and is left as it
/// is, with the staged app still staged and nothing run or removed; one whose version list cannot
/// be read, or that cannot be looked at, fails `xcode_installed_unreadable`.
///
/// Catches: a hand-installed Xcode overwritten or merged into, a link followed, a rename onto a
/// file.
#[test]
fn a_path_holding_anything_else_is_left_alone() {
    let new = entry("27.2", "27B52");
    let app = "/Applications/Xcode-27.2.app";
    let taken = |what: &str| format!("{app} {what}; it is left as it is");
    let program = format!("{app}/{XCODEBUILD}");
    let not_held = |path: &str| {
        format!(
            "{path} is a link, is not this process's user's, or is writable by others; it is left \
             as it is"
        )
    };
    let cases: [(&str, Setup<'_>, &'static str, String); 9] = [
        (
            "open",
            Box::new(|mac| {
                mac.installed("27.2", "27B52");
                mac.mode(app, 0o775);
            }),
            REASON_INSTALL_TAKEN,
            not_held(app),
        ),
        (
            "program-link",
            Box::new(|mac| {
                mac.installed("27.2", "27B52");
                fs::remove_file(mac.path(&program)).expect("removed");
                symlink("/bin/sh", mac.path(&program)).expect("a link");
            }),
            REASON_INSTALL_TAKEN,
            not_held(&program),
        ),
        (
            "program-missing",
            Box::new(|mac| {
                mac.installed("27.2", "27B52");
                fs::remove_file(mac.path(&program)).expect("removed");
            }),
            REASON_INSTALL_TAKEN,
            format!(
                "cannot read {program}: No such file or directory (os error 2); it is left as it \
                 is"
            ),
        ),
        (
            "build",
            Box::new(|mac| mac.installed("27.2", "27B51")),
            REASON_INSTALL_TAKEN,
            taken("is Xcode 27.2 (27B51), not Xcode 27.2 (27B52)"),
        ),
        (
            "version",
            Box::new(|mac| mac.app(app, "27.2.1", "27B52")),
            REASON_INSTALL_TAKEN,
            taken("is Xcode 27.2.1 (27B52), not Xcode 27.2 (27B52)"),
        ),
        (
            "list",
            Box::new(|mac| fs::create_dir(mac.path(app)).expect("a directory")),
            REASON_INSTALL_TAKEN,
            taken("has no version list, so it is not Xcode 27.2 (27B52)"),
        ),
        (
            "link",
            Box::new(|mac| {
                mac.installed("26.3", "26D10");
                symlink(mac.path("/Applications/Xcode-26.3.app"), mac.path(app)).expect("a link");
            }),
            REASON_INSTALL_TAKEN,
            taken("is not a directory"),
        ),
        (
            "garbage",
            Box::new(|mac| mac.file(&format!("{app}/Contents/version.plist"), GARBAGE)),
            REASON_INSTALLED_UNREADABLE,
            GARBAGE_DETAIL.replace("<app>", app),
        ),
        (
            "file",
            Box::new(|mac| mac.file(app, b"not an app")),
            REASON_INSTALL_TAKEN,
            taken("is not a directory"),
        ),
    ];
    for (name, setup, reason, detail) in cases {
        let mac = Mac::new(&format!("xcode-apply-taken-{name}"));
        mac.staged(&new);
        setup(&mac);
        let before = mac.apps();
        let ended = mac
            .apply()
            .cutover(&params(std::slice::from_ref(&new), &[], "27.2"));
        assert_eq!(ended, failure(reason, &detail), "{name}");
        assert_eq!(mac.ran(), Vec::<String>::new(), "{name}");
        assert_eq!(mac.apps(), before, "{name}");
        assert_eq!(mac.names(&entry_host_dir(&new)), [STAGED_APP], "{name}");
    }
}

/// Proves: `/Applications` that cannot be looked in fails `xcode_installed_unreadable`, and a
/// rename the volume refuses fails `xcode_move_failed`, naming both paths; neither runs a program.
///
/// Catches: an unreadable path taken as free and an app moved over what is there, or a failed
/// rename taken as done.
#[test]
fn a_path_that_cannot_be_read_or_written_fails_naming_it() {
    let new = entry("27.2", "27B52");
    let order = params(std::slice::from_ref(&new), &[], "27.2");
    let mac = Mac::new("xcode-apply-unreadable");
    mac.staged(&new);
    mac.mode("/Applications", 0o000);
    let ended = mac.apply().cutover(&order);
    mac.mode("/Applications", 0o755);
    assert_eq!(
        ended,
        failure(
            REASON_INSTALLED_UNREADABLE,
            "cannot read /Applications/Xcode-27.2.app: Permission denied (os error 13)"
        )
    );
    mac.mode(&entry_host_dir(&new), 0o500);
    let ended = mac.apply().cutover(&order);
    mac.mode(&entry_host_dir(&new), 0o700);
    assert_eq!(
        ended,
        failure(
            REASON_MOVE_FAILED,
            &format!(
                "cannot move {} to /Applications/Xcode-27.2.app: Permission denied (os error 13)",
                staged_app(&new)
            )
        )
    );
    assert_eq!(mac.ran(), Vec::<String>::new());
    assert_eq!(mac.names(&entry_host_dir(&new)), [STAGED_APP]);
}

/// Proves: a licence, a first launch or a select that does not succeed fails the cutover with its
/// own reason and a detail naming the step, its exit code and its first line of standard error
/// (or why it could not start), runs nothing after it and removes nothing.
///
/// Catches: a failing `runFirstLaunch` taken as done and the old Xcode removed under a Mac whose
/// new one cannot build; the select run on an Xcode whose licence was refused.
#[test]
fn a_failing_program_fails_the_cutover_and_removes_nothing() {
    let first_launch = xcodebuild("27.2", "-runFirstLaunch");
    let licence = xcodebuild("26.3", "-license accept");
    let select = "/usr/bin/xcode-select -s /Applications/Xcode-27.2.app".to_owned();
    let mut through_launch: Vec<String> = Vec::new();
    through_launch.extend(licence_and_first_launch("26.3"));
    through_launch.extend(licence_and_first_launch("27.2"));
    let mut through_select = through_launch.clone();
    through_select.push(select.clone());
    let cases = [
        (
            first_launch.clone(),
            exits(1, "xcodebuild: error: packages failed to install\nmore"),
            REASON_FIRST_LAUNCH_FAILED,
            format!(
                "{first_launch} failed with exit code 1: xcodebuild: error: packages failed to \
                 install"
            ),
            through_launch.clone(),
        ),
        (
            licence.clone(),
            cannot_start(),
            REASON_LICENSE_FAILED,
            format!("{licence} could not be run: No such file or directory (os error 2)"),
            vec![licence.clone()],
        ),
        (
            select.clone(),
            exits(1, ""),
            REASON_SELECT_FAILED,
            "xcode-select -s /Applications/Xcode-27.2.app failed with exit code 1".to_owned(),
            through_select,
        ),
    ];
    for (line, answer, reason, detail, ran) in cases {
        let (mac, xcodes) = usual_mac(&format!("xcode-apply-fails-{reason}"));
        mac.answers(&line, answer);
        let ended = mac.apply().cutover(&params(&xcodes, &["25.4"], "27.2"));
        assert_eq!(ended, failure(reason, &detail), "{line}");
        assert_eq!(mac.ran(), ran, "{line}");
        assert_eq!(
            mac.apps(),
            [
                "/Applications/.hostkeeper-staging",
                "/Applications/Xcode-25.4.app",
                "/Applications/Xcode-26.3.app",
                "/Applications/Xcode-27.2.app"
            ],
            "{line}"
        );
    }
}

/// Proves: an order the apply does not take fails `xcode_order_invalid` with its reason before
/// anything is read, run, moved or removed: an Xcode whose SHA-256, version, build or size is
/// wrong, a version given twice, a default the order does not keep, a version to remove that is
/// not digits and dots or is not older than every Xcode the order keeps, and a removal with no
/// default.
///
/// Catches: a path built from a version holding `/` or `..`; the kept or the default Xcode
/// removed; a removal of what `xcode-select` names while no default moves it.
#[test]
fn an_order_the_apply_does_not_take_fails_before_anything_is_touched() {
    let new = entry("27.2", "27B52");
    let kept = entry("26.3", "26D10");
    let both = [kept.clone(), new.clone()];
    let bad_sha = XcodeEntry {
        sha256: "AB".repeat(32),
        ..new.clone()
    };
    let bad_version = XcodeEntry {
        version: "../27".to_owned(),
        ..new.clone()
    };
    let bad_build = XcodeEntry {
        build: "27-B".to_owned(),
        ..new.clone()
    };
    let empty = XcodeEntry {
        size: 0,
        ..new.clone()
    };
    let twice = XcodeEntry {
        sha256: "1".repeat(64),
        ..new.clone()
    };
    let entry_refused = "an Xcode of the order has a SHA-256, version, build or size the apply \
                         does not take";
    let cases: [(Vec<XcodeEntry>, Vec<&str>, &str, String); 11] = [
        (vec![bad_sha], vec![], "", entry_refused.to_owned()),
        (vec![bad_version], vec![], "", entry_refused.to_owned()),
        (vec![bad_build], vec![], "", entry_refused.to_owned()),
        (vec![empty], vec![], "", entry_refused.to_owned()),
        (
            vec![new.clone(), twice],
            vec![],
            "27.2",
            "the order names Xcode 27.2 twice".to_owned(),
        ),
        (
            both.to_vec(),
            vec![],
            "26.4",
            "the order's default Xcode 26.4 is not one of the Xcodes it names".to_owned(),
        ),
        (
            both.to_vec(),
            vec!["../25"],
            "27.2",
            "a version the order removes is not digits and dots".to_owned(),
        ),
        (
            both.to_vec(),
            vec!["25.4"],
            "",
            "the order removes Xcode 25.4 and names no default".to_owned(),
        ),
        (
            both.to_vec(),
            vec!["26.4"],
            "27.2",
            "the order removes Xcode 26.4, which is not older than every Xcode it keeps".to_owned(),
        ),
        (
            both.to_vec(),
            vec!["26.3"],
            "27.2",
            "the order removes Xcode 26.3, which is not older than every Xcode it keeps".to_owned(),
        ),
        (
            both.to_vec(),
            vec!["25.4", "28"],
            "27.2",
            "the order removes Xcode 28, which is not older than every Xcode it keeps".to_owned(),
        ),
    ];
    for (at, (xcodes, remove, default, detail)) in cases.into_iter().enumerate() {
        let (mac, _) = usual_mac(&format!("xcode-apply-order-{at}"));
        let before = mac.apps();
        let order = params(&xcodes, &remove, default);
        assert_eq!(
            mac.apply().cutover(&order),
            failure(REASON_ORDER_INVALID, &detail),
            "{detail}"
        );
        assert_eq!(
            mac.apply().verify(&order),
            Err(KindError::Failed {
                reason: REASON_ORDER_INVALID,
                detail: detail.clone(),
            }),
            "{detail}"
        );
        assert_eq!(mac.ran(), Vec::<String>::new(), "{detail}");
        assert_eq!(mac.apps(), before, "{detail}");
        assert_eq!(mac.names(&entry_host_dir(&new)), [STAGED_APP], "{detail}");
    }
}
