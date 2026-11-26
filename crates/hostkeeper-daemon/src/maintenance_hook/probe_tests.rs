//! The tests of the `probe` kind, over a fake Mac: each program's answer scripted by its path and
//! exact arguments, each run recorded with its path, deadline and arguments, and a fixed clock.
//! What ran is asserted by equality of the whole list of runs, the probe by equality of the whole
//! file it wrote, and each failure by equality of its reason and whole detail.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use hostkeeper_launchd::{Launchctl, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintenanceKind as WireKind};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_hook::params::Kind;

/// The clock's second.
const NOW: u64 = 1_800_000_000;

type Answer = Result<LaunchctlOutput, LaunchctlError>;

/// One run of a program: its path, its deadline and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ran {
    program: PathBuf,
    deadline: Duration,
    args: Vec<String>,
}

/// What the fake Mac answers, and what was asked of it.
#[derive(Default)]
struct Script {
    answers: BTreeMap<(PathBuf, Vec<String>), Answer>,
    runs: Vec<Ran>,
}

/// A fake Mac. Clones share one script.
#[derive(Clone, Default)]
struct Mac(Arc<Mutex<Script>>);

impl Mac {
    /// A Mac as the farm's were read with nobody logged in: root at the console, nobody in
    /// `who`, the hook as root in the System domain with an empty environment, macOS 27.2.1 and
    /// FileVault off.
    fn nobody_logged_in() -> Mac {
        let mac = Mac::default();
        mac.answer(
            "/usr/bin/stat",
            &["-f", "%Su", "/dev/console"],
            out(0, "root\n", ""),
        );
        mac.answer("/usr/bin/who", &[], out(0, "", ""));
        mac.answer(
            "/usr/bin/id",
            &[],
            out(
                0,
                "uid=0(root) gid=0(wheel) groups=0(wheel),1(daemon)\n",
                "",
            ),
        );
        mac.answer("/bin/launchctl", &["managername"], out(0, "System\n", ""));
        mac.answer("/usr/bin/env", &[], out(0, "", ""));
        mac.answer(
            "/usr/bin/sw_vers",
            &[],
            out(
                0,
                "ProductName:\t\tmacOS\nProductVersion:\t\t27.2.1\nBuildVersion:\t\t26B131\n",
                "",
            ),
        );
        mac.answer(
            "/usr/bin/fdesetup",
            &["status"],
            out(0, "FileVault is Off.\n", ""),
        );
        mac.answer(
            "/usr/bin/fdesetup",
            &["supportsauthrestart"],
            out(0, "true\n", ""),
        );
        mac
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        self.0.lock().expect("the script")
    }

    /// Answers `program` run with exactly `args` with `answer`, in place of any answer before.
    fn answer(&self, program: &str, args: &[&str], answer: Answer) {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect();
        self.script()
            .answers
            .insert((PathBuf::from(program), args), answer);
    }

    fn runs(&self) -> Vec<Ran> {
        self.script().runs.clone()
    }

    /// The probe of this Mac, at [`NOW`], keeping its file in `dir`.
    fn probe(&self, dir: Option<&Path>) -> ProbeKind {
        ProbeKind::new(Box::new(self.clone()), Box::new(Fixed), dir)
    }
}

impl Programs for Mac {
    fn tool(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(Tool {
            mac: self.clone(),
            program: program.to_owned(),
            deadline,
        })
    }
}

/// One program of the fake Mac: a run is recorded, and answered as scripted; a run with no
/// answer could not be started.
struct Tool {
    mac: Mac,
    program: PathBuf,
    deadline: Duration,
}

impl Launchctl for Tool {
    fn run(&self, args: &[String]) -> Answer {
        let mut script = self.mac.script();
        script.runs.push(Ran {
            program: self.program.clone(),
            deadline: self.deadline,
            args: args.to_vec(),
        });
        script
            .answers
            .get(&(self.program.clone(), args.to_vec()))
            .cloned()
            .unwrap_or_else(|| {
                Err(LaunchctlError::Spawn {
                    args: args.join(" "),
                    why: "No such file or directory (os error 2)".to_owned(),
                })
            })
    }
}

/// A clock at [`NOW`].
struct Fixed;

impl UnixClock for Fixed {
    fn unix_secs(&self) -> u64 {
        NOW
    }
}

/// How a program ended: `code`, with `stdout` and `stderr`.
fn out(code: i32, stdout: &str, stderr: &str) -> Answer {
    Ok(LaunchctlOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// The parameters of a probe run `run_id`.
fn params(run_id: &str) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: run_id.to_owned(),
        kinds: vec![WireKind::Probe.into()],
        ..Maintain::default()
    })
    .expect("valid")
}

/// A step of the probe as it is kept.
fn kept(name: &str, exit_code: Option<i32>, output: &str, cut: bool) -> MaintenanceProbeStep {
    MaintenanceProbeStep {
        name: name.to_owned(),
        exit_code,
        output: output.to_owned(),
        cut,
    }
}

/// Every program of [`STEPS`], each at the step deadline, in order.
fn every_step_ran() -> Vec<Ran> {
    STEPS
        .iter()
        .map(|step| Ran {
            program: PathBuf::from(step.program),
            deadline: STEP_DEADLINE,
            args: step.args.iter().map(|arg| (*arg).to_owned()).collect(),
        })
        .collect()
}

/// Proves: a probe runs each program of its list once, in order, by its absolute path with its
/// deadline, and keeps in the probe file the run's id, the clock's second, and for each step its
/// name, its exit code and its output, the lines of a multi-line output trimmed and joined by
/// `; `, an empty output kept empty; the `cutover` answers applied.
///
/// Catches: a step left out, run twice or out of order, a program run by another path or with
/// other arguments, an output kept with its tabs and newlines, an exit code lost, or a probe file
/// that does not say which run wrote it.
#[test]
fn a_probe_runs_each_step_in_order_and_keeps_what_each_gave() {
    let dir = scratch("probe-all");
    let mac = Mac::nobody_logged_in();
    let probe = mac.probe(Some(dir.path()));
    assert_eq!(probe.prepare(&params("run-1")), Ok(()));
    assert!(mac.runs().is_empty(), "the stage runs nothing");
    assert_eq!(
        probe.cutover(&params("run-1")),
        Ok(Cutover::Applied {
            version: "8 steps".to_owned()
        })
    );
    assert_eq!(mac.runs(), every_step_ran());
    let written = read_probe(&dir.path().join(PROBE_FILE)).expect("the probe file");
    assert_eq!(
        written,
        MaintenanceProbe {
            run_id: "run-1".to_owned(),
            probed_unix: NOW,
            steps: vec![
                kept("console", Some(0), "root", false),
                kept("who", Some(0), "", false),
                kept(
                    "id",
                    Some(0),
                    "uid=0(root) gid=0(wheel) groups=0(wheel),1(daemon)",
                    false
                ),
                kept("managername", Some(0), "System", false),
                kept("env", Some(0), "", false),
                kept(
                    "sw_vers",
                    Some(0),
                    "ProductName:\t\tmacOS; ProductVersion:\t\t27.2.1; BuildVersion:\t\t26B131",
                    false
                ),
                kept("fdesetup_status", Some(0), "FileVault is Off.", false),
                kept("fdesetup_supportsauthrestart", Some(0), "true", false),
            ],
        }
    );
    assert_eq!(probe.verify(&params("run-1")), Ok(()));
}

/// Proves: a program that exits non-zero, writes on standard error, cannot be started, is killed
/// at its deadline or ends by a signal is kept as a result, with its exit code (none when it did
/// not exit by itself) and what it wrote or why it did not run, and the probe goes on to the next
/// program and still answers applied.
///
/// Catches: a probe that stops at the first program that fails (and so leaves the later steps,
/// the ones the proof needs, unread), a failed program that halts the host, or a non-zero exit
/// kept as 0.
#[test]
fn a_program_that_fails_is_a_result_and_the_probe_goes_on() {
    let dir = scratch("probe-fail");
    let mac = Mac::nobody_logged_in();
    mac.answer(
        "/usr/bin/fdesetup",
        &["status"],
        out(1, "", "Error: unable to read FileVault status.\n"),
    );
    mac.answer(
        "/usr/bin/who",
        &[],
        Err(LaunchctlError::TimedOut {
            args: String::new(),
        }),
    );
    mac.answer(
        "/usr/bin/env",
        &[],
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    mac.script().answers.remove(&(
        PathBuf::from("/bin/launchctl"),
        vec!["managername".to_owned()],
    ));
    let probe = mac.probe(Some(dir.path()));
    assert_eq!(
        probe.cutover(&params("run-2")),
        Ok(Cutover::Applied {
            version: "8 steps".to_owned()
        })
    );
    assert_eq!(mac.runs(), every_step_ran());
    let steps = read_probe(&dir.path().join(PROBE_FILE))
        .expect("the probe file")
        .steps;
    assert_eq!(
        steps[1],
        kept(
            "who",
            None,
            "/usr/bin/who was killed at its deadline of 60s",
            false
        )
    );
    assert_eq!(
        steps[3],
        kept(
            "managername",
            None,
            "/bin/launchctl did not start: No such file or directory (os error 2)",
            false
        )
    );
    assert_eq!(steps[4], kept("env", None, "", false));
    assert_eq!(
        steps[6],
        kept(
            "fdesetup_status",
            Some(1),
            "Error: unable to read FileVault status.",
            false
        )
    );
    assert_eq!(
        steps[7],
        kept("fdesetup_supportsauthrestart", Some(0), "true", false)
    );
}

/// Proves: an output is the lines of standard output, then of standard error, and one longer
/// than its bound is cut to exactly 128 bytes at a character's boundary, marked cut; one of
/// exactly 128 bytes is kept whole and not marked.
///
/// Catches: standard error dropped or put first, an output over the report's bound (which the
/// coordinator refuses whole, losing the probe), a cut inside a character, or a cut that is not
/// said.
#[test]
fn an_output_keeps_both_streams_and_is_cut_to_its_bound_at_a_character() {
    assert_eq!(
        joined(&["a\n\n  b  \n", "warning: c\n"]),
        "a; b; warning: c"
    );
    assert_eq!(joined(&["", "\n \n"]), "");
    let exact = "x".repeat(MAX_PROBE_OUTPUT);
    assert_eq!(bounded(exact.clone()), (exact, false));
    let wide = format!("{}é", "x".repeat(MAX_PROBE_OUTPUT - 1));
    assert_eq!(bounded(wide), ("x".repeat(MAX_PROBE_OUTPUT - 1), true));

    let dir = scratch("probe-cut");
    let mac = Mac::nobody_logged_in();
    let long = format!("{}\n", "e=".repeat(100));
    mac.answer("/usr/bin/env", &[], out(0, &long, "env: note\n"));
    mac.probe(Some(dir.path()))
        .cutover(&params("run-3"))
        .expect("applied");
    let env = read_probe(&dir.path().join(PROBE_FILE))
        .expect("the probe file")
        .steps
        .swap_remove(4);
    assert_eq!(env, kept("env", Some(0), &"e=".repeat(64), true));
}

/// Proves: a probe whose file cannot be written fails `probe_unwritten`, naming why: a hook with
/// no run directory (its parameters on standard input), which runs no program, and a run
/// directory that is not there.
///
/// Catches: a probe that answers applied with nothing kept, so that the run ends done and the
/// operator reads an older probe as this run's.
#[test]
fn a_probe_whose_file_cannot_be_written_fails() {
    let mac = Mac::nobody_logged_in();
    assert_eq!(
        mac.probe(None).cutover(&params("run-4")),
        Err(KindError::Failed {
            reason: REASON_PROBE_UNWRITTEN,
            detail: "the probe file could not be written: the hook keeps no run directory"
                .to_owned(),
        })
    );
    assert!(mac.runs().is_empty(), "no program runs");
    let dir = scratch("probe-gone");
    let gone = dir.path().join("gone");
    assert_eq!(
        mac.probe(Some(&gone)).cutover(&params("run-4")),
        Err(KindError::Failed {
            reason: REASON_PROBE_UNWRITTEN,
            detail: "the probe file could not be written: No such file or directory (os error 2)"
                .to_owned(),
        })
    );
}

/// Proves: `verify` passes only when the probe file holds this run's probe, and fails
/// `probe_missing` naming why for no file, another run's probe, a file over its bound and one
/// that is not a probe; `after_reboot`, which a probe never asks for, answers
/// `unsupported_kind`.
///
/// Catches: a run that ends done while the report shows an earlier run's probe, or a file that
/// is read whatever its size.
#[test]
fn verify_finds_this_runs_probe_or_fails() {
    let dir = scratch("probe-verify");
    let mac = Mac::nobody_logged_in();
    let probe = mac.probe(Some(dir.path()));
    let path = dir.path().join(PROBE_FILE);
    let missing = |detail: &str| {
        Err(KindError::Failed {
            reason: REASON_PROBE_MISSING,
            detail: detail.to_owned(),
        })
    };
    assert_eq!(
        probe.verify(&params("run-5")),
        missing("the probe file could not be read: No such file or directory (os error 2)")
    );
    probe.cutover(&params("run-4")).expect("applied");
    assert_eq!(
        probe.verify(&params("run-5")),
        missing("the probe file holds the probe of run run-4")
    );
    fs::write(&path, vec![0x0a; MAX_PROBE_FILE_BYTES as usize + 1]).expect("write");
    assert_eq!(
        probe.verify(&params("run-5")),
        missing("the probe file could not be read: 8193 bytes, more than 8192")
    );
    fs::write(&path, [0xff]).expect("write");
    assert!(matches!(
        probe.verify(&params("run-5")),
        Err(KindError::Failed {
            reason: REASON_PROBE_MISSING,
            ..
        })
    ));
    assert_eq!(
        probe.after_reboot(&params("run-5")),
        Err(KindError::unsupported(Kind::Probe))
    );
}

/// Proves: a second probe replaces the first whole, and leaves no staged file beside it.
///
/// Catches: a probe file appended to, or a staged file left in the run directory.
#[test]
fn a_new_probe_replaces_the_last_whole() {
    let dir = scratch("probe-again");
    let mac = Mac::nobody_logged_in();
    let probe = mac.probe(Some(dir.path()));
    probe.cutover(&params("run-6")).expect("applied");
    probe.cutover(&params("run-7")).expect("applied");
    let written = read_probe(&dir.path().join(PROBE_FILE)).expect("the probe file");
    assert_eq!((written.run_id.as_str(), written.steps.len()), ("run-7", 8));
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, [PROBE_FILE]);
}
