//! `cargo xtask farm-build`, end to end: the verb is run as a process with `--buck2` naming a copy
//! of the `buck2` fixture (tests/bin/buck2_fixture.rs) in a directory of its own, whose files say
//! what each buck2 call answers; the test reads the verb's exit status and words, and the calls
//! the fixture recorded.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use testdir::TestDir;

/// What buck2 prints for a key that is set.
const SET: &str = r#"{"buildrules_re.linux_x86_64_properties":"OSFamily=Linux,ISA=x86-64"}"#;
/// A farm build's summary line.
const FARM: &str = "Commands: 474 (cached: 0, remote: 474, local: 0)";
/// The calls of a run that got as far as the build.
const BUILT: [&str; 3] = [
    "audit config buildrules_re.linux_x86_64_properties --style json",
    "build //:ci",
    "kill",
];

/// A directory with a copy of the fixture as `buck2`, and `answers` (file name, text) beside it.
fn buck2(name: &str, answers: &[(&str, &str)]) -> (TestDir, PathBuf) {
    let dir = TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("farm-build-{name}"),
    );
    let program = dir.join("buck2");
    std::fs::copy(env!("CARGO_BIN_EXE_xtask-buck2-fixture"), &program).expect("the fixture");
    for (file, text) in answers {
        std::fs::write(dir.join(file), text).expect("an answer");
    }
    (dir, program)
}

fn farm_build(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("farm-build")
        .args(args)
        .output()
        .expect("run xtask")
}

fn run(program: &Path) -> Output {
    farm_build(&[
        "--buck2",
        program.to_str().expect("a UTF-8 path"),
        "--target",
        "//:ci",
    ])
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_farm_build_prints_its_summary_line_and_stops_buck2_with_status_0() {
    // Catches: the build not run, run before the worker check, its output not passed on, its
    // summary not printed, or buck2's daemon left running.
    let log = format!("[t] Waiting on 3 actions\n[t] {FARM}\nBUILD SUCCEEDED\n");
    let (dir, program) = buck2(
        "ok",
        &[
            ("audit.stdout", SET),
            ("build.stderr", &log),
            ("build.stdout", "out\n"),
        ],
    );
    let o = run(&program);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), format!("{FARM}\n"));
    assert_eq!(stderr(&o), format!("out\n{log}"));
    assert_eq!(calls(&dir), BUILT);
}

#[test]
fn a_build_with_actions_on_this_machine_fails_with_status_1() {
    // Catches: `local: N` with N above 0 (no farm, or a fallback to the runner) passed as a farm
    // build because the build itself succeeded.
    let line = "Commands: 3 (cached: 0, remote: 1, local: 2)";
    let (dir, program) = buck2(
        "local",
        &[
            ("audit.stdout", SET),
            ("build.stderr", &format!("{line}\n")),
        ],
    );
    let o = run(&program);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).ends_with(&format!(
            "xtask: `{line}`: actions ran on this machine, not on the farm (local must be 0)\n"
        )),
        "{}",
        stderr(&o)
    );
    assert_eq!(calls(&dir), BUILT);
}

#[test]
fn a_build_with_no_summary_line_fails_with_status_1() {
    // Catches: a build whose summary is missing (a buck2 that printed nothing) read as local: 0.
    let (_dir, program) = buck2(
        "no-summary",
        &[("audit.stdout", SET), ("build.stderr", "BUILD SUCCEEDED\n")],
    );
    let o = run(&program);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).ends_with(
            "xtask: the build printed no summary line (`Commands: N (cached: N, remote: N, \
             local: N)`)\n"
        ),
        "{}",
        stderr(&o)
    );
}

#[test]
fn a_failed_build_fails_with_status_1_and_still_stops_buck2() {
    // Catches: a failed build passed because its summary says local: 0, or the daemon left
    // running after a failure.
    let (dir, program) = buck2(
        "failed",
        &[
            ("audit.stdout", SET),
            ("build.stderr", &format!("{FARM}\nBUILD FAILED\n")),
            ("build.status", "3"),
        ],
    );
    let o = run(&program);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).ends_with("xtask: `buck2 build //:ci` failed (exit status: 3)\n"),
        "{}",
        stderr(&o)
    );
    assert_eq!(calls(&dir), BUILT);
}

#[test]
fn no_worker_property_set_fails_before_the_build_with_status_1() {
    // Catches: a build started with no farm configured (buck2 would have no execution platform,
    // or run on the runner), for the key unset (`{}`) and for it set to nothing.
    for (name, json) in [
        ("unset", "{}"),
        ("empty", r#"{"buildrules_re.linux_x86_64_properties":""}"#),
    ] {
        let (dir, program) = buck2(name, &[("audit.stdout", json)]);
        let o = run(&program);
        assert_eq!(o.status.code(), Some(1), "{name}");
        assert_eq!(
            stderr(&o),
            "xtask: buildrules_re.linux_x86_64_properties is empty: farm-connect wrote no \
             [komira_re] linux_x86_64_properties, so buck2 has no farm to build on\n",
            "{name}"
        );
        assert_eq!(calls(&dir), [BUILT[0], BUILT[2]], "{name}");
    }
}

#[test]
fn a_failed_audit_fails_before_the_build_with_status_1() {
    // Catches: buck2 that cannot read its configuration (no .buckconfig) taken for one with no
    // workers, or ignored.
    let (dir, program) = buck2(
        "audit",
        &[
            ("audit.stderr", "no .buckconfig here\n"),
            ("audit.status", "3"),
        ],
    );
    let o = run(&program);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        "xtask: `buck2 audit config` failed (exit status: 3): no .buckconfig here\n"
    );
    assert_eq!(calls(&dir), [BUILT[0], BUILT[2]]);
}

#[test]
fn a_buck2_that_cannot_start_fails_with_status_1_and_target_is_required() {
    // Catches: a missing buck2 reported as something else, and a build of nothing when no
    // target is given.
    let dir = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "farm-build-none");
    let missing = dir.join("buck2");
    let o = run(&missing);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!("xtask: cannot run {missing:?}: No such file or directory (os error 2)\n")
    );
    let o = farm_build(&["--buck2", "buck2"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("--target <TARGET>"), "{}", stderr(&o));
}
