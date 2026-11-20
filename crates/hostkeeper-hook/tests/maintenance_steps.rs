//! The additive `drain` and `resume` steps of a maintenance run: their words, their command
//! lines, what the updater makes of how each ends, the params keys they read, and the exit code
//! of a drain whose deadline passed.

use std::ffi::OsString;
use std::path::PathBuf;

use hostkeeper_hook::{
    ArgsError, Class, Context, DRAIN_DEADLINE_KEY, EXIT_DEFERRED, Invocation, Next, Outcome,
    Params, ParamsError, Phase, RUN_ID_KEY, Report, SERVICE_KEY, Step, Verdict, next,
};

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// Catches a `drain` or `resume` the contract does not know, one that crept into the five steps
/// of an update (the harness runs every one of those against every hook), and a near word that
/// parses: both print, serialise and parse as their words, and are not among the five.
#[test]
fn drain_and_resume_are_words_of_the_contract_outside_the_five_steps() {
    let words: Vec<&str> = Step::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(
        words,
        ["prepare", "cutover", "verify", "rollback", "cleanup"]
    );
    for (step, word) in [(Step::Drain, "drain"), (Step::Resume, "resume")] {
        assert!(!Step::ALL.contains(&step));
        assert_eq!(step.name(), word);
        assert_eq!(step.to_string(), word);
        assert_eq!(word.parse::<Step>(), Ok(step));
        assert_eq!(
            serde_json::to_string(&step).expect("serialises"),
            format!("\"{word}\"")
        );
        assert_eq!(
            serde_json::from_str::<Step>(&format!("\"{word}\"")).expect("parses"),
            step
        );
    }
    for near in ["drains", "Resume", "undrain"] {
        assert_eq!(
            near.parse::<Step>(),
            Err(ArgsError::UnknownStep(near.to_owned()))
        );
    }
}

/// Catches a `drain` or `resume` command line that takes an artifact, drops the params file, or
/// does not write back as it was read.
#[test]
fn drain_and_resume_take_a_params_file_and_no_artifact() {
    let drain = Invocation::parse(os(&["drain", "--params", "/p"])).expect("parses");
    assert_eq!(
        drain,
        Invocation {
            step: Step::Drain,
            artifact: None,
            params: Some(PathBuf::from("/p")),
        }
    );
    assert_eq!(drain.to_args(), os(&["drain", "--params", "/p"]));

    let resume = Invocation::parse(os(&["resume"])).expect("parses");
    assert_eq!(
        resume,
        Invocation {
            step: Step::Resume,
            artifact: None,
            params: None,
        }
    );
    assert_eq!(resume.to_args(), os(&["resume"]));

    assert_eq!(
        Invocation::parse(os(&["drain", "/some/artifact"])),
        Err(ArgsError::Unexpected(OsString::from("/some/artifact")))
    );
    assert_eq!(
        Invocation::parse(os(&["resume", "--params"])),
        Err(ArgsError::MissingParamsFile)
    );
}

/// Catches a `drain` or `resume` read as a step of an update (a `Run` of another step or a
/// rollback) or a verdict that loses the hook's class or its backoff: each is reported by
/// itself, success as succeeded, failure as failed before the commit with the hook's class,
/// deferral with the hook's retry time, halt as halt.
#[test]
fn drain_and_resume_report_their_own_verdict() {
    let report = Report {
        class: Class::Environment,
        retry_after_secs: Some(120),
        ..Report::default()
    };
    let ctx = Context::default();
    let cases = [
        (
            Outcome::Success,
            Next::Report(Verdict::Succeeded {
                promotion_pending: false,
            }),
        ),
        (
            Outcome::Failure,
            Next::Report(Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Environment,
            }),
        ),
        (
            Outcome::Deferred,
            Next::Report(Verdict::Deferred {
                retry_after_secs: Some(120),
            }),
        ),
        (Outcome::Halt, Next::Report(Verdict::Halt)),
    ];
    for step in [Step::Drain, Step::Resume] {
        for (outcome, want) in &cases {
            assert_eq!(
                &next(step, *outcome, &report, &ctx),
                want,
                "{step} {outcome:?}"
            );
        }
    }
}

/// Catches the maintenance keys of the params file read wrong: they read back by their
/// accessors, an absent key is `None`, a deadline that is negative or not a number is refused
/// with its message, and a service or run id that is not text is refused as a bad reserved key.
#[test]
fn the_params_file_of_a_drain_carries_service_run_and_deadline() {
    assert_eq!(SERVICE_KEY, "updater.service");
    assert_eq!(RUN_ID_KEY, "updater.run_id");
    assert_eq!(DRAIN_DEADLINE_KEY, "updater.drain_deadline_unix");

    let params = Params::parse(concat!(
        "\"updater.service\" = \"js-agent\"\n",
        "\"updater.run_id\" = \"run-7\"\n",
        "\"updater.drain_deadline_unix\" = 1800000000\n",
    ))
    .expect("parses");
    assert_eq!(params.service(), Some("js-agent"));
    assert_eq!(params.run_id(), Some("run-7"));
    assert_eq!(params.drain_deadline_unix(), Some(1_800_000_000));

    let none = Params::parse("").expect("parses");
    assert_eq!(
        (none.service(), none.run_id(), none.drain_deadline_unix()),
        (None, None, None)
    );

    for bad in [
        "\"updater.drain_deadline_unix\" = -1\n",
        "\"updater.drain_deadline_unix\" = \"soon\"\n",
    ] {
        let err = Params::parse(bad).expect_err("refused");
        assert!(matches!(err, ParamsError::BadDrainDeadline), "{bad}");
        assert_eq!(
            err.to_string(),
            "updater.drain_deadline_unix must be a whole number of seconds since the Unix epoch, not negative"
        );
    }

    for key in ["updater.service", "updater.run_id"] {
        assert!(
            matches!(
                Params::parse(&format!("\"{key}\" = 3\n")),
                Err(ParamsError::BadReserved(k)) if k == key
            ),
            "{key}"
        );
    }
}

/// Catches a manifest whose own `[params]` could set a key only the updater writes: each new
/// key is refused there as reserved.
#[test]
fn the_manifest_params_may_not_set_the_maintenance_keys() {
    for (key, value) in [
        ("updater.service", "\"x\""),
        ("updater.run_id", "\"x\""),
        ("updater.drain_deadline_unix", "5"),
    ] {
        assert!(
            matches!(
                Params::parse_declared(&format!("\"{key}\" = {value}\n")),
                Err(ParamsError::ReservedKey(k)) if k == key
            ),
            "{key}"
        );
    }
}

/// Catches a drain that passed its deadline ending with another code than the deferral's: the
/// code is `75`, and the updater reads it as a deferral.
#[test]
fn a_drain_past_its_deadline_exits_75_which_reads_as_deferred() {
    assert_eq!(EXIT_DEFERRED, 75);
    assert_eq!(Outcome::from_exit(Some(75)), Outcome::Deferred);
}
