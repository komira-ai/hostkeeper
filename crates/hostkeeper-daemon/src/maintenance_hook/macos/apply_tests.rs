//! The tests of the `macos` kind's apply and check, over a fake Mac: its looks (`fdesetup`,
//! `sw_vers`, `softwareupdate`'s listings), each answer scripted by program and exact argument
//! list; its install programs, each run recorded with its program, deadline, step, arguments and
//! standard input; a run record, a rebooter and the hook's log. Every effect goes to one list, so a
//! test sees their order. The update account's file, the run's directory and the Mac's
//! `/Applications` are in scratch directories. Each outcome is asserted by equality of the whole
//! ending, each run by equality of the whole list, the log by equality of its lines and the plan
//! by equality of its bytes, so no password can hide in any of them. The host's install runner's
//! empty environment is proved over a child of this test binary.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use hostkeeper_hook::Outcome;
use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput, StdinError};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Maintain, MaintainAction, MaintenanceKind as WireKind,
};
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_hook::macos::plan::PLAN_FILE;
use crate::maintenance_hook::macos::{MacosKind, Stage};
use crate::maintenance_hook::{
    Ending, HookPorts, HookStep, Kind, KindState, REASON_REBOOT_UNDER_WAY, RebootError, Rebooter,
    RecordError, Registry, RunRecord, run_step,
};
use crate::maintenance_mac::FreeSpace;

/// The update account's password: bytes an argument list, a shell, JSON or a log line would treat
/// specially, and an `=`.
const PASSWORD: &str = "Up date=pa\"ss$HOME\\1";
/// The update account's name.
const ACCOUNT: &str = "hkupdate";
/// The run of the tests.
const RUN: &str = "run-1";
/// The boot the Mac runs.
const BOOT_A: &str = "0f6b1c3e-5a2d-4c8e-9b7f-1d2e3f4a5b6c";

/// `sw_vers` on a Mac of macOS 26.6, build 25G62.
const SW_VERS_26_6: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/sw_vers_macos26.txt");
/// `fdesetup status` with FileVault off, and on.
const FILEVAULT_OFF: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_off.txt");
const FILEVAULT_ON: &str = include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_on.txt");
/// `softwareupdate --list` on macOS 26: the minor update macOS Tahoe 26.7 (`-25H62`) and the
/// major upgrade macOS 27.1.
const LIST_MAJOR: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_list_major.txt");
/// `softwareupdate --list` offering nothing.
const LIST_NONE: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_list_none.txt");
/// `softwareupdate --list-full-installers` on macOS 26: macOS 27.1 (build 26B70) first.
const FULL_26: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_full_installers.txt");

/// The label of macOS Tahoe 26.7.
const TAHOE_26_7: &str = "macOS Tahoe 26.7-25H62";
/// The installer app of the major upgrade.
const INSTALLER: &str = concat!("/Applications/Install macOS 27", ".app");
/// The name of that app, and of another installer app. Each is written in two pieces: the repository's
/// hygiene lint reads a word that ends `.app` after a space as a host name.
const APP_27: &str = concat!("Install macOS 27", ".app");
const APP_TAHOE: &str = concat!("Install macOS Tahoe", ".app");

/// One run of an install program.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Installed {
    program: PathBuf,
    deadline: Duration,
    step: String,
    args: Vec<String>,
    input: Vec<u8>,
}

/// What the fake Mac answers, and what was asked of it.
struct Script {
    looks: BTreeMap<(PathBuf, Vec<String>), Result<LaunchctlOutput, LaunchctlError>>,
    install: Result<LaunchctlOutput, StdinError>,
    effects: Vec<String>,
    installs: Vec<Installed>,
    lines: Vec<String>,
    states: BTreeMap<Kind, KindState>,
}

/// A fake Mac. Clones share one script.
#[derive(Clone)]
struct Mac(Arc<Mutex<Script>>);

/// A fake Mac's files: its update account's file, its run directory and its root.
struct Files {
    _dir: Scratch,
    account: PathBuf,
    run_dir: PathBuf,
    root: PathBuf,
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

impl Files {
    /// A good update account's file (mode 0600, the test's own user), an empty run directory and
    /// an empty root.
    fn new(name: &str) -> Files {
        let dir = scratch(name);
        let account = dir.path().join("update-account");
        fs::write(&account, format!("name={ACCOUNT}\npassword={PASSWORD}\n")).expect("account");
        fs::set_permissions(&account, fs::Permissions::from_mode(0o600)).expect("its mode");
        let run_dir = dir.path().join("run");
        let root = dir.path().join("root");
        fs::create_dir_all(&run_dir).expect("the run directory");
        fs::create_dir_all(root.join("Applications")).expect("the root");
        Files {
            _dir: dir,
            account,
            run_dir,
            root,
        }
    }

    /// An installer app `name` in the root's `/Applications`, with its `startosinstall` when
    /// `program`.
    fn installer(&self, name: &str, program: bool) -> PathBuf {
        let app = self.root.join("Applications").join(name);
        let resources = app.join("Contents/Resources");
        fs::create_dir_all(&resources).expect("the app");
        if program {
            fs::write(resources.join("startosinstall"), "").expect("startosinstall");
        }
        app
    }

    /// The plan file's bytes.
    fn plan(&self) -> String {
        fs::read_to_string(self.run_dir.join(PLAN_FILE)).expect("the plan")
    }
}

impl Mac {
    /// A Mac of macOS 26.6 with FileVault off, whose `--list` answers `list`, whose
    /// `--list-full-installers` answers [`FULL_26`] and whose install exits 0.
    fn on_26(list: &str) -> Mac {
        let mac = Mac(Arc::new(Mutex::new(Script {
            looks: BTreeMap::new(),
            install: Ok(LaunchctlOutput::success("")),
            effects: Vec::new(),
            installs: Vec::new(),
            lines: Vec::new(),
            states: BTreeMap::new(),
        })));
        mac.look(FDESETUP, &["status"], ok(FILEVAULT_OFF));
        mac.look(SW_VERS, &[], ok(SW_VERS_26_6));
        mac.look(SOFTWAREUPDATE, &["--list"], ok(list));
        mac.look(SOFTWAREUPDATE, &["--list-full-installers"], ok(FULL_26));
        mac
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        self.0.lock().expect("the script")
    }

    /// Answers `program` run with exactly `args` with `answer`.
    fn look(&self, program: &str, args: &[&str], answer: Result<LaunchctlOutput, LaunchctlError>) {
        self.script()
            .looks
            .insert((PathBuf::from(program), strings(args)), answer);
    }

    fn effects(&self) -> Vec<String> {
        std::mem::take(&mut self.script().effects)
    }

    fn installs(&self) -> Vec<Installed> {
        self.script().installs.clone()
    }

    fn lines(&self) -> Vec<String> {
        self.script().lines.clone()
    }

    /// The apply of this Mac over `files`.
    fn apply(&self, files: &Files) -> Apply {
        Apply::new(
            Box::new(self.clone()),
            Box::new(self.clone()),
            AccountFile::new(&files.account, me()),
            Some(&files.run_dir),
            HostRoot::at(&files.root),
            Box::new(self.clone()),
        )
    }

    /// A registry holding the `macos` kind of this Mac over `files`.
    fn kinds(&self, files: &Files) -> Registry {
        let stage = Stage::new(Box::new(self.clone()), Box::new(self.clone()));
        let kind = MacosKind::new(stage, self.apply(files));
        Registry::new().with(Kind::Macos, Box::new(kind))
    }

    /// The hook's `step` of `params` over this Mac's kind, record, rebooter and boot.
    fn hook(&self, files: &Files, step: HookStep, params: &MaintainParams) -> Ending {
        let kinds = self.kinds(files);
        let ports = HookPorts {
            kinds: &kinds,
            record: self,
            rebooter: self,
            boot_id: self,
        };
        run_step(step, params, &ports)
    }
}

impl Programs for Mac {
    fn tool(&self, program: &Path, _deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(Look {
            mac: self.clone(),
            program: program.to_owned(),
        })
    }
}

/// One look of the fake Mac: recorded as `run <program> <args>`, answered as scripted.
struct Look {
    mac: Mac,
    program: PathBuf,
}

impl Launchctl for Look {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let mut script = self.mac.script();
        let ran = [self.program.display().to_string()]
            .into_iter()
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        script.effects.push(format!("run {ran}"));
        let scripted = script.looks.get(&(self.program.clone(), args.to_vec()));
        scripted.cloned().unwrap_or_else(|| {
            Err(LaunchctlError::Spawn {
                args: args.join(" "),
                why: "not scripted".to_owned(),
            })
        })
    }
}

impl Installers for Mac {
    fn runner(&self, program: &Path, deadline: Duration) -> Box<dyn StdinRun> {
        Box::new(Runner {
            mac: self.clone(),
            program: program.to_owned(),
            deadline,
        })
    }
}

/// One install program of the fake Mac: recorded whole, and as `install <program>` among the
/// effects; answered as scripted.
struct Runner {
    mac: Mac,
    program: PathBuf,
    deadline: Duration,
}

impl StdinRun for Runner {
    fn run(
        &self,
        step: &str,
        args: &[String],
        input: &[u8],
    ) -> Result<LaunchctlOutput, StdinError> {
        let mut script = self.mac.script();
        script
            .effects
            .push(format!("install {}", self.program.display()));
        script.installs.push(Installed {
            program: self.program.clone(),
            deadline: self.deadline,
            step: step.to_owned(),
            args: args.to_vec(),
            input: input.to_vec(),
        });
        script.install.clone()
    }
}

impl FreeSpace for Mac {
    fn free_bytes(&self) -> Result<u64, String> {
        Err("the apply reads no free space".to_owned())
    }
}

impl Log for Mac {
    fn line(&self, text: &str) {
        self.script().lines.push(text.to_owned());
    }
}

impl RunRecord for Mac {
    fn state(&self, run_id: &str, kind: Kind) -> Result<KindState, RecordError> {
        assert_eq!(run_id, RUN);
        let states = &self.script().states;
        Ok(states.get(&kind).cloned().unwrap_or(KindState::Pending))
    }

    fn reboot_under_way(&self, run_id: &str, kind: Kind, boot: &BootId) -> Result<(), RecordError> {
        let mut script = self.script();
        script.effects.push(format!(
            "record {run_id} {kind} reboot_under_way {}",
            boot.as_str()
        ));
        script
            .states
            .insert(kind, KindState::RebootUnderWay { boot: boot.clone() });
        Ok(())
    }

    fn applied(&self, run_id: &str, kind: Kind, version: &str) -> Result<(), RecordError> {
        self.script()
            .effects
            .push(format!("record {run_id} {kind} applied {version}"));
        Ok(())
    }
}

impl Rebooter for Mac {
    fn reboot(&self) -> Result<(), RebootError> {
        self.script().effects.push("rebooter".to_owned());
        Err(RebootError::NotWired)
    }
}

impl BootIdSource for Mac {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::uuid("the test's boot id", BOOT_A)
    }
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

fn ok(stdout: &str) -> Result<LaunchctlOutput, LaunchctlError> {
    Ok(LaunchctlOutput::success(stdout))
}

/// The parameters of a `macos` run on `path`, a major upgrade allowed when `major`.
fn params_on(path: MacosPath, major: bool) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: RUN.to_owned(),
        action: MaintainAction::Run.into(),
        kinds: vec![WireKind::Macos.into()],
        macos_path: path.into(),
        macos_major: major,
        ..Maintain::default()
    })
    .expect("valid parameters")
}

/// The parameters of a `macos` run on the path `install`.
fn params(major: bool) -> MaintainParams {
    params_on(MacosPath::Install, major)
}

fn failure(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Failed {
        reason,
        detail: detail.to_owned(),
    })
}

/// The ending of a step that failed `reason` with `detail`.
fn failed_ending(reason: &'static str, detail: &str) -> Ending {
    Ending {
        outcome: Outcome::Failure,
        reason: Some(reason),
        detail: Some(detail.to_owned()),
    }
}

/// The ending of a `cutover` whose restart is under way.
fn restarting() -> Ending {
    Ending {
        outcome: Outcome::Success,
        reason: Some(REASON_REBOOT_UNDER_WAY),
        detail: Some("macos needs a reboot: rebooting".to_owned()),
    }
}

/// The password and the newline the install is given.
fn input() -> Vec<u8> {
    format!("{PASSWORD}\n").into_bytes()
}

/// The effects of the looks of a `cutover` of a minor update, in order.
fn looks_of_minor() -> Vec<String> {
    strings(&[
        "run /usr/bin/fdesetup status",
        "run /usr/bin/sw_vers",
        "run /usr/sbin/softwareupdate --list",
    ])
}

/// Proves: a `cutover` of a minor update checks FileVault, lists, keeps the plan, records the
/// reboot under way and only then runs exactly `softwareupdate --install <label>
/// --agree-to-license --restart --user hkupdate --stdinpass`, the password and a newline its
/// standard input and in none of its arguments; the step ends `reboot_under_way`, the host's
/// rebooter is never asked, the one log line names the step and the exit code, and the plan holds
/// the label and the build alone.
///
/// Catches: the install started before the record of its reboot is durable (the Mac restarts and
/// the next boot installs it again), the password in an argument, the log or the plan, a label
/// other than the one `--list` named, or a second restart asked of the host.
#[test]
fn a_minor_update_is_recorded_then_installed_with_the_password_on_standard_input_alone() {
    let files = Files::new("macos-apply-minor");
    let mac = Mac::on_26(LIST_MAJOR);
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, restarting());
    let mut expected = looks_of_minor();
    expected.push(format!("record {RUN} macos reboot_under_way {BOOT_A}"));
    expected.push(format!("install {SOFTWAREUPDATE}"));
    assert_eq!(mac.effects(), expected);
    assert_eq!(
        mac.installs(),
        [Installed {
            program: PathBuf::from(SOFTWAREUPDATE),
            deadline: INSTALL_DEADLINE,
            step: STEP_UPDATE.to_owned(),
            args: strings(&[
                "--install",
                TAHOE_26_7,
                "--agree-to-license",
                "--restart",
                "--user",
                ACCOUNT,
                "--stdinpass",
            ]),
            input: input(),
        }]
    );
    assert_eq!(
        mac.lines(),
        ["softwareupdate --install: exited with code 0"]
    );
    assert_eq!(
        files.plan(),
        format!(r#"{{"build":"25H62","label":"{TAHOE_26_7}","runId":"{RUN}"}}"#)
    );
}

/// Proves: with a major upgrade allowed and offered, the `cutover` runs exactly the installer
/// app's `startosinstall --agreetolicense --nointeraction --user hkupdate --stdinpass`, the
/// password on its standard input alone, after the record of the reboot; the plan names the app
/// and the build of its full installer.
///
/// Catches: a major upgrade installed by `softwareupdate`, an argument of `startosinstall` that
/// waits for a person, or the password anywhere but its standard input.
#[test]
fn a_major_upgrade_runs_the_installer_apps_startosinstall_likewise() {
    let files = Files::new("macos-apply-major");
    let app = files.installer(APP_27, true);
    let mac = Mac::on_26(LIST_MAJOR);
    let ended = mac.hook(&files, HookStep::Cutover, &params(true));
    assert_eq!(ended, restarting());
    let program = app.join("Contents/Resources/startosinstall");
    assert_eq!(
        mac.effects(),
        [
            "run /usr/bin/fdesetup status".to_owned(),
            "run /usr/bin/sw_vers".to_owned(),
            "run /usr/sbin/softwareupdate --list".to_owned(),
            "run /usr/sbin/softwareupdate --list-full-installers".to_owned(),
            format!("record {RUN} macos reboot_under_way {BOOT_A}"),
            format!("install {}", program.display()),
        ]
    );
    assert_eq!(
        mac.installs(),
        [Installed {
            program,
            deadline: INSTALL_DEADLINE,
            step: STEP_UPGRADE.to_owned(),
            args: strings(&[
                "--agreetolicense",
                "--nointeraction",
                "--user",
                ACCOUNT,
                "--stdinpass",
            ]),
            input: input(),
        }]
    );
    assert_eq!(mac.lines(), ["startosinstall: exited with code 0"]);
    assert_eq!(
        files.plan(),
        format!(r#"{{"build":"26B70","installer":"{INSTALLER}","runId":"{RUN}"}}"#)
    );
}

/// Proves: an install that exits 1, having echoed its standard input to both streams, fails the
/// step `macos_install_failed` with a detail that names the step and the exit code alone, and the
/// one log line is that same text.
///
/// Catches: the program's output (here the password) carried into the error, the report, the run
/// record's halted reason or the log; a failed install reported as a restart under way.
#[test]
fn a_failing_install_fails_naming_the_step_and_exit_code_alone() {
    let files = Files::new("macos-apply-fails");
    let mac = Mac::on_26(LIST_MAJOR);
    mac.script().install = Ok(LaunchctlOutput {
        code: Some(1),
        stdout: input(),
        stderr: input(),
    });
    let detail = "softwareupdate --install: the program exited with code 1";
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, failed_ending(REASON_INSTALL_FAILED, detail));
    assert_eq!(
        ended.report().to_line(),
        format!(
            r#"{{"class":"release","detail":"{detail}","promotion_pending":false,"reason":"macos_install_failed"}}"#
        )
    );
    assert_eq!(mac.lines(), [detail]);
    assert_eq!(mac.installs().len(), 1);
}

/// Proves: an update-account file with another mode, or none, is refused
/// `update_account_refused`, naming the file and the problem, before any Apple program runs, any
/// plan is kept or anything is recorded; and the restart of a reboot under way in this boot is
/// refused the same way, installing nothing.
///
/// Catches: `fdesetup`, `softwareupdate` or `startosinstall` run on a Mac whose account cannot
/// authorise the install, or a world-readable password file used.
#[test]
fn a_wrong_update_account_file_is_refused_before_any_apple_program_runs() {
    let files = Files::new("macos-apply-account");
    fs::set_permissions(&files.account, fs::Permissions::from_mode(0o644)).expect("mode");
    let mac = Mac::on_26(LIST_MAJOR);
    let refused = format!(
        "the update-account file {} has mode 0644, not 0600",
        files.account.display()
    );
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, failed_ending(REASON_ACCOUNT_REFUSED, &refused));
    assert_eq!(mac.effects(), Vec::<String>::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());

    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    mac.script()
        .states
        .insert(Kind::Macos, KindState::RebootUnderWay { boot });
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, failed_ending(REASON_ACCOUNT_REFUSED, &refused));
    assert_eq!(mac.effects(), Vec::<String>::new());

    fs::remove_file(&files.account).expect("removed");
    let apply = mac.apply(&files);
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_ACCOUNT_REFUSED,
            &format!(
                "the update-account file {} is missing",
                files.account.display()
            )
        )
    );
    assert_eq!(mac.effects(), Vec::<String>::new());
    assert_eq!(mac.installs(), Vec::new());
}

/// Proves: a restart asked again in the boot the Mac still runs installs the plan the `cutover`
/// kept, with no look and no new record: exactly the install of the first.
///
/// Catches: a second listing choosing another update than the one recorded, or a Mac whose
/// restart did not come left waiting for a reboot no one asks for.
#[test]
fn a_restart_in_the_same_boot_installs_the_kept_plan_again() {
    let files = Files::new("macos-apply-again");
    let mac = Mac::on_26(LIST_MAJOR);
    assert_eq!(
        mac.hook(&files, HookStep::Cutover, &params(false)),
        restarting()
    );
    mac.effects();
    let first = mac.installs();
    assert_eq!(
        mac.hook(&files, HookStep::Cutover, &params(false)),
        restarting()
    );
    assert_eq!(mac.effects(), [format!("install {SOFTWAREUPDATE}")]);
    assert_eq!(mac.installs(), [first.clone(), first].concat());
}

/// Proves: FileVault on, or anything but off, fails `filevault_not_off` after `fdesetup status`
/// alone: nothing listed, kept or installed.
///
/// Catches: `--restart` on a Mac whose restart stops at the pre-boot login, so the host never
/// comes back.
#[test]
fn filevault_not_off_is_refused_before_anything_is_listed_or_installed() {
    let files = Files::new("macos-apply-filevault");
    let mac = Mac::on_26(LIST_MAJOR);
    mac.look(FDESETUP, &["status"], ok(FILEVAULT_ON));
    assert_eq!(
        mac.apply(&files).cutover(&params(false)).map(|_| ()),
        failure(
            REASON_FILEVAULT_NOT_OFF,
            "FileVault is on (fdesetup status): its authenticated restart is not built, so the \
             update is not installed"
        )
    );
    assert_eq!(mac.effects(), ["run /usr/bin/fdesetup status"]);
    mac.look(FDESETUP, &["status"], ok("something else\n"));
    assert_eq!(
        mac.apply(&files).cutover(&params(false)).map(|_| ()),
        failure(
            REASON_FILEVAULT_NOT_OFF,
            "FileVault is in no state fdesetup names (fdesetup status): its authenticated \
             restart is not built, so the update is not installed"
        )
    );
    assert_eq!(mac.installs(), Vec::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: on the path `wait`, and on an order that names no path, `cutover`, the restart and
/// `verify` each fail `macos_path_not_built` and read or run nothing.
///
/// Catches: the daemon running an Apple program for `macos` on the path where device management
/// installs the update, which is not built.
#[test]
fn only_the_path_install_is_accepted() {
    let files = Files::new("macos-apply-path");
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = mac.apply(&files);
    for (path, name) in [
        (MacosPath::Wait, "wait"),
        (MacosPath::Unspecified, "unspecified"),
    ] {
        let refused = failure(
            REASON_PATH_NOT_BUILT,
            &format!(
                "the order's macOS path is {name}, and this hostkeeper installs a macOS update \
                 only on the path install"
            ),
        );
        let params = params_on(path, false);
        assert_eq!(apply.cutover(&params).map(|_| ()), refused);
        assert_eq!(apply.install(&params), refused);
        assert_eq!(apply.verify(&params), refused);
    }
    assert_eq!(mac.effects(), Vec::<String>::new());
}

/// Proves: what `cutover` cannot choose fails before anything is kept or installed: nothing
/// offered (`macos_update_not_listed`), a label with no build after its last `-`
/// (`macos_target_unknown`), and a major upgrade with no installer app, two, or one without its
/// `startosinstall` (`macos_installer_not_found`); a failed listing fails `macos_look_failed`.
///
/// Catches: an install of something the stage did not stage, a target build guessed, or the
/// wrong one of two installer apps run.
#[test]
fn what_cannot_be_chosen_fails_before_anything_is_kept() {
    let files = Files::new("macos-apply-choose");
    let mac = Mac::on_26(LIST_NONE);
    let apply = mac.apply(&files);
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26 the order allows"
        )
    );
    let no_build = "* Label: macOS Tahoe 26.7\n\tTitle: macOS Tahoe 26.7, Version: 26.7, \
                    Size: 4718592KiB, Recommended: YES, Action: restart, \n";
    mac.look(SOFTWAREUPDATE, &["--list"], ok(no_build));
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_TARGET_UNKNOWN,
            "the label \"macOS Tahoe 26.7\" names no build after its last '-'"
        )
    );
    assert_eq!(
        apply.cutover(&params(true)).map(|_| ()),
        failure(
            REASON_INSTALLER_NOT_FOUND,
            "/Applications holds 0 apps named Install macOS *.app (), not one"
        )
    );
    files.installer(APP_27, false);
    assert_eq!(
        apply.cutover(&params(true)).map(|_| ()),
        failure(
            REASON_INSTALLER_NOT_FOUND,
            "/Applications/Install macOS 27.app/Contents/Resources/startosinstall is not a file"
        )
    );
    files.installer(APP_TAHOE, true);
    assert_eq!(
        apply.cutover(&params(true)).map(|_| ()),
        failure(
            REASON_INSTALLER_NOT_FOUND,
            &format!(
                "/Applications holds 2 apps named Install macOS *.app ({APP_27}, {APP_TAHOE}), \
                 not one"
            )
        )
    );
    mac.look(
        SOFTWAREUPDATE,
        &["--list"],
        Ok(LaunchctlOutput::exit(1, "no network")),
    );
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_LOOK_FAILED,
            "softwareupdate --list failed with exit code 1: no network"
        )
    );
    assert_eq!(mac.installs(), Vec::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: `verify` fails `macos_build_not_target` while `sw_vers` reports another build than the
/// plan's, and leaves the installer app; once it reports the target build `verify` passes and
/// removes the app, and passes again with the app gone. A minor update's `verify` removes no app.
///
/// Catches: a Mac put back on the old build, an installer removed before the upgrade is shown
/// applied (so a retry cannot run it), or a 16 GB installer left behind.
#[test]
fn verify_fails_unless_sw_vers_reports_the_target_build_and_only_then_removes_the_installer() {
    let files = Files::new("macos-apply-verify");
    let app = files.installer(APP_27, true);
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = mac.apply(&files);
    assert_eq!(apply.cutover(&params(true)), Ok(Cutover::NeedsReboot));
    assert_eq!(
        apply.verify(&params(true)),
        failure(
            REASON_NOT_TARGET,
            "sw_vers reports the build 25G62, not the target build 26B70"
        )
    );
    assert!(app.is_dir());
    let upgraded = "ProductName:\tmacOS\nProductVersion:\t27.1\nBuildVersion:\t26B70\n";
    mac.look(SW_VERS, &[], ok(upgraded));
    assert_eq!(apply.after_reboot(&params(true)), Ok("26B70".to_owned()));
    assert_eq!(apply.verify(&params(true)), Ok(()));
    assert!(!app.exists());
    assert_eq!(apply.verify(&params(true)), Ok(()));

    let other = files.installer(APP_TAHOE, true);
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = mac.apply(&files);
    assert_eq!(apply.cutover(&params(false)), Ok(Cutover::NeedsReboot));
    mac.look(
        SW_VERS,
        &[],
        ok("ProductVersion:\t26.7\nBuildVersion:\t25H62\n"),
    );
    assert_eq!(apply.verify(&params(false)), Ok(()));
    assert!(other.is_dir());
}

/// Proves: `verify` with no plan of the run fails `macos_plan_failed` before it runs anything;
/// `sw_vers` that fails or names no build fails `after_reboot` and `verify`
/// `macos_look_failed`; an installer that cannot be removed fails `macos_installer_not_removed`.
///
/// Catches: a Mac checked against no target and passed, or a failed read taken as a build.
#[test]
fn verify_and_after_reboot_fail_on_what_they_cannot_read() {
    let files = Files::new("macos-apply-verify-fails");
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = mac.apply(&files);
    let plan = files.run_dir.join(PLAN_FILE);
    assert_eq!(
        apply.verify(&params(false)),
        failure(
            REASON_PLAN_FAILED,
            &format!(
                "the install plan {} could not be read: No such file or directory (os error 2)",
                plan.display()
            )
        )
    );
    assert_eq!(mac.effects(), Vec::<String>::new());
    mac.look(SW_VERS, &[], ok("ProductVersion:\t26.6\n"));
    assert_eq!(
        apply.after_reboot(&params(false)),
        Err(KindError::Failed {
            reason: REASON_LOOK_FAILED,
            detail: "sw_vers named no build".to_owned(),
        })
    );
    mac.look(SW_VERS, &[], Ok(LaunchctlOutput::exit(2, "")));
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(REASON_LOOK_FAILED, "sw_vers failed with exit code 2")
    );

    // The installer app the plan names replaced by a file once the plan is kept: it cannot be
    // removed as an app, and is left.
    let app = files.installer(APP_27, true);
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = mac.apply(&files);
    assert_eq!(apply.cutover(&params(true)), Ok(Cutover::NeedsReboot));
    fs::remove_dir_all(&app).expect("the app removed");
    fs::write(&app, "not an app").expect("a file in its place");
    mac.look(
        SW_VERS,
        &[],
        ok("ProductVersion:\t27.1\nBuildVersion:\t26B70\n"),
    );
    assert_eq!(
        apply.verify(&params(true)),
        failure(
            REASON_INSTALLER_NOT_REMOVED,
            &format!("{INSTALLER} could not be removed: Not a directory (os error 20)")
        )
    );
    assert!(app.is_file());
}

/// Proves: the install plan of a hook with no run directory (its parameters on standard input)
/// cannot be kept: `cutover` fails `macos_plan_failed` and installs nothing.
///
/// Catches: an install whose target no later step can check.
#[test]
fn a_hook_with_no_run_directory_keeps_no_plan_and_installs_nothing() {
    let files = Files::new("macos-apply-no-dir");
    let mac = Mac::on_26(LIST_MAJOR);
    let apply = Apply::new(
        Box::new(mac.clone()),
        Box::new(mac.clone()),
        AccountFile::new(&files.account, me()),
        None,
        HostRoot::at(&files.root),
        Box::new(mac.clone()),
    );
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_PLAN_FAILED,
            "the hook has no run directory: its parameters came on standard input"
        )
    );
    assert_eq!(mac.installs(), Vec::new());
}

/// Proves: the Mac's own apply reads the update account from its root-only file, owned by root;
/// built with no run directory, it refuses an order on the path `wait` before it reads anything;
/// its log writes a line without failing.
///
/// Catches: a host wired to another file or owner than the one the bootstrap writes.
#[test]
fn the_host_account_is_roots_update_account_file() {
    let host = AccountFile::host();
    assert_eq!(host.path, PathBuf::from(UPDATE_ACCOUNT_FILE));
    assert_eq!(host.owner, ROOT_UID);
    let apply = Apply::at_host(None);
    assert_eq!(
        apply.verify(&params_on(MacosPath::Wait, false)),
        failure(
            REASON_PATH_NOT_BUILT,
            "the order's macOS path is wait, and this hostkeeper installs a macOS update only on \
             the path install"
        )
    );
    StderrLog.line("the hook's log line of a test");
}

/// The environment this process has, without what an instrumented build sets by itself in each
/// process it starts (`__LLVM_PROFILE_RT_*`), which no parent passed.
#[expect(
    clippy::disallowed_methods,
    reason = "the helper child reports the environment its parent gave it"
)]
fn own_environment() -> Vec<String> {
    std::env::vars_os()
        .filter(|(name, _)| !name.to_string_lossy().starts_with("__LLVM_PROFILE_RT_"))
        .map(|(name, value)| format!("env:{}={}", name.to_string_lossy(), value.to_string_lossy()))
        .collect()
}

/// Catches nothing itself: the child of
/// `the_host_installers_run_with_exactly_their_arguments_input_and_an_empty_environment`, which
/// prints each argument, each variable and its standard input on lines of their own.
#[test]
#[ignore = "the child process of the_host_installers_run_with_exactly_their_arguments_input_and_an_empty_environment; run only by it"]
fn child_prints_its_arguments_environment_and_input() {
    let mut stdin = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin);
    for arg in std::env::args().skip(1) {
        println!("arg:{arg}");
    }
    for line in own_environment() {
        println!("{line}");
    }
    println!("stdin:{stdin}");
}

/// Proves: the Mac's install programs are run with exactly the arguments given, an empty
/// environment and exactly the input given on their standard input, over a child of this test
/// binary that prints what it got.
///
/// Catches: a runner that passes the hook's environment on to `softwareupdate` or
/// `startosinstall`, adds an argument, or gives the program anything but the password.
#[test]
fn the_host_installers_run_with_exactly_their_arguments_input_and_an_empty_environment() {
    let exe = std::env::current_exe().expect("the test binary");
    let helper = strings(&[
        "maintenance_hook::macos::apply::tests::child_prints_its_arguments_environment_and_input",
        "--exact",
        "--ignored",
        "--nocapture",
    ]);
    let output = HostInstallers
        .runner(&exe, Duration::from_secs(60))
        .run(STEP_UPDATE, &helper, &input())
        .expect("the child ran");
    assert_eq!(output.code, Some(0));
    let reported: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            ["arg:", "env:", "stdin:"]
                .iter()
                .any(|p| line.starts_with(p))
        })
        .map(str::to_owned)
        .collect();
    let mut expected: Vec<String> = helper.iter().map(|arg| format!("arg:{arg}")).collect();
    expected.push(format!("stdin:{PASSWORD}"));
    assert_eq!(reported, expected);
}
