//! The linking side's unit control over launchd: which `launchctl` commands each call issues, the
//! snapshot of the placed links that `daemon_reload` compares, and that a failing `launchctl` is
//! always an error. The launchctl behind it is the fake, and the unit directory a scratch one.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::launchctl::{
    LaunchctlOutput, bootout_args, bootstrap_args, kickstart_args, print_args,
};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchdUnitControl};
use hostkeeper_placed::unit_control::{UnitControl, UnitControlError};
use hostkeeper_testkit::Scratch;

const WEB: &str = "dev.hostkeeper.web-main";
const DB: &str = "dev.hostkeeper.db";

/// A unit directory with two placed links and two version directories holding their files.
struct Rig {
    _root: Scratch,
    units: PathBuf,
    v1: PathBuf,
    v2: PathBuf,
}

fn rig() -> Rig {
    let root = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "unit-control")
        .expect("a scratch directory");
    let (units, v1, v2) = (
        root.path().join("units"),
        root.path().join("v1"),
        root.path().join("v2"),
    );
    for dir in [&units, &v1, &v2] {
        std::fs::create_dir(dir).expect("a directory");
    }
    for dir in [&v1, &v2] {
        for label in [WEB, DB] {
            std::fs::write(dir.join(format!("{label}.plist")), label).expect("a plist");
        }
    }
    let rig = Rig {
        _root: root,
        units,
        v1,
        v2,
    };
    symlink(rig.v1.join(format!("{WEB}.plist")), rig.link(WEB)).expect("a link");
    symlink(rig.v2.join(format!("{DB}.plist")), rig.link(DB)).expect("a link");
    rig
}

impl Rig {
    fn link(&self, label: &str) -> PathBuf {
        self.units.join(format!("{label}.plist"))
    }

    /// Makes the link of `label` point at the file of the same name in `dir`.
    fn repoint(&self, label: &str, dir: &Path) {
        std::fs::remove_file(self.link(label)).expect("remove the link");
        symlink(dir.join(format!("{label}.plist")), self.link(label)).expect("a link");
    }

    fn control(&self, fake: &FakeLaunchctl) -> LaunchdUnitControl<FakeLaunchctl> {
        LaunchdUnitControl::new(fake.clone(), &self.units).expect("a control")
    }

    fn bootstrap(&self, label: &str) -> Vec<String> {
        bootstrap_args(&self.link(label))
    }
}

fn label(text: &str) -> Label {
    Label::parse(text).expect("a valid label")
}

fn printed(label: &str, lines: &[&str]) -> String {
    let body: String = lines.iter().map(|line| format!("\t{line}\n")).collect();
    format!("system/{label} = {{\n{body}}}\n")
}

/// `label` is loaded and running on every print.
fn loaded(fake: &FakeLaunchctl, name: &str) {
    let text = printed(name, &["state = running", "pid = 7"]);
    fake.always(
        &print_args(&label(name)),
        Ok(LaunchctlOutput::success(&text)),
    );
}

fn waiting(fake: &FakeLaunchctl, name: &str) {
    let text = printed(name, &["state = waiting", "last exit code = 0"]);
    fake.always(
        &print_args(&label(name)),
        Ok(LaunchctlOutput::success(&text)),
    );
}

fn not_loaded(fake: &FakeLaunchctl, name: &str) {
    fake.always(&print_args(&label(name)), Ok(LaunchctlOutput::not_loaded()));
}

/// Every bootout of `name` exits 0.
fn bootout_ok(fake: &FakeLaunchctl, name: &str) {
    fake.always(
        &bootout_args(&label(name)),
        Ok(LaunchctlOutput::success("")),
    );
}

fn bootstrap_ok(fake: &FakeLaunchctl, rig: &Rig, name: &str) {
    fake.always(&rig.bootstrap(name), Ok(LaunchctlOutput::success("")));
}

fn error(action: &str, why: &str) -> UnitControlError {
    UnitControlError {
        action: action.to_owned(),
        why: why.to_owned(),
    }
}

fn print_call(name: &str) -> Vec<String> {
    print_args(&label(name))
}

/// What `bootout` issues for a label that is loaded and boots out: a print, then the command.
fn bootout_calls(name: &str) -> Vec<Vec<String>> {
    vec![print_call(name), bootout_args(&label(name))]
}

/// Catches a reload that touches the wrong jobs: only the link that was re-pointed is printed,
/// booted out and bootstrapped from its plist path; the unchanged link gets no command, and a
/// reload with nothing changed gets none at all.
#[test]
fn launchd_unit_control_issues_the_expected_launchctl_commands() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    loaded(&fake, WEB);
    bootout_ok(&fake, WEB);
    bootstrap_ok(&fake, &rig, WEB);

    rig.repoint(WEB, &rig.v2);
    control.daemon_reload().expect("the reload");

    let mut expected = vec![print_call(WEB)];
    expected.extend(bootout_calls(WEB));
    expected.push(vec![
        "bootstrap".to_owned(),
        "system".to_owned(),
        rig.link(WEB).display().to_string(),
    ]);
    assert_eq!(fake.calls(), expected);

    control.daemon_reload().expect("the second reload");
    assert_eq!(
        fake.calls(),
        expected,
        "nothing changed, so nothing is issued"
    );

    let other = FakeLaunchctl::new();
    let control = rig.control(&other);
    not_loaded(&other, WEB);
    control.stop_unit(WEB).expect("a stop of a job not loaded");
    assert_eq!(other.calls(), vec![print_call(WEB)]);
}

/// Catches a link that appeared since the last reload being ignored: a new link whose job is
/// loaded is booted out and bootstrapped again from its plist.
#[test]
fn a_new_link_whose_job_is_loaded_is_bootstrapped() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    let extra = "dev.hostkeeper.cache";
    std::fs::write(rig.v1.join(format!("{extra}.plist")), extra).expect("a plist");
    symlink(rig.v1.join(format!("{extra}.plist")), rig.link(extra)).expect("a link");
    loaded(&fake, extra);
    bootout_ok(&fake, extra);
    bootstrap_ok(&fake, &rig, extra);

    control.daemon_reload().expect("the reload");

    let mut expected = vec![print_call(extra)];
    expected.extend(bootout_calls(extra));
    expected.push(rig.bootstrap(extra));
    assert_eq!(fake.calls(), expected);
}

/// Catches a newly placed plist left unloaded, so that a hook's `launchctl kickstart` of its job
/// finds nothing (the first install of a package): a new link whose job is not loaded is
/// printed and bootstrapped from its link, and not booted out.
#[test]
fn a_new_link_whose_job_is_not_loaded_is_bootstrapped() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    let extra = "dev.hostkeeper.cache";
    std::fs::write(rig.v1.join(format!("{extra}.plist")), extra).expect("a plist");
    symlink(rig.v1.join(format!("{extra}.plist")), rig.link(extra)).expect("a link");
    not_loaded(&fake, extra);
    bootstrap_ok(&fake, &rig, extra);

    control.daemon_reload().expect("the reload");

    assert_eq!(fake.calls(), vec![print_call(extra), rig.bootstrap(extra)]);
}

/// Catches a re-pointed link whose job is not loaded being left so (the slot an update does not
/// serve from, unloaded by hand): it is bootstrapped from its link, and a failed bootstrap of it
/// is an error naming the command, after which the next reload tries it again.
#[test]
fn a_repointed_link_whose_job_is_not_loaded_is_bootstrapped() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    not_loaded(&fake, WEB);
    fake.then(
        &rig.bootstrap(WEB),
        Ok(LaunchctlOutput {
            code: Some(5),
            stdout: Vec::new(),
            stderr: b"Bootstrap failed: 5: Input/output error".to_vec(),
        }),
    );
    rig.repoint(WEB, &rig.v2);

    let failed = control.daemon_reload().expect_err("the bootstrap failed");
    assert_eq!(
        failed,
        error(
            &format!("launchctl bootstrap system {}", rig.link(WEB).display()),
            "exit 5: Bootstrap failed: 5: Input/output error"
        )
    );
    bootstrap_ok(&fake, &rig, WEB);
    control.daemon_reload().expect("the reload tried again");

    assert_eq!(
        fake.calls(),
        vec![
            print_call(WEB),
            rig.bootstrap(WEB),
            print_call(WEB),
            rig.bootstrap(WEB),
        ]
    );
    control.daemon_reload().expect("nothing changed since");
    assert_eq!(
        fake.calls().len(),
        4,
        "a reload with nothing changed issues nothing"
    );
}

/// Catches a removed link being bootstrapped again: the job is booted out and nothing is loaded.
#[test]
fn a_removed_link_boots_its_job_out_and_loads_nothing() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    loaded(&fake, DB);
    bootout_ok(&fake, DB);
    std::fs::remove_file(rig.link(DB)).expect("remove the link");

    control.daemon_reload().expect("the reload");

    let mut expected = vec![print_call(DB)];
    expected.extend(bootout_calls(DB));
    assert_eq!(fake.calls(), expected);
}

/// Catches other software's plists, files that are not plists, and links that lead nowhere being
/// acted on: none of them is read, so changing them issues no command; a dangling link counts as
/// absent until its target exists, and as removed when its target goes.
#[test]
fn regular_files_other_names_and_dangling_links_are_left_alone() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    std::fs::write(rig.link("other-software"), "other software").expect("a regular plist");
    std::fs::write(rig.units.join("bad label.plist"), "other software").expect("a regular plist");
    symlink(rig.v1.join("notes"), rig.units.join("notes.txt")).expect("a link");
    let odd = rig.units.join(OsStr::from_bytes(b"\xff.plist"));
    symlink(rig.v1.join("anything"), odd).expect("a link");
    let ghost = "dev.hostkeeper.ghost";
    symlink(rig.v1.join("ghost-target"), rig.link(ghost)).expect("a dangling link");
    let control = rig.control(&fake);

    std::fs::write(rig.link("other-software"), "changed").expect("rewrite");
    control.daemon_reload().expect("the reload");
    assert_eq!(fake.calls(), Vec::<Vec<String>>::new());

    std::fs::write(rig.v1.join("ghost-target"), "now there").expect("the target");
    loaded(&fake, ghost);
    bootout_ok(&fake, ghost);
    bootstrap_ok(&fake, &rig, ghost);
    control.daemon_reload().expect("the reload");
    let mut expected = vec![print_call(ghost)];
    expected.extend(bootout_calls(ghost));
    expected.push(rig.bootstrap(ghost));
    assert_eq!(fake.calls(), expected);

    std::fs::remove_file(rig.v1.join("ghost-target")).expect("remove the target");
    control.daemon_reload().expect("the reload");
    let mut again = expected.clone();
    again.push(print_call(ghost));
    again.extend(bootout_calls(ghost));
    assert_eq!(fake.calls(), again);
}

/// Catches a link with a name that is not a label being skipped quietly: it is an error naming
/// the path, from the constructor and from a reload alike.
#[test]
fn a_link_whose_name_is_not_a_label_is_an_error() {
    let rig = rig();
    let bad = rig.units.join("bad label.plist");
    symlink(rig.v1.join(format!("{WEB}.plist")), &bad).expect("a link");

    let made = LaunchdUnitControl::new(FakeLaunchctl::new(), &rig.units);
    let Err(e) = made else {
        panic!("a control was made over a bad link");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        e.to_string(),
        format!(
            "{}: launchd label \"bad label\" holds the byte 0x20, which a label may not",
            bad.display()
        )
    );

    std::fs::remove_file(&bad).expect("remove the link");
    let control = rig.control(&FakeLaunchctl::new());
    symlink(rig.v1.join(format!("{WEB}.plist")), &bad).expect("a link");
    assert_eq!(
        control.daemon_reload(),
        Err(error(
            &format!("read {}", rig.units.display()),
            &format!(
                "{}: launchd label \"bad label\" holds the byte 0x20, which a label may not",
                bad.display()
            )
        ))
    );
}

/// Catches a missing or unreadable unit directory being taken for an empty one: both the
/// constructor and a later reload say so.
#[test]
fn a_missing_unit_directory_is_an_error() {
    let rig = rig();
    let missing = rig.units.join("absent");
    let Err(e) = LaunchdUnitControl::new(FakeLaunchctl::new(), &missing) else {
        panic!("a control was made over a missing directory");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);

    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    for name in [WEB, DB] {
        std::fs::remove_file(rig.link(name)).expect("remove the link");
    }
    std::fs::remove_dir(&rig.units).expect("remove the directory");
    assert_eq!(
        control.daemon_reload(),
        Err(error(
            &format!("read {}", rig.units.display()),
            "No such file or directory (os error 2)"
        ))
    );
    assert!(fake.calls().is_empty());
}

/// Catches a job that is not loaded being kickstarted and a loaded one being bootstrapped:
/// not loaded is bootstrapped from its plist, running is left alone, loaded and not running is
/// kickstarted.
#[test]
fn start_unit_bootstraps_what_is_not_loaded_and_kickstarts_what_is_not_running() {
    let rig = rig();

    let fake = FakeLaunchctl::new();
    not_loaded(&fake, WEB);
    bootstrap_ok(&fake, &rig, WEB);
    rig.control(&fake).start_unit(WEB).expect("the start");
    assert_eq!(fake.calls(), vec![print_call(WEB), rig.bootstrap(WEB)]);

    let fake = FakeLaunchctl::new();
    loaded(&fake, WEB);
    rig.control(&fake).start_unit(WEB).expect("the start");
    assert_eq!(fake.calls(), vec![print_call(WEB)]);

    let fake = FakeLaunchctl::new();
    waiting(&fake, WEB);
    fake.always(
        &kickstart_args(&label(WEB)),
        Ok(LaunchctlOutput::success("")),
    );
    rig.control(&fake).start_unit(WEB).expect("the start");
    assert_eq!(
        fake.calls(),
        vec![print_call(WEB), kickstart_args(&label(WEB))]
    );
}

/// Catches a job read as active when it is not: only a loaded job that is running is active.
#[test]
fn is_active_is_true_only_for_a_running_job() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    loaded(&fake, WEB);
    waiting(&fake, DB);
    not_loaded(&fake, "dev.hostkeeper.cache");
    let control = rig.control(&fake);

    assert_eq!(control.is_active(WEB), Ok(true));
    assert_eq!(control.is_active(DB), Ok(false));
    assert_eq!(control.is_active("dev.hostkeeper.cache"), Ok(false));
}

/// Catches a launchctl failure being swallowed as success or as "not active": a command that
/// cannot be run and one that exits 1 are each an error naming the command, in every call.
#[test]
fn a_failing_launchctl_is_an_error_in_every_call() {
    let rig = rig();
    let print_cmd = format!("launchctl print system/{WEB}");
    let silent = rig.control(&FakeLaunchctl::new());
    let no_answer =
        "cannot run launchctl print system/dev.hostkeeper.web-main: the fake has no answer";

    rig.repoint(WEB, &rig.v2);
    assert_eq!(silent.daemon_reload(), Err(error(&print_cmd, no_answer)));
    assert_eq!(
        silent.stop_unit(WEB),
        Err(error(&format!("launchctl bootout system/{WEB}"), no_answer))
    );
    assert_eq!(silent.start_unit(WEB), Err(error(&print_cmd, no_answer)));
    assert_eq!(silent.is_active(WEB), Err(error(&print_cmd, no_answer)));

    let fake = FakeLaunchctl::new();
    fake.always(
        &print_call(WEB),
        Ok(LaunchctlOutput::exit(1, "launchd is busy")),
    );
    let control = rig.control(&fake);
    let busy = "launchctl print exited with Some(1): launchd is busy";
    rig.repoint(WEB, &rig.v1);
    assert_eq!(control.daemon_reload(), Err(error(&print_cmd, busy)));
    assert_eq!(
        control.stop_unit(WEB),
        Err(error(&format!("launchctl bootout system/{WEB}"), busy))
    );
    assert_eq!(control.start_unit(WEB), Err(error(&print_cmd, busy)));
    assert_eq!(control.is_active(WEB), Err(error(&print_cmd, busy)));
}

/// Catches a failed boot-out, bootstrap or kickstart being taken for success: each is an error
/// naming its own command and how it ended.
#[test]
fn a_failing_bootout_bootstrap_or_kickstart_is_an_error() {
    let rig = rig();
    let bootout_cmd = format!("launchctl bootout system/{WEB}");
    let bootstrap_cmd = format!("launchctl bootstrap system {}", rig.link(WEB).display());
    let kickstart_cmd = format!("launchctl kickstart system/{WEB}");

    let fake = FakeLaunchctl::new();
    loaded(&fake, WEB);
    fake.always(
        &bootout_args(&label(WEB)),
        Ok(LaunchctlOutput::exit(5, "denied")),
    );
    assert_eq!(
        rig.control(&fake).stop_unit(WEB),
        Err(error(
            &bootout_cmd,
            "launchctl bootout exited with Some(5): denied"
        ))
    );

    let fake = FakeLaunchctl::new();
    not_loaded(&fake, WEB);
    fake.always(
        &rig.bootstrap(WEB),
        Ok(LaunchctlOutput::exit(5, "no such plist")),
    );
    assert_eq!(
        rig.control(&fake).start_unit(WEB),
        Err(error(&bootstrap_cmd, "exit 5: no such plist"))
    );

    let fake = FakeLaunchctl::new();
    waiting(&fake, WEB);
    fake.always(
        &kickstart_args(&label(WEB)),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: b"killed".to_vec(),
        }),
    );
    assert_eq!(
        rig.control(&fake).start_unit(WEB),
        Err(error(&kickstart_cmd, "ended by a signal: killed"))
    );

    let fake = FakeLaunchctl::new();
    not_loaded(&fake, WEB);
    assert_eq!(
        rig.control(&fake).start_unit(WEB),
        Err(error(
            &bootstrap_cmd,
            &format!("cannot run {bootstrap_cmd}: the fake has no answer")
        ))
    );

    let fake = FakeLaunchctl::new();
    loaded(&fake, WEB);
    bootout_ok(&fake, WEB);
    let control = rig.control(&fake);
    rig.repoint(WEB, &rig.v2);
    assert_eq!(
        control.daemon_reload(),
        Err(error(
            &bootstrap_cmd,
            &format!("cannot run {bootstrap_cmd}: the fake has no answer")
        ))
    );
}

/// Catches a reload that failed being counted as done: the snapshot is kept, so the next call
/// issues the same commands again, and only a successful one settles it.
#[test]
fn a_failed_reload_is_retried_on_the_next_call() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    loaded(&fake, WEB);
    bootout_ok(&fake, WEB);
    fake.then(&rig.bootstrap(WEB), Ok(LaunchctlOutput::exit(1, "boom")));
    bootstrap_ok(&fake, &rig, WEB);
    rig.repoint(WEB, &rig.v2);

    let bootstrap_cmd = format!("launchctl bootstrap system {}", rig.link(WEB).display());
    assert_eq!(
        control.daemon_reload(),
        Err(error(&bootstrap_cmd, "exit 1: boom"))
    );
    let mut attempt = vec![print_call(WEB)];
    attempt.extend(bootout_calls(WEB));
    attempt.push(rig.bootstrap(WEB));
    assert_eq!(fake.calls(), attempt);

    control.daemon_reload().expect("the retry");
    let twice: Vec<_> = attempt.iter().chain(&attempt).cloned().collect();
    assert_eq!(fake.calls(), twice);

    control.daemon_reload().expect("the settled reload");
    assert_eq!(fake.calls(), twice, "the retry settled the snapshot");
}

/// Catches a bad label reaching launchctl: each call refuses it with the reason, and nothing is
/// run.
#[test]
fn a_bad_label_is_refused_before_launchctl_is_asked() {
    let rig = rig();
    let fake = FakeLaunchctl::new();
    let control = rig.control(&fake);
    let refused = error(
        "launchctl bootout system/bad label",
        "launchd label \"bad label\" holds the byte 0x20, which a label may not",
    );

    assert_eq!(control.stop_unit("bad label"), Err(refused));
    assert!(control.start_unit("bad label").is_err());
    assert!(control.is_active("bad label").is_err());
    assert!(fake.calls().is_empty());
}
