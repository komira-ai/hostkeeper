//! Check 9 of the conformance harness, `drain` and `resume`: proven against the example hook,
//! which drains and resumes as the contract says, and against copies of it with one flaw each,
//! which the harness must refuse by name. The deadline is a time already passed that the harness
//! writes into the params file; nothing here waits for a time.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};

use hostkeeper_hook::conformance::{
    Conformance, FakeService, Fault, HookCommand, Sandbox, Violation, check,
};
use hostkeeper_hook::{Outcome, Params, RUN_ID_KEY, Report, SERVICE_KEY, Scalar, Step};

mod support;
use support::{ExampleFake, LEASE, Scratch, example, example_with};

const CONVENTION: &[&str] = &["update-hook"];

/// Check 9 alone on `hook`, against the example fake.
fn drain_check(name: &str, hook: &HookCommand) -> Result<(Vec<String>, Vec<String>), Violation> {
    let scratch = Scratch::new(name);
    let mut fake = ExampleFake::default();
    Conformance::new(hook, &mut fake, scratch.path()).drain_resume()
}

/// Check 9 on the example hook with one defect, which the harness must refuse.
fn refused(name: &str, defect: &str) -> Violation {
    match drain_check(name, &example_with(defect)) {
        Ok(ran) => panic!("{defect} passed: {ran:?}"),
        Err(violation) => violation,
    }
}

/// The example fake, but with no work to put in flight: it keeps the default `start_work`.
struct Idle(ExampleFake);

impl FakeService for Idle {
    fn init(&mut self, sandbox: &Sandbox) -> io::Result<()> {
        self.0.init(sandbox)
    }

    fn env(&self, sandbox: &Sandbox) -> Vec<(OsString, OsString)> {
        self.0.env(sandbox)
    }

    fn params(&self) -> Params {
        self.0.params()
    }

    fn inject(&mut self, sandbox: &Sandbox, step: Step, fault: Fault) -> io::Result<bool> {
        self.0.inject(sandbox, step, fault)
    }

    fn residue(&self, sandbox: &Sandbox) -> io::Result<Vec<String>> {
        self.0.residue(sandbox)
    }
}

// --- a hook that drains and resumes passes ---

/// Catches a harness that refuses a hook that keeps the drain contract, or that leaves check 9
/// out of `check_all` once it is asked for: every check passes, and the summary lists the
/// update's 16 scenarios and the two of check 9, with nothing skipped.
#[test]
fn a_hook_that_drains_and_resumes_passes_every_check() {
    for (name, hook) in [
        ("drain-pass-convention", example(CONVENTION)),
        ("drain-pass-no-hook-args", example(&[])),
    ] {
        let scratch = Scratch::new(name);
        let mut fake = ExampleFake::default();
        let summary = Conformance::new(&hook, &mut fake, scratch.path())
            .with_drain()
            .check_all()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(summary.scenarios.len(), 18, "{name}");
        assert_eq!(
            summary.scenarios[16..],
            ["drain,resume", "drain=deadline"],
            "{name}"
        );
        assert!(summary.skipped.is_empty(), "{name}: {:?}", summary.skipped);
    }
}

/// Catches check 9 running for a hook that did not ask for it: without `with_drain` the harness
/// runs neither step, so a hook whose `resume` would be refused passes the other checks.
#[test]
fn without_with_drain_the_harness_runs_neither_step() {
    let scratch = Scratch::new("drain-not-asked");
    let mut fake = ExampleFake::default();
    let summary = check(
        &example_with("resume-without-drain-writes"),
        &mut fake,
        scratch.path(),
    )
    .expect("the update's checks pass");
    assert_eq!(summary.scenarios.len(), 16);
    assert!(!summary.scenarios.iter().any(|s| s.starts_with("drain")));
    assert!(!scratch.path().join("run-1/control/drain.toml").exists());
}

/// Catches a fake that cannot put work in flight being judged on the deadline anyway, or the
/// scenario being dropped without a word: it is skipped and said to be, and the rest of check
/// 9 still runs.
#[test]
fn a_fake_with_no_work_skips_the_deadline_and_says_so() {
    let scratch = Scratch::new("drain-idle-fake");
    let hook = example(CONVENTION);
    let mut fake = Idle(ExampleFake::default());
    let (ran, skipped) = Conformance::new(&hook, &mut fake, scratch.path())
        .drain_resume()
        .expect("passes");
    assert_eq!(ran, ["drain,resume"]);
    assert_eq!(skipped, ["drain=deadline"]);
}

/// Catches `drain` and `resume` run with the update's params file, or without the service, the
/// run or the deadline: they get a file of their own with the fake's table, the service and the
/// run, and the deadline (one second after the epoch, long passed) only where it is tested.
#[test]
fn drain_and_resume_get_the_service_the_run_and_a_passed_deadline() {
    let scratch = Scratch::new("drain-params");
    let mut fake = ExampleFake::default();
    let hook = example(CONVENTION);
    Conformance::new(&hook, &mut fake, scratch.path())
        .drain_resume()
        .expect("passes");
    // Three sandboxes: resume alone, drain then resume, the deadline.
    for (run, deadline) in [(1, None), (2, None), (3, Some(1))] {
        let file = scratch.path().join(format!("run-{run}/control/drain.toml"));
        let params = Params::read(&file).expect("the drain params");
        assert_eq!(params.service(), Some("conformance"), "run-{run}");
        assert_eq!(params.run_id(), Some("conformance-run"), "run-{run}");
        assert_eq!(params.drain_deadline_unix(), deadline, "run-{run}");
        assert_eq!(params.get_u64("promotion_bound_secs"), Some(900));
        assert_eq!(params.staged_release(), None, "a drain stages nothing");
    }
    // The deadline run kept its work, and the service is back in service.
    let world = scratch.path().join("run-3/world");
    assert_eq!(fs::read(world.join("work").join(LEASE)).unwrap(), b"job 1");
    assert!(!world.join("drained").exists());
}

// --- each flaw is refused by name ---

/// Catches a `drain` that writes again when run a second time.
#[test]
fn a_drain_that_is_not_idempotent_is_refused() {
    match refused("drain-bad-idempotent", "drain-not-idempotent") {
        Violation::NotIdempotent { step, changes } => {
            assert_eq!(step, Step::Drain);
            assert_eq!(changes, ["changed drain-log"]);
        }
        other => panic!("{other}"),
    }
}

/// Catches a `resume` that writes again when run a second time.
#[test]
fn a_resume_that_is_not_idempotent_is_refused() {
    match refused("resume-bad-idempotent", "resume-not-idempotent") {
        Violation::NotIdempotent { step, changes } => {
            assert_eq!(step, Step::Resume);
            assert_eq!(changes, ["changed resume-log"]);
        }
        other => panic!("{other}"),
    }
}

/// Catches a `drain` that exits 0 with work in flight after its deadline: the updater would
/// update a host still doing work.
#[test]
fn a_drain_that_exits_0_past_its_deadline_is_refused() {
    let violation = refused("drain-bad-deadline", "drain-ignores-deadline");
    assert!(
        matches!(violation, Violation::DrainMissedDeadline),
        "{violation}"
    );
    assert_eq!(
        violation.to_string(),
        "drain exited 0 with work in flight after its deadline had passed"
    );
}

/// Catches a `drain` that cuts work in flight to meet its deadline, even one that then resumes
/// and exits 75 as it should.
#[test]
fn a_drain_that_cuts_work_is_refused() {
    match refused("drain-bad-cuts", "drain-cuts-work") {
        Violation::DrainCutWork { changes } => {
            assert_eq!(changes, [format!("work/{LEASE}")]);
        }
        other => panic!("{other}"),
    }
}

/// Catches a `drain` that defers at its deadline but leaves the service out of service: the
/// host would stay cordoned with nothing changed.
#[test]
fn a_drain_deferred_by_its_deadline_must_resume() {
    match refused("drain-bad-stays", "deadline-stays-drained") {
        Violation::DeferredDrainNotResumed { changes } => {
            assert_eq!(changes, ["added drained"]);
        }
        other => panic!("{other}"),
    }
}

/// Catches a `resume` that changes a service it never drained: the updater runs `resume` for
/// every service it may have drained.
#[test]
fn a_resume_without_a_drain_must_change_nothing() {
    match refused("resume-bad-alone", "resume-without-drain-writes") {
        Violation::ResumeWithoutDrainChanged { changes } => {
            assert_eq!(changes, ["added resumed"]);
        }
        other => panic!("{other}"),
    }
}

/// Catches a `resume` that succeeds but leaves the service drained.
#[test]
fn a_resume_that_does_not_put_the_service_back_is_refused() {
    match refused("resume-bad-leaves", "resume-leaves-drained") {
        Violation::ResumeLeftDrained { changes } => {
            assert_eq!(changes, ["added drained"]);
        }
        other => panic!("{other}"),
    }
}

// --- the example's drain waits for its work ---

/// Catches an example `drain` that returns while work is in flight and no deadline is set: it
/// says it is waiting (on standard error), and exits 0, drained, only after the test ends the
/// work. Nothing is timed: the test reads the hook's line and then waits for it to exit.
#[test]
fn a_drain_with_no_deadline_returns_once_its_work_has_ended() {
    let scratch = Scratch::new("drain-waits");
    let sandbox = Sandbox::create(scratch.path().join("run")).expect("a sandbox");
    let mut fake = ExampleFake::default();
    fake.init(&sandbox).expect("init");
    assert!(fake.start_work(&sandbox).expect("work"));
    let mut params = fake.params();
    params.insert(SERVICE_KEY, Scalar::Text("example".to_owned()));
    params.insert(RUN_ID_KEY, Scalar::Text("run-1".to_owned()));
    let file = sandbox.control().join("drain.toml");
    fs::write(&file, params.to_toml()).expect("the params");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hostkeeper-hook-example"))
        .args(["update-hook", "drain", "--params"])
        .arg(&file)
        .envs(fake.env(&sandbox))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the example hook");
    let mut stderr = BufReader::new(child.stderr.take().expect("a pipe"));
    let mut line = String::new();
    stderr.read_line(&mut line).expect("a line");
    assert_eq!(
        line,
        "hostkeeper-hook-example: drain: waiting for 1 piece of work\n"
    );
    assert!(sandbox.world().join("drained").exists(), "drained first");
    fs::remove_file(sandbox.world().join("work").join(LEASE)).expect("end the work");
    let out = child.wait_with_output().expect("the hook exits");
    assert_eq!(out.status.code(), Some(0));
    let report = Report::from_stdout(&String::from_utf8_lossy(&out.stdout)).expect("a last line");
    assert_eq!(report.retry_after_secs, None);
    assert!(sandbox.world().join("drained").exists());
    assert_eq!(
        fs::read_to_string(sandbox.record().join("drained")).expect("the record"),
        "run-1"
    );
    // The outcome the updater reads.
    assert_eq!(Outcome::from_exit(out.status.code()), Outcome::Success);
}
