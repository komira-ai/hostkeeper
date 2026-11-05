//! The additive `uninstall` step (design section 4.7, "Uninstall"): its word, its command line,
//! what the updater makes of how it ends, and that the five steps of an update and the exit codes
//! are as they were.

use std::ffi::OsString;
use std::path::PathBuf;

use hostkeeper_hook::{
    ArgsError, Class, Context, EXIT_DEFERRED, EXIT_FAILURE, EXIT_HALT, EXIT_OK, Invocation, Next,
    Outcome, Phase, Report, Step, Verdict, next,
};

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// Catches an `uninstall` the contract does not know, one that crept into the steps of an update
/// (the conformance harness runs every one of those against every hook, kbf's included), and a
/// change to the words or exit codes the imported hooks already speak: `uninstall` parses, prints
/// and serialises as its word, takes `--params` and no artifact, is not among the five steps,
/// and maps 0, 1, 75 and 76 to the uninstall's verdict.
#[test]
fn uninstall_step_is_parsed_and_listed_in_the_contract() {
    // The five steps of an update are unchanged, and `uninstall` is not one of them.
    let words: Vec<&str> = Step::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(
        words,
        ["prepare", "cutover", "verify", "rollback", "cleanup"]
    );
    assert!(!Step::ALL.contains(&Step::Uninstall));

    // Its word, both ways, and as JSON (the step record's `step`).
    assert_eq!(Step::Uninstall.name(), "uninstall");
    assert_eq!(Step::Uninstall.to_string(), "uninstall");
    assert_eq!("uninstall".parse::<Step>(), Ok(Step::Uninstall));
    assert_eq!(
        serde_json::to_string(&Step::Uninstall).expect("serialises"),
        "\"uninstall\""
    );
    assert_eq!(
        serde_json::from_str::<Step>("\"uninstall\"").expect("parses"),
        Step::Uninstall
    );
    assert_eq!(
        "uninstal".parse::<Step>(),
        Err(ArgsError::UnknownStep("uninstal".to_owned()))
    );

    // Its command line: the params file, and no artifact.
    let invocation = Invocation::parse(os(&["uninstall", "--params", "/state/web/params.toml"]))
        .expect("parses");
    assert_eq!(
        invocation,
        Invocation {
            step: Step::Uninstall,
            artifact: None,
            params: Some(PathBuf::from("/state/web/params.toml")),
        }
    );
    assert_eq!(
        invocation.to_args(),
        os(&["uninstall", "--params", "/state/web/params.toml"])
    );
    assert_eq!(
        Invocation::parse(os(&["uninstall", "/staged"])),
        Err(ArgsError::Unexpected(OsString::from("/staged")))
    );

    // The exit codes are the four of every step.
    assert_eq!(
        (EXIT_OK, EXIT_FAILURE, EXIT_DEFERRED, EXIT_HALT),
        (0, 1, 75, 76)
    );
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
    for (outcome, want) in cases {
        assert_eq!(
            next(Step::Uninstall, outcome, &report, &ctx),
            want,
            "{outcome:?}"
        );
    }
}
