//! The library without root: the fixture's command line, modes, output and pid file, the host
//! check, and the names, files and `systemctl` lines of the rig. Files go to a scratch directory
//! beside the test binary, inside the target directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hostkeeper_hook::{Invocation, Outcome, Params, Report, Step};
use hostkeeper_testkit::Scratch;

use crate::PRIVILEGED;
use crate::fixture::{
    self, Ending, FLOOD_LINE, FixtureError, MODE_KEY, Me, Mode, PID_FILE_KEY, PidFile, Run,
    StepRun, failed_report, ok_report,
};
use crate::host::{HostError, check_host};
use crate::rig::{self, Rig, RigError};

/// A scratch directory beside the test binary.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe.parent().expect("its directory").join("privileged-lib");
    Scratch::under(&base, name).expect("create the scratch directory")
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// Writes a params file holding `text` and gives its path.
fn params_file(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("params");
    std::fs::write(&path, text).expect("write the params file");
    path
}

fn parse_step(args: &[&str]) -> Result<Run, FixtureError> {
    fixture::parse_args(os(args))
}

fn step_run(step: Step, mode: Mode, pid_file: Option<PathBuf>) -> StepRun {
    StepRun {
        invocation: Invocation {
            step,
            artifact: None,
            params: Some(PathBuf::from("/params")),
        },
        mode,
        pid_file,
    }
}

/// Runs `run` as process 42 with the executable `exe`: how it ended, its stdout and stderr.
fn run(run: &StepRun, exe: &Path) -> (Result<Ending, FixtureError>, String, String) {
    let me = Me {
        pid: 42,
        exe: exe.to_path_buf(),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ending = fixture::run_step(run, &me, &mut out, &mut err);
    (
        ending,
        String::from_utf8(out).expect("stdout is text"),
        String::from_utf8(err).expect("stderr is text"),
    )
}

/// Catches a mode a test relies on parsed as another, printed in a form it does not
/// read back, or an unknown or malformed mode taken as one: every mode round-trips through its
/// text, `fail-at` takes every step, and anything else is refused with the list of modes.
#[test]
fn the_fixture_hook_parses_every_mode_and_refuses_an_unknown_one() {
    let mut modes = vec![Mode::Ok, Mode::Sleep, Mode::PrintPid, Mode::Flood(0)];
    modes.push(Mode::Flood(3 * 1024 * 1024));
    modes.extend(Step::ALL.map(Mode::FailAt));
    modes.push(Mode::FailAt(Step::Uninstall));
    for mode in modes {
        assert_eq!(Mode::parse(&mode.to_string()).expect("parses"), mode);
    }
    assert_eq!(
        Mode::parse("fail-at=verify").expect("parses"),
        Mode::FailAt(Step::Verify)
    );
    assert_eq!(Mode::parse("flood=100").expect("parses"), Mode::Flood(100));
    assert_eq!(Mode::Sleep.to_string(), "sleep");
    for bad in [
        "",
        "OK",
        "sleepy",
        "fail-at=",
        "fail-at=deploy",
        "flood=",
        "flood=-1",
        "flood=x",
        "nap=1",
        "print-pid=1",
    ] {
        let err = Mode::parse(bad).expect_err(bad);
        assert_eq!(
            err.to_string(),
            format!(
                "unknown fixture mode {bad:?} (ok, sleep, fail-at=<step>, print-pid, flood=<bytes>)"
            )
        );
    }
}

/// Catches `serve` read as a step or given an argument it would ignore, and a step whose params
/// file is missing, unreadable, without a mode, or with a key of the wrong type or a relative pid
/// file run anyway.
#[test]
fn the_fixture_hook_reads_its_command_line_and_refuses_a_bad_one() {
    assert_eq!(parse_step(&["serve"]).expect("serve"), Run::Serve);
    let extra = parse_step(&["serve", "now"]).expect_err("an argument after serve");
    assert_eq!(extra.to_string(), "serve takes no argument, not \"now\"");
    let empty = fixture::parse_args(Vec::<OsString>::new()).expect_err("no step");
    assert_eq!(empty.to_string(), "no step given");
    let no_params = parse_step(&["cutover"]).expect_err("no params");
    assert_eq!(no_params.to_string(), "the step was given no --params file");

    let dir = scratch("args");
    let missing = dir.path().join("missing");
    let unread = parse_step(&["cutover", "--params", missing.to_str().expect("utf-8")]);
    assert!(matches!(unread, Err(FixtureError::Params(_))), "{unread:?}");
    let cases: [(&str, &str); 4] = [
        ("\"other\" = \"x\"\n", "the params file has no fixture.mode"),
        ("\"fixture.mode\" = 1\n", "fixture.mode must be text"),
        (
            "\"fixture.mode\" = \"sleep\"\n\"fixture.pid_file\" = true\n",
            "fixture.pid_file must be text",
        ),
        (
            "\"fixture.mode\" = \"sleep\"\n\"fixture.pid_file\" = \"pids\"\n",
            "fixture.pid_file must be an absolute path, not \"pids\"",
        ),
    ];
    for (text, want) in cases {
        let file = params_file(dir.path(), text);
        let err =
            parse_step(&["verify", "--params", file.to_str().expect("utf-8")]).expect_err(text);
        assert_eq!(err.to_string(), want, "{text}");
    }

    let pids = dir.path().join("pids");
    let text = format!(
        "\"fixture.mode\" = \"fail-at=cleanup\"\n\"fixture.pid_file\" = {:?}\n",
        pids.to_str().expect("utf-8")
    );
    let file = params_file(dir.path(), &text);
    let file_arg = file.to_str().expect("utf-8");
    let found = parse_step(&["prepare", "/releases/a", "--params", file_arg]).expect("parses");
    assert_eq!(
        found,
        Run::Step(StepRun {
            invocation: Invocation {
                step: Step::Prepare,
                artifact: Some(PathBuf::from("/releases/a")),
                params: Some(file.clone()),
            },
            mode: Mode::FailAt(Step::Cleanup),
            pid_file: Some(pids),
        })
    );
    let file = params_file(dir.path(), "\"fixture.mode\" = \"ok\"\n");
    let found = parse_step(&["cleanup", "--params", file.to_str().expect("utf-8")]);
    assert_eq!(
        found.expect("parses"),
        Run::Step(step_run_at(Step::Cleanup, Mode::Ok, None, &file))
    );
}

fn step_run_at(step: Step, mode: Mode, pid_file: Option<PathBuf>, params: &Path) -> StepRun {
    StepRun {
        invocation: Invocation {
            step,
            artifact: None,
            params: Some(params.to_path_buf()),
        },
        mode,
        pid_file,
    }
}

/// Catches an `ok` step that exits otherwise than `0`, writes a last line the updater does not
/// read as the fixture's report (the line round-trips through `hostkeeper_hook::Report`), or
/// leaves no log line on standard error.
#[test]
fn the_fixture_hook_prints_a_valid_last_line_for_ok() {
    for step in Step::ALL {
        let (ending, out, err) = run(&step_run(step, Mode::Ok, None), Path::new("/no-exe"));
        assert_eq!(ending.expect("runs"), Ending::Exit(Outcome::Success));
        let line = out.strip_suffix('\n').expect("one line and a newline");
        assert!(!line.contains('\n'), "{out:?}");
        assert_eq!(Report::parse_line(line).expect("a report"), ok_report(step));
        assert_eq!(Report::read(&out), ok_report(step));
        assert_eq!(err, format!("fixture: {step} ok\n"));
    }
}

/// Catches `fail-at` failing at another step than the one it names, or failing without its
/// reason on the last line: it exits `1` with [`failed_report`] at that step and is `ok` at the
/// others.
#[test]
fn fail_at_fails_its_step_only() {
    let mode = Mode::FailAt(Step::Cutover);
    let (ending, out, err) = run(
        &step_run(Step::Cutover, mode.clone(), None),
        Path::new("/x"),
    );
    assert_eq!(ending.expect("runs"), Ending::Exit(Outcome::Failure));
    assert_eq!(Report::read(&out), failed_report(Step::Cutover));
    assert_eq!(
        Report::read(&out).reason.as_deref(),
        Some(fixture::FAILED_REASON)
    );
    assert_eq!(err, "fixture: cutover fails\n");
    let (ending, out, _) = run(&step_run(Step::Prepare, mode, None), Path::new("/x"));
    assert_eq!(ending.expect("runs"), Ending::Exit(Outcome::Success));
    assert_eq!(Report::read(&out), ok_report(Step::Prepare));
}

/// Catches `sleep` that exits instead of running on, writes to standard output, or loses its
/// pid file.
#[test]
fn sleep_blocks_with_its_pid_file() {
    let pids = PathBuf::from("/scratch/pids");
    let (ending, out, err) = run(
        &step_run(Step::Verify, Mode::Sleep, Some(pids.clone())),
        Path::new("/x"),
    );
    assert_eq!(
        ending.expect("runs"),
        Ending::Block {
            pid_file: Some(pids)
        }
    );
    assert_eq!(out, "");
    assert_eq!(err, "fixture: verify runs until it is stopped\n");
    let (ending, _, _) = run(&step_run(Step::Verify, Mode::Sleep, None), Path::new("/x"));
    assert_eq!(ending.expect("runs"), Ending::Block { pid_file: None });
}

/// Catches a flood of another size than asked, one that does not end with a newline (the report
/// would not be a line of its own), or one that hides the report.
#[test]
fn flood_writes_exactly_its_bytes_then_the_report() {
    assert_eq!(FLOOD_LINE.len(), 64);
    assert_eq!(FLOOD_LINE.last(), Some(&b'\n'));
    for bytes in [0_u64, 1, 63, 64, 100, 64 * 1024 + 5] {
        let (ending, out, _) = run(
            &step_run(Step::Cutover, Mode::Flood(bytes), None),
            Path::new("/x"),
        );
        assert_eq!(ending.expect("runs"), Ending::Exit(Outcome::Success));
        let report = format!("{}\n", ok_report(Step::Cutover).to_line());
        let filler = out.strip_suffix(&report).expect("the report is last");
        assert_eq!(filler.len() as u64, bytes);
        assert!(bytes == 0 || filler.ends_with('\n'), "{bytes}");
        assert!(filler.bytes().all(|b| FLOOD_LINE.contains(&b)), "{bytes}");
        assert_eq!(Report::read(&out), ok_report(Step::Cutover));
    }
}

/// Catches `print-pid` that writes another pid, a digest of another file than its executable, or
/// nothing at all, and one that runs without the file it must write.
#[test]
fn print_pid_writes_its_pid_and_the_digest_of_its_executable() {
    let dir = scratch("print-pid");
    let exe = dir.path().join("exe");
    std::fs::write(&exe, b"abc").expect("write the executable");
    let pids = dir.path().join("pids");
    let (ending, out, _) = run(
        &step_run(Step::Verify, Mode::PrintPid, Some(pids.clone())),
        &exe,
    );
    assert_eq!(ending.expect("runs"), Ending::Exit(Outcome::Success));
    assert_eq!(Report::read(&out), ok_report(Step::Verify));
    // SHA-256 of "abc".
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(
        std::fs::read_to_string(&pids).expect("the pid file"),
        format!("pid 42\nexe-sha256 {abc}\n")
    );
    assert_eq!(
        fixture::read_pid_file(&pids).expect("reads"),
        PidFile {
            pids: vec![42],
            exe_sha256: Some(abc.to_owned()),
        }
    );
    assert_eq!(fixture::file_sha256(&exe).expect("hashes"), abc);
    let (ending, _, _) = run(&step_run(Step::Verify, Mode::PrintPid, None), &exe);
    assert_eq!(
        ending.expect_err("no pid file").to_string(),
        "print-pid needs fixture.pid_file"
    );
    let (ending, _, _) = run(
        &step_run(Step::Verify, Mode::PrintPid, Some(pids)),
        &dir.path().join("no-exe"),
    );
    assert!(matches!(ending, Err(FixtureError::Io(_))), "{ending:?}");
}

/// Catches a pid file read partly or wrongly: the text round-trips, a line of another form, a
/// pid that is not a number and a file with no pid are refused, and a file that cannot be
/// written or read is an error.
#[test]
fn a_pid_file_round_trips_and_a_bad_one_is_refused() {
    let dir = scratch("pid-file");
    let path = dir.path().join("pids");
    fixture::write_pid_file(&path, &[7, 8], None).expect("writes");
    assert_eq!(
        std::fs::read_to_string(&path).expect("reads"),
        "pid 7\npid 8\n"
    );
    assert!(
        !dir.path().join("pids-part").exists(),
        "the part file is renamed"
    );
    assert_eq!(
        fixture::read_pid_file(&path).expect("parses"),
        PidFile {
            pids: vec![7, 8],
            exe_sha256: None,
        }
    );
    for (text, want) in [
        (
            "pid x\n",
            "the pid file holds a line that is not `pid <n>` or `exe-sha256 <hex>`: \"pid x\"",
        ),
        (
            "pid 1\nport 2\n",
            "the pid file holds a line that is not `pid <n>` or `exe-sha256 <hex>`: \"port 2\"",
        ),
        (
            "pid1\n",
            "the pid file holds a line that is not `pid <n>` or `exe-sha256 <hex>`: \"pid1\"",
        ),
        ("", "the pid file holds no pid"),
        ("exe-sha256 ab\n", "the pid file holds no pid"),
    ] {
        let err = fixture::parse_pid_file(text).expect_err(text);
        assert_eq!(err.to_string(), want, "{text:?}");
    }
    let nowhere = dir.path().join("missing").join("pids");
    assert!(fixture::write_pid_file(&nowhere, &[1], None).is_err());
    assert!(matches!(
        fixture::read_pid_file(&nowhere),
        Err(FixtureError::Io(_))
    ));
}

/// Catches a privileged test that would act as another user than root, or on a host whose
/// service manager is not systemd: each is refused with its reason, the user first.
#[test]
fn a_privileged_test_refuses_a_host_that_is_not_root_with_systemd() {
    let dir = scratch("host");
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write a file");
    let missing = dir.path().join("missing");
    assert_eq!(check_host(false, dir.path()), Err(HostError::NotRoot));
    assert_eq!(
        HostError::NotRoot.to_string(),
        "the privileged tests run only as root (effective uid 0): run them as \
         .github/workflows/privileged.yml does, in a throwaway VM, never on a workstation"
    );
    assert_eq!(check_host(false, &missing), Err(HostError::NotRoot));
    for not_dir in [&missing, &file] {
        let err = check_host(true, not_dir).expect_err("not a directory");
        assert_eq!(err, HostError::NoSystemd(not_dir.clone()));
        assert_eq!(
            err.to_string(),
            format!(
                "the privileged tests need systemd as the service manager, and {not_dir:?} is \
                 not a directory"
            )
        );
    }
    assert_eq!(check_host(true, dir.path()), Ok(()));
    assert_eq!(crate::SYSTEMD_RUNTIME_DIR, "/run/systemd/system");
}

fn rig(dir: &Path) -> Rig {
    Rig::new(
        dir,
        Path::new("/target/debug/hostkeeper-fixture-hook"),
        "a_test_1",
        77,
    )
    .expect("a rig")
}

/// Catches a rig made in a relative directory or with a test name that would not make a plain
/// unit name, and a unit file written for a fixture path that `ExecStart=` would read otherwise.
#[test]
fn a_rig_refuses_relative_paths_odd_paths_and_bad_test_names() {
    let fixture = Path::new("/target/debug/hostkeeper-fixture-hook");
    let err = Rig::new(Path::new("scratch"), fixture, "t", 1).expect_err("relative dir");
    assert_eq!(err.to_string(), "\"scratch\" is not an absolute path");
    let err = Rig::new(Path::new("/s"), Path::new("fixture"), "t", 1).expect_err("relative");
    assert!(matches!(err, RigError::NotAbsolute(p) if p == Path::new("fixture")));
    for odd_path in ["/a b/fixture", "/a%b", "/a\\b", "/a\"b", "/a;b", "/a$b"] {
        let rig = Rig::new(Path::new("/s"), Path::new(odd_path), "t", 1).expect("a rig");
        let err = rig.unit_file_text().expect_err(odd_path);
        assert_eq!(
            err.to_string(),
            format!(
                "{odd_path:?} holds a character other than letters, digits and `/._+-`, which \
                 a unit file's ExecStart= would read otherwise"
            )
        );
    }
    let long = "a".repeat(rig::MAX_TEST_NAME + 1);
    for name in ["", "Upper", "has-dash", "a b", long.as_str()] {
        let err = Rig::new(Path::new("/s"), fixture, name, 1).expect_err(name);
        assert_eq!(
            err.to_string(),
            format!("the test name {name:?} is not 1 to 64 lowercase letters, digits and `_`")
        );
    }
    let longest = "z".repeat(rig::MAX_TEST_NAME);
    let plain = Rig::new(Path::new("/s"), Path::new("/a+b/c.d_e-1"), &longest, 1).expect("a rig");
    assert!(plain.unit_file_text().is_ok());
}

/// Catches two units of one test, or of two tests, given the same name, a name the journal step
/// of privileged.yml does not find, or a unit left out of the teardown.
#[test]
fn a_rig_names_each_unit_once_and_tears_down_every_one() {
    let dir = scratch("rig-units");
    let rig = rig(dir.path());
    assert_eq!(rig.dir(), dir.path());
    assert_eq!(
        rig.fixture(),
        Path::new("/target/debug/hostkeeper-fixture-hook")
    );
    let (a, b) = (rig.unit(), rig.unit());
    assert_eq!(a, "hostkeeper-ci-a-test-1-77-0");
    assert_eq!(b, "hostkeeper-ci-a-test-1-77-1");
    assert!(a.starts_with(crate::UNIT_PREFIX));
    assert_eq!(rig.units(), [a.clone(), b.clone()]);
    let other = Rig::new(dir.path(), rig.fixture(), "a_test_1", 78).expect("a rig");
    assert_ne!(other.unit(), a, "another process names its units apart");
    let teardown: Vec<Vec<OsString>> = ["0", "1"]
        .iter()
        .flat_map(|n| {
            let unit = format!("hostkeeper-ci-a-test-1-77-{n}.service");
            ["stop", "disable", "reset-failed"].map(|verb| os(&[verb, "--system", unit.as_str()]))
        })
        .collect();
    assert_eq!(rig.teardown(), teardown);
    let none = Rig::new(dir.path(), rig.fixture(), "none", 1).expect("a rig");
    assert!(none.teardown().is_empty());
}

/// Catches a params file the fixture does not read back as the mode and pid file it was written
/// with, a pid file outside the test's directory, and a write to a directory that is gone
/// reported as success.
#[test]
fn a_rig_writes_params_the_fixture_reads_back() {
    let dir = scratch("rig-params");
    let rig = rig(dir.path());
    let pids = rig.pid_file("sleep");
    assert_eq!(pids, dir.path().join("sleep-pids"));
    let file = rig
        .params("sleep", &Mode::Sleep, Some(pids.as_path()))
        .expect("writes");
    assert_eq!(file, dir.path().join("sleep-params"));
    let params = Params::read(&file).expect("a params file");
    assert!(params.get(MODE_KEY).is_some() && params.get(PID_FILE_KEY).is_some());
    let found = parse_step(&["cutover", "--params", file.to_str().expect("utf-8")]);
    assert_eq!(
        found.expect("parses"),
        Run::Step(step_run_at(Step::Cutover, Mode::Sleep, Some(pids), &file))
    );
    let file = rig.params("ok", &Mode::Ok, None).expect("writes");
    let found = parse_step(&["cutover", "--params", file.to_str().expect("utf-8")]);
    assert_eq!(
        found.expect("parses"),
        Run::Step(step_run_at(Step::Cutover, Mode::Ok, None, &file))
    );
    let gone = Rig::new(&dir.path().join("gone"), rig.fixture(), "t", 1).expect("a rig");
    assert!(matches!(
        gone.params("ok", &Mode::Ok, None),
        Err(RigError::Io(_))
    ));
}

/// Catches a unit file whose main process is not the fixture's `serve`, or of another type than
/// `exec`, and a transient unit of `serve` started with a property or waited for.
#[test]
fn a_rig_writes_the_unit_of_serve_and_its_transient_line() {
    let dir = scratch("rig-unit-file");
    let rig = rig(dir.path());
    let text = "[Unit]\nDescription=hostkeeper privileged test: the fixture's serve\n\n\
                [Service]\nType=exec\nExecStart=/target/debug/hostkeeper-fixture-hook serve\n";
    assert_eq!(rig.unit_file_text().expect("a plain path"), text);
    let path = rig.unit_file("hostkeeper-ci-x-1-0").expect("writes");
    assert_eq!(path, dir.path().join("hostkeeper-ci-x-1-0.service"));
    assert_eq!(std::fs::read_to_string(&path).expect("reads"), text);
    let gone = Rig::new(&dir.path().join("gone"), rig.fixture(), "t", 1).expect("a rig");
    assert!(gone.unit_file("u").is_err());
    assert_eq!(
        rig.serve_command("hostkeeper-ci-x-1-0"),
        os(&[
            "--system",
            "--quiet",
            "--collect",
            "--unit=hostkeeper-ci-x-1-0",
            "--",
            "/target/debug/hostkeeper-fixture-hook",
            "serve",
        ])
    );
}

/// Catches a `systemctl show` line that asks another unit or other properties, and a reading of
/// its output that drops or mangles a value holding `=`.
#[test]
fn show_asks_the_named_properties_and_its_output_is_read_by_key() {
    assert_eq!(
        rig::show_command("hostkeeper-ci-x-1-0", &["ActiveState", "RuntimeMaxUSec"]),
        os(&[
            "show",
            "--system",
            "--property=ActiveState,RuntimeMaxUSec",
            "--",
            "hostkeeper-ci-x-1-0.service",
        ])
    );
    let found = rig::parse_show("ActiveState=active\nExecStart=a=b\nnoise\nUser=\n");
    let want: Vec<(String, String)> = [
        ("ActiveState", "active"),
        ("ExecStart", "a=b"),
        ("User", ""),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .to_vec();
    assert_eq!(found.into_iter().collect::<Vec<_>>(), want);
}

/// Catches the feature `privileged` turned on in the workspace's own test run (by a default, or
/// by a crate that depends on this one with it): the privileged tests would then be compiled
/// and run as part of the pull-request set. privileged.yml runs only the privileged test
/// targets, never this one.
#[test]
fn privileged_tests_are_compiled_out_by_default() {
    // Read through `black_box`: the value is the build's, and the test is about the build.
    assert!(!std::hint::black_box(PRIVILEGED));
}
