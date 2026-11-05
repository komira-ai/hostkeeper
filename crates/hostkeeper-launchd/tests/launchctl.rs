//! The reading of `launchctl print`, `print` and `bootout` over the scripted `launchctl`, and the
//! scripted `launchctl` itself.

use std::path::Path;

use hostkeeper_launchd::launchctl::{
    EXIT_NOT_LOADED, JobInfo, JobState, JobStatus, Launchctl, LaunchctlError, LaunchctlOutput,
    NOT_LOADED_TEXT, ParseError, PrintError, StopError, bootout, bootout_args, bootstrap_args,
    kickstart_args, parse_print, print, print_args,
};
use hostkeeper_launchd::{FakeLaunchctl, Label};

const RUNNING: &str = include_str!("fixtures/print-running.txt");
const WAITING_0: &str = include_str!("fixtures/print-waiting-exit-0.txt");
const WAITING_3: &str = include_str!("fixtures/print-waiting-exit-3.txt");
const SIGNAL: &str = include_str!("fixtures/print-signal.txt");
const SPAWN_SCHEDULED: &str = include_str!("fixtures/print-spawn-scheduled.txt");
const UNKNOWN_LINES: &str = include_str!("fixtures/print-unknown-lines.txt");
const UNKNOWN_STATE: &str = include_str!("fixtures/print-unknown-state.txt");
const TRUNCATED: &str = include_str!("fixtures/print-truncated.txt");

/// A fixture without its leading `# ` comment lines.
fn fixture(text: &str) -> String {
    text.lines()
        .skip_while(|line| line.starts_with("# "))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn info(state: JobState, pid: Option<u32>, exit: Option<i32>, signal: Option<i32>) -> JobInfo {
    JobInfo {
        state,
        pid,
        last_exit_status: exit,
        last_terminating_signal: signal,
    }
}

fn label() -> Label {
    Label::parse("dev.hostkeeper.web-main").expect("a label")
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn bad(key: &str, value: &str) -> Result<JobInfo, ParseError> {
    Err(ParseError::BadValue {
        key: key.to_owned(),
        value: value.to_owned(),
    })
}

/// A job block holding `lines` (tab indented) after its header.
fn job(lines: &[&str]) -> String {
    let body: String = lines.iter().map(|line| format!("\t{line}\n")).collect();
    format!("system/x = {{\n{body}}}\n")
}

/// Catches a running job read without its process id, or a "never exited" read as a code.
#[test]
fn a_running_job_has_its_pid() {
    assert_eq!(
        parse_print(&fixture(RUNNING)),
        Ok(info(JobState::Running, Some(4242), None, None))
    );
}

/// Catches a waiting job's exit code 0 lost.
#[test]
fn a_waiting_job_that_exited_zero() {
    assert_eq!(
        parse_print(&fixture(WAITING_0)),
        Ok(info(JobState::Waiting, None, Some(0), None))
    );
}

/// Catches a waiting job's non-zero exit code lost or read as 0.
#[test]
fn a_waiting_job_that_exited_three() {
    assert_eq!(
        parse_print(&fixture(WAITING_3)),
        Ok(info(JobState::Waiting, None, Some(3), None))
    );
}

/// Catches a job killed by a signal being given an exit code, or the signal number lost.
#[test]
fn a_job_killed_by_a_signal_has_no_exit_code() {
    let parsed = parse_print(&fixture(SIGNAL)).expect("parsed");
    assert_eq!(parsed.last_terminating_signal, Some(9));
    assert_eq!(parsed.last_exit_status, None);
    assert_eq!(parsed.state, JobState::NotRunning);
}

/// Catches "spawn scheduled" read as another state.
#[test]
fn a_spawn_scheduled_job() {
    assert_eq!(
        parse_print(&fixture(SPAWN_SCHEDULED)),
        Ok(info(JobState::SpawnScheduled, None, Some(1), None))
    );
}

/// Catches a state word this daemon does not know being refused, or its word lost.
#[test]
fn an_unknown_state_keeps_its_word() {
    let parsed = parse_print(&fixture(UNKNOWN_STATE)).expect("parsed");
    assert_eq!(parsed.state, JobState::Other("frobnicating".to_owned()));
    assert_eq!(parsed.state.word(), "frobnicating");
    assert_eq!(parsed.pid, Some(17));
}

/// Catches output cut before the closing brace being read as a whole job.
#[test]
fn a_truncated_print_is_refused() {
    assert_eq!(parse_print(&fixture(TRUNCATED)), Err(ParseError::Truncated));
}

/// Catches unknown keys, nested blocks holding their own `state` lines, blank lines, other
/// indentation, and lines after the closing brace changing what is read.
#[test]
fn an_unknown_extra_line_in_print_output_is_ignored() {
    assert_eq!(
        parse_print(&fixture(UNKNOWN_LINES)),
        parse_print(&fixture(RUNNING))
    );
    assert_eq!(
        parse_print(&fixture(UNKNOWN_LINES)),
        Ok(info(JobState::Running, Some(4242), None, None))
    );
}

/// Catches the header comment of a fixture reaching the parser unstripped as a valid header, and
/// the helper keeping its comment lines.
#[test]
fn a_fixture_header_is_not_a_job_header() {
    assert!(RUNNING.starts_with("# "));
    assert!(!fixture(RUNNING).starts_with('#'));
    assert_eq!(parse_print(RUNNING), Err(ParseError::NoHeader));
}

/// Catches empty or blank output being read as a job.
#[test]
fn no_text_is_empty() {
    assert_eq!(parse_print(""), Err(ParseError::Empty));
    assert_eq!(parse_print("  \n\t\n\n"), Err(ParseError::Empty));
}

/// Catches text that does not start with a job header being read.
#[test]
fn text_without_a_header_is_refused() {
    assert_eq!(parse_print("state = running\n"), Err(ParseError::NoHeader));
    assert_eq!(
        parse_print("Could not find service\n"),
        Err(ParseError::NoHeader)
    );
}

/// Catches a job without a state being read as having one.
#[test]
fn a_job_without_a_state() {
    assert_eq!(parse_print(&job(&["pid = 3"])), Err(ParseError::NoState));
    assert_eq!(parse_print("system/x = {\n}\n"), Err(ParseError::NoState));
}

/// Catches a key given twice being resolved silently, whichever spelling the second has.
#[test]
fn a_known_key_twice_is_refused() {
    let twice = |key: &str, lines: &[&str]| {
        assert_eq!(
            parse_print(&job(lines)),
            Err(ParseError::Duplicate {
                key: key.to_owned()
            })
        );
    };
    twice("state", &["state = running", "state = waiting"]);
    twice("pid", &["state = running", "pid = 1", "pid = 2"]);
    twice(
        "last exit status",
        &[
            "state = waiting",
            "last exit code = 1",
            "last exit status = 1",
        ],
    );
    twice(
        "last terminating signal",
        &[
            "state = waiting",
            "last terminating signal = 9",
            "last terminating signal = 9",
        ],
    );
}

/// Catches a value that is not a number being dropped or defaulted.
#[test]
fn values_that_are_not_numbers_are_refused() {
    assert_eq!(
        parse_print(&job(&["state = running", "pid = abc"])),
        bad("pid", "abc")
    );
    assert_eq!(
        parse_print(&job(&["state = running", "pid = -4"])),
        bad("pid", "-4")
    );
    assert_eq!(
        parse_print(&job(&["state = waiting", "last exit code = soon"])),
        bad("last exit code", "soon")
    );
    assert_eq!(
        parse_print(&job(&["state = waiting", "last exit code = x: EX_CONFIG"])),
        bad("last exit code", "x: EX_CONFIG")
    );
    assert_eq!(
        parse_print(&job(&[
            "state = waiting",
            "last terminating signal = Killed: nine"
        ])),
        bad("last terminating signal", "Killed: nine")
    );
}

/// Catches an exit code with a name after it being refused, and both spellings of the key not
/// being read.
#[test]
fn an_exit_code_may_carry_a_name_and_either_spelling() {
    let exit = |line: &str| {
        parse_print(&job(&["state = waiting", line]))
            .expect("parsed")
            .last_exit_status
    };
    assert_eq!(exit("last exit code = 78: EX_CONFIG"), Some(78));
    assert_eq!(exit("last exit status = 78: EX_CONFIG"), Some(78));
    assert_eq!(exit("last exit status = 5"), Some(5));
    assert_eq!(exit("last exit code = (never exited)"), None);
    assert_eq!(exit("last exit status = (never exited)"), None);
}

/// Catches a signal given as a bare number being refused, or a named one misread.
#[test]
fn a_signal_is_a_name_and_number_or_a_number() {
    let signal = |line: &str| {
        parse_print(&job(&["state = waiting", line]))
            .expect("parsed")
            .last_terminating_signal
    };
    assert_eq!(signal("last terminating signal = Killed: 9"), Some(9));
    assert_eq!(signal("last terminating signal = 15"), Some(15));
}

/// Catches a tab-only or space-only indentation, or a mix, being read differently.
#[test]
fn spaces_and_tabs_indent_alike() {
    let spaces = "system/x = {\n    state = running\n    pid = 7\n}\n";
    let tabs = "system/x = {\n\t\tstate = running\n\t pid = 7\n}\n";
    let flat = "system/x = {\nstate = running\npid = 7\n}\n";
    let want = Ok(info(JobState::Running, Some(7), None, None));
    assert_eq!(parse_print(spaces), want);
    assert_eq!(parse_print(tabs), want);
    assert_eq!(parse_print(flat), want);
}

/// Catches a `state` inside a nested block replacing the job's state, a block's close being
/// taken for the job's, and lines after the job's close being read.
#[test]
fn nested_blocks_are_skipped_and_the_text_after_the_close_is_ignored() {
    let text = "system/x = {\n\tstate = waiting\n\tinner = {\n\t\tstate = running\n\t\tdeeper = {\n\t\t\tpid = 9\n\t\t}\n\t\tpid = 8\n\t}\n}\nstate = running\npid = 5\n";
    assert_eq!(
        parse_print(text),
        Ok(info(JobState::Waiting, None, None, None))
    );
}

/// Catches a line inside the job with no ` = ` being an error or ending the job.
#[test]
fn a_line_without_an_equals_sign_is_ignored() {
    assert_eq!(
        parse_print(&job(&["just words", "state = exited", "key=nospaces"])),
        Ok(info(JobState::NotRunning, None, None, None))
    );
}

/// Catches the state words mapped to the wrong states, or a state printed as another word.
#[test]
fn state_words_round_trip() {
    let cases = [
        ("running", JobState::Running, "running"),
        ("waiting", JobState::Waiting, "waiting"),
        (
            "spawn scheduled",
            JobState::SpawnScheduled,
            "spawn scheduled",
        ),
        ("xpcproxy", JobState::Spawning, "xpcproxy"),
        ("not running", JobState::NotRunning, "not running"),
        ("exited", JobState::NotRunning, "not running"),
        ("odd", JobState::Other("odd".to_owned()), "odd"),
    ];
    for (word, state, printed) in cases {
        let parsed = parse_print(&job(&[&format!("state = {word}")])).expect("parsed");
        assert_eq!(parsed.state, state, "{word}");
        assert_eq!(parsed.state.word(), printed, "{word}");
    }
}

/// Catches the argument lists of the four commands drifting from what callers and tests expect.
#[test]
fn the_argument_lists() {
    let label = label();
    assert_eq!(
        print_args(&label),
        strings(&["print", "system/dev.hostkeeper.web-main"])
    );
    assert_eq!(
        bootout_args(&label),
        strings(&["bootout", "system/dev.hostkeeper.web-main"])
    );
    assert_eq!(
        kickstart_args(&label),
        strings(&["kickstart", "system/dev.hostkeeper.web-main"])
    );
    assert_eq!(
        bootstrap_args(Path::new("/Library/LaunchDaemons/a.plist")),
        strings(&["bootstrap", "system", "/Library/LaunchDaemons/a.plist"])
    );
}

/// Catches the output helpers filling the wrong stream or code.
#[test]
fn the_output_helpers() {
    let ok = LaunchctlOutput::success("text");
    assert_eq!(ok.code, Some(0));
    assert_eq!(ok.stdout, b"text");
    assert!(ok.stderr.is_empty());
    let failed = LaunchctlOutput::exit(5, "bad");
    assert_eq!(failed.code, Some(5));
    assert!(failed.stdout.is_empty());
    assert_eq!(failed.stderr_text(), "bad");
    let missing = LaunchctlOutput::not_loaded();
    assert_eq!(missing.code, Some(EXIT_NOT_LOADED));
    assert!(missing.stderr_text().starts_with(NOT_LOADED_TEXT));
    let odd = LaunchctlOutput {
        code: None,
        stdout: Vec::new(),
        stderr: vec![b'a', 0xff],
    };
    assert_eq!(odd.stderr_text(), "a\u{fffd}");
}

/// The fake answering `answer` to `print` of [`label`].
fn printing(answer: Result<LaunchctlOutput, LaunchctlError>) -> FakeLaunchctl {
    let fake = FakeLaunchctl::new();
    fake.then(&print_args(&label()), answer);
    fake
}

/// Catches a label the domain does not hold being an error, by the exit code or by the text in
/// either stream under any exit code.
#[test]
fn print_of_a_label_that_is_not_loaded() {
    let in_stdout = LaunchctlOutput {
        stdout: NOT_LOADED_TEXT.as_bytes().to_vec(),
        ..LaunchctlOutput::success("")
    };
    let cases = [
        LaunchctlOutput::exit(EXIT_NOT_LOADED, ""),
        LaunchctlOutput::not_loaded(),
        LaunchctlOutput::exit(1, &format!("{NOT_LOADED_TEXT}: system/x")),
        LaunchctlOutput::exit(0, NOT_LOADED_TEXT),
        in_stdout,
    ];
    for out in cases {
        let fake = printing(Ok(out.clone()));
        assert_eq!(print(&fake, &label()), Ok(JobStatus::NotLoaded), "{out:?}");
        assert_eq!(fake.calls(), [print_args(&label())]);
    }
}

/// Catches an exit that is neither success nor not-loaded being read as an answer.
#[test]
fn print_with_another_exit_is_an_error() {
    let fake = printing(Ok(LaunchctlOutput::exit(5, "boom")));
    assert_eq!(
        print(&fake, &label()),
        Err(PrintError::Exit {
            code: Some(5),
            stderr: "boom".to_owned()
        })
    );
    let signalled = printing(Ok(LaunchctlOutput {
        code: None,
        ..LaunchctlOutput::exit(0, "")
    }));
    assert_eq!(
        print(&signalled, &label()),
        Err(PrintError::Exit {
            code: None,
            stderr: String::new()
        })
    );
}

/// Catches output that is not UTF-8 being parsed lossily.
#[test]
fn print_of_output_that_is_not_utf8() {
    let fake = printing(Ok(LaunchctlOutput {
        stdout: vec![0xff, 0xfe],
        ..LaunchctlOutput::success("")
    }));
    assert_eq!(print(&fake, &label()), Err(PrintError::NotUtf8));
}

/// Catches a command that could not run being reported as a job's state, and a parse failure
/// being lost.
#[test]
fn print_reports_what_failed() {
    let spawn = LaunchctlError::Spawn {
        args: "print system/dev.hostkeeper.web-main".to_owned(),
        why: "gone".to_owned(),
    };
    assert_eq!(
        print(&printing(Err(spawn.clone())), &label()),
        Err(PrintError::Launchctl(spawn))
    );
    assert_eq!(
        print(
            &printing(Ok(LaunchctlOutput::success("nonsense\n"))),
            &label()
        ),
        Err(PrintError::Parse(ParseError::NoHeader))
    );
}

/// Catches a loaded job's parsed state being lost on the way out of `print`.
#[test]
fn print_of_a_loaded_job() {
    let fake = printing(Ok(LaunchctlOutput::success(&fixture(RUNNING))));
    assert_eq!(
        print(&fake, &label()),
        Ok(JobStatus::Loaded(info(
            JobState::Running,
            Some(4242),
            None,
            None
        )))
    );
}

fn loaded() -> Result<LaunchctlOutput, LaunchctlError> {
    Ok(LaunchctlOutput::success(&fixture(RUNNING)))
}

/// Catches a bootout of a label that is not loaded running `launchctl bootout` anyway.
#[test]
fn bootout_of_a_label_that_is_not_loaded_only_prints() {
    let fake = printing(Ok(LaunchctlOutput::not_loaded()));
    assert_eq!(bootout(&fake, &label()), Ok(()));
    assert_eq!(fake.calls(), [print_args(&label())]);
}

/// Catches a successful bootout being followed by more calls, or not made at all.
#[test]
fn bootout_of_a_loaded_label() {
    let fake = printing(loaded());
    fake.then(&bootout_args(&label()), Ok(LaunchctlOutput::success("")));
    assert_eq!(bootout(&fake, &label()), Ok(()));
    assert_eq!(fake.calls(), [print_args(&label()), bootout_args(&label())]);
}

/// Catches a failed bootout of a label that went away meanwhile being an error.
#[test]
fn a_failed_bootout_of_a_label_that_went_away_is_done() {
    let fake = printing(loaded());
    fake.then(
        &bootout_args(&label()),
        Ok(LaunchctlOutput::exit(5, "busy")),
    );
    fake.then(&print_args(&label()), Ok(LaunchctlOutput::not_loaded()));
    assert_eq!(bootout(&fake, &label()), Ok(()));
    assert_eq!(fake.calls().len(), 3);
}

/// Catches a failed bootout of a label still loaded being treated as stopped, and its reason
/// lost.
#[test]
fn a_failed_bootout_of_a_label_still_loaded_is_an_error() {
    let fake = printing(loaded());
    fake.then(
        &bootout_args(&label()),
        Ok(LaunchctlOutput::exit(5, "busy")),
    );
    fake.always(&print_args(&label()), loaded());
    let err = bootout(&fake, &label()).expect_err("still loaded");
    assert_eq!(
        err,
        StopError::Bootout {
            code: Some(5),
            stderr: "busy".to_owned()
        }
    );
    assert_eq!(
        err.to_string(),
        "launchctl bootout exited with Some(5): busy"
    );
}

/// Catches a print or a bootout command that cannot run being swallowed by `bootout`.
#[test]
fn bootout_reports_a_command_that_cannot_run() {
    let broken = LaunchctlError::TimedOut {
        args: "x".to_owned(),
    };
    let first = printing(Err(broken.clone()));
    assert_eq!(
        bootout(&first, &label()),
        Err(StopError::Print(PrintError::Launchctl(broken.clone())))
    );
    let second = printing(loaded());
    second.then(&bootout_args(&label()), Err(broken.clone()));
    assert_eq!(
        bootout(&second, &label()),
        Err(StopError::Launchctl(broken.clone()))
    );
    let third = printing(loaded());
    third.then(&bootout_args(&label()), Ok(LaunchctlOutput::exit(5, "")));
    third.then(&print_args(&label()), Err(broken.clone()));
    assert_eq!(
        bootout(&third, &label()),
        Err(StopError::Print(PrintError::Launchctl(broken)))
    );
}

/// Catches an error message changing what an operator reads.
#[test]
fn error_messages() {
    let spawn = LaunchctlError::Spawn {
        args: "print system/x".to_owned(),
        why: "gone".to_owned(),
    };
    assert_eq!(
        spawn.to_string(),
        "cannot run launchctl print system/x: gone"
    );
    let timed = LaunchctlError::TimedOut {
        args: "print system/x".to_owned(),
    };
    assert_eq!(
        timed.to_string(),
        "launchctl print system/x did not finish within its deadline and was killed"
    );
    assert_eq!(
        PrintError::Launchctl(timed.clone()).to_string(),
        timed.to_string()
    );
    assert_eq!(
        StopError::Launchctl(timed.clone()).to_string(),
        timed.to_string()
    );
    assert_eq!(
        StopError::Print(PrintError::NotUtf8).to_string(),
        "launchctl print wrote output that is not UTF-8"
    );
    assert_eq!(
        PrintError::Exit {
            code: None,
            stderr: "e".to_owned()
        }
        .to_string(),
        "launchctl print exited with None: e"
    );
    let parse = [
        (ParseError::Empty, "launchctl print wrote nothing"),
        (
            ParseError::NoHeader,
            "launchctl print did not start with a job header",
        ),
        (
            ParseError::Truncated,
            "launchctl print ended before the job's closing brace",
        ),
        (ParseError::NoState, "launchctl print has no state line"),
        (
            ParseError::BadValue {
                key: "pid".to_owned(),
                value: "x".to_owned(),
            },
            "launchctl print has \"x\" for \"pid\", which is not a number",
        ),
        (
            ParseError::Duplicate {
                key: "pid".to_owned(),
            },
            "launchctl print has \"pid\" more than once",
        ),
    ];
    for (err, text) in parse {
        assert_eq!(err.to_string(), text);
        assert_eq!(PrintError::Parse(err).to_string(), text);
    }
}

/// Catches queued answers given out of order, the steady answer used before the queue is empty,
/// or a queue and a steady answer of one argument list mixed up with another's.
#[test]
fn the_fake_gives_queued_answers_then_the_steady_one() {
    let fake = FakeLaunchctl::new();
    let one = strings(&["a"]);
    let two = strings(&["b"]);
    fake.then(&one, Ok(LaunchctlOutput::success("first")));
    fake.then(&one, Ok(LaunchctlOutput::success("second")));
    fake.always(&one, Ok(LaunchctlOutput::success("steady")));
    fake.always(&two, Ok(LaunchctlOutput::success("other")));
    let said = |args: &[String]| fake.run(args).expect("answered").stdout;
    assert_eq!(said(&one), b"first");
    assert_eq!(said(&two), b"other");
    assert_eq!(said(&one), b"second");
    assert_eq!(said(&one), b"steady");
    assert_eq!(said(&one), b"steady");
    assert_eq!(
        fake.calls(),
        [one.clone(), two, one.clone(), one.clone(), one]
    );
}

/// Catches an unscripted command being answered, and clones not sharing one script.
#[test]
fn the_fake_fails_what_it_has_no_answer_for_and_clones_share_a_script() {
    let fake = FakeLaunchctl::new();
    let clone = fake.clone();
    clone.always(&strings(&["a"]), Ok(LaunchctlOutput::success("x")));
    assert_eq!(
        fake.run(&strings(&["a"])),
        Ok(LaunchctlOutput::success("x"))
    );
    assert_eq!(
        fake.run(&strings(&["b", "c"])),
        Err(LaunchctlError::Spawn {
            args: "b c".to_owned(),
            why: "the fake has no answer".to_owned()
        })
    );
    assert_eq!(clone.calls(), [strings(&["a"]), strings(&["b", "c"])]);
}
