//! The hook contract is the one kbf shipped at its tag: the bytes a host, a hook or a file on
//! disk sees are the literals below, copied from kbf's `update-hook` crate at the tag and never
//! from this crate's source. The example hook passes the whole conformance harness, including
//! the allowance a hook may declare for a `rollback` rerun.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_hook::conformance::gate::{GATE_DIR_VAR, KILL_AT_VAR, PARKED};
use hostkeeper_hook::conformance::{Allowances, Conformance, HookCommand, Violation};
use hostkeeper_hook::{
    ARTIFACT_DIGEST_KEY, Class, EXIT_DEFERRED, EXIT_FAILURE, EXIT_HALT, EXIT_OK, Outcome,
    PREVIOUS_RELEASE_KEY, PREVIOUS_VERSION_KEY, PROMOTION_CEILING_KEY, Params, Phase, Report,
    STAGED_RELEASE_KEY, Step, TARGET_VERSION_KEY, Verdict,
};

mod support;
use support::{ExampleFake, Scratch, example};

/// The kbf release the literals of this file come from.
const KBF_TAG: &str = "v0.0.64";
/// The commit of that tag.
const KBF_COMMIT: &str = "8f90d0604794f3fee057e71647c7e1c1e034fa2e";

/// The example hook's arguments before the step, with its commit at `cutover`.
const IRREVERSIBLE: &[&str] = &["update-hook", "--irreversible-cutover"];

/// Catches a migration record that names a different kbf release than the one the literals of
/// this file were copied from: the two lines of MIGRATION.md must be exactly the tag and the
/// commit of this file.
#[test]
fn migration_names_the_tag_the_literals_come_from() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../MIGRATION.md");
    let text = fs::read_to_string(path).expect("read MIGRATION.md");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.contains(&format!("- kbf tag: `{KBF_TAG}`").as_str()));
    assert!(lines.contains(&format!("- kbf commit: `{KBF_COMMIT}`").as_str()));
}

/// Catches a constant of the contract that moved from kbf's tag: the exit codes, the reserved
/// `updater.*` keys (by name and as the params file reads them), the default `hook_args`, the
/// kill-point variables, the parked line, the step words, the `no_previous_release` reason and
/// the longest reason token.
#[test]
fn constants_match_kbf_at_the_tag() {
    assert_eq!(
        [EXIT_OK, EXIT_FAILURE, EXIT_DEFERRED, EXIT_HALT],
        [0, 1, 75, 76]
    );
    let outcomes = [
        Outcome::Success,
        Outcome::Failure,
        Outcome::Deferred,
        Outcome::Halt,
    ];
    assert_eq!(outcomes.map(Outcome::code), [0, 1, 75, 76]);
    assert_eq!(ARTIFACT_DIGEST_KEY, "updater.artifact_digest");
    assert_eq!(PREVIOUS_RELEASE_KEY, "updater.previous_release");
    assert_eq!(PREVIOUS_VERSION_KEY, "updater.previous_version");
    assert_eq!(PROMOTION_CEILING_KEY, "updater.promotion_ceiling_secs");
    assert_eq!(STAGED_RELEASE_KEY, "updater.staged_release");
    assert_eq!(TARGET_VERSION_KEY, "updater.target_version");
    assert_eq!(GATE_DIR_VAR, "UPDATE_HOOK_GATE_DIR");
    assert_eq!(KILL_AT_VAR, "UPDATE_HOOK_KILL_AT");
    assert_eq!(PARKED, "update-hook-gate: parked at the kill point");
    assert_eq!(
        hostkeeper_hook::conformance::NO_PREVIOUS_RELEASE,
        "no_previous_release"
    );
    assert_eq!(hostkeeper_hook::MAX_REASON_LEN, 64);
    let hook = HookCommand::new(Path::new("hook"));
    assert_eq!(
        hook.hook_args,
        vec![std::ffi::OsString::from("update-hook")]
    );
    let names: Vec<&str> = Step::ALL.iter().map(|step| step.name()).collect();
    assert_eq!(
        names,
        ["prepare", "cutover", "verify", "rollback", "cleanup"]
    );
    let shown: Vec<String> = Step::ALL.iter().map(Step::to_string).collect();
    assert_eq!(shown, names);
}

/// Catches a params file the tag's updater writes that this crate reads differently: each
/// reserved key, written with the literal of the tag, is read by its accessor, and the ceiling
/// the harness writes is the quoted literal key.
#[test]
fn reserved_params_keys_are_read_under_the_names_of_the_tag() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let text = format!(
        "\"updater.staged_release\" = \"/staged\"\n\
         \"updater.previous_release\" = \"/previous\"\n\
         \"updater.target_version\" = \"2.0.0\"\n\
         \"updater.previous_version\" = \"1.0.0\"\n\
         \"updater.artifact_digest\" = \"{digest}\"\n\
         \"updater.promotion_ceiling_secs\" = 3600\n"
    );
    let params = Params::parse(&text).expect("parse a params file of the tag");
    assert_eq!(params.staged_release(), Some(PathBuf::from("/staged")));
    assert_eq!(params.previous_release(), Some(PathBuf::from("/previous")));
    assert_eq!(params.target_version(), Some("2.0.0"));
    assert_eq!(params.previous_version(), Some("1.0.0"));
    assert_eq!(
        params.artifact_digest().expect("a valid digest"),
        Some(digest.as_str())
    );
    assert_eq!(params.promotion_ceiling_secs(), Some(3600));
    let written = Params::default()
        .with_promotion_ceiling_secs(3600)
        .to_toml();
    let table: toml::Table = written.parse().expect("the written file is TOML");
    assert_eq!(
        table.get("updater.promotion_ceiling_secs"),
        Some(&toml::Value::Integer(3600))
    );
}

/// Catches a wire word of the JSON last line or of the updater's verdict that changed since the
/// tag: the class words, the field names a report prints, and the phase words.
#[test]
fn report_and_verdict_wire_words_match_kbf_at_the_tag() {
    assert_eq!(Class::Release.as_str(), "release");
    assert_eq!(Class::Environment.as_str(), "environment");
    let report = Report {
        class: Class::Environment,
        reason: Some("state_invalid".to_owned()),
        retry_after_secs: Some(300),
        reversible: false,
        reversible_stated: true,
        promotion_pending: true,
        detail: Some("text".to_owned()),
    };
    let line: serde_json::Value =
        serde_json::from_str(&report.to_line()).expect("the line is JSON");
    assert_eq!(
        line,
        serde_json::json!({
            "class": "environment",
            "reversible": false,
            "promotion_pending": true,
            "reason": "state_invalid",
            "retry_after_secs": 300,
            "detail": "text",
        })
    );
    assert_eq!(
        Report::from_stdout(&report.to_line()).expect("read back"),
        report
    );
    let phases = [
        (Phase::BeforeCommit, "before_commit"),
        (Phase::GateRelease, "gate_release"),
        (Phase::GateEnvironment, "gate_environment"),
        (Phase::AfterCommit, "after_commit"),
        (Phase::Halt, "halt"),
    ];
    for (phase, word) in phases {
        assert_eq!(
            serde_json::to_string(&phase).expect("serialize a phase"),
            format!("\"{word}\"")
        );
    }
    let failed = Verdict::Failed {
        phase: Phase::AfterCommit,
        class: Class::Release,
    };
    assert_eq!(
        serde_json::to_string(&failed).expect("serialize a verdict"),
        r#"{"Failed":{"phase":"after_commit","class":"release"}}"#
    );
    assert_eq!(
        serde_json::to_string(&Verdict::Halt).expect("serialize a verdict"),
        "\"Halt\""
    );
}

/// The scenarios the harness runs, in order: success, then a failure, a deferral and a halt at
/// each of the four steps of an update, then three failures of `rollback` itself.
const SCENARIOS: [&str; 16] = [
    "none",
    "prepare=Fail",
    "prepare=Defer",
    "prepare=Halt",
    "cutover=Fail",
    "cutover=Defer",
    "cutover=Halt",
    "verify=Fail",
    "verify=Defer",
    "verify=Halt",
    "cleanup=Fail",
    "cleanup=Defer",
    "cleanup=Halt",
    "cutover=Fail,rollback=Fail",
    "cutover=Fail,rollback=Defer",
    "verify=Fail,rollback=Fail",
];

/// Every check of the harness on the example hook with these arguments, as an update or, with
/// `first_install`, as a first install: returns the number of kill points it was stopped at,
/// after asserting that the scenarios are exactly [`SCENARIOS`] and none was skipped.
fn passes(name: &str, args: &[&str], first_install: bool) -> u64 {
    let scratch = Scratch::new(name);
    let mut fake = ExampleFake::default();
    let hook = example(args);
    let mut harness = Conformance::new(&hook, &mut fake, scratch.path());
    if first_install {
        harness = harness.with_first_install();
    }
    let summary = harness
        .check_all()
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(summary.kill_points > 0, "{name}: no kill point was reached");
    assert_eq!(summary.scenarios, SCENARIOS, "{name}");
    assert_eq!(summary.skipped, Vec::<String>::new(), "{name}");
    summary.kill_points
}

/// Catches an example hook that stops passing the harness, or a harness that stops running a
/// check, in either mode. Four runs of `check_all`: the example with its commit at `cleanup`
/// and with its commit at `cutover`, each as an update (a previous release in the params) and as
/// a first install (none). Each run passes every check, is stopped at a positive number of kill
/// points (the same number in both modes, which the previous release does not change), and runs
/// all sixteen fault scenarios, a failure, a deferral and a halt at each step among them, none
/// skipped. Then a `rollback` rerun that says `reversible: true` where the first said `false`
/// is refused without the allowance and passes with it.
#[test]
fn example_hook_passes_the_conformance_harness() {
    for (name, args) in [
        ("kbf-tag", &["update-hook"][..]),
        ("kbf-tag-irreversible", IRREVERSIBLE),
    ] {
        let update = passes(&format!("{name}-update"), args, false);
        let first = passes(&format!("{name}-first"), args, true);
        assert_eq!(update, first, "{name}: kill points differ by mode");
    }

    let rerun = example(&[
        "update-hook",
        "--irreversible-cutover",
        "--behaviour=forgets-commit",
    ]);
    let strict = Scratch::new("kbf-tag-rerun-strict");
    let mut fake = ExampleFake::default();
    match Conformance::new(&rerun, &mut fake, strict.path()).check_all() {
        Err(Violation::RerunReversibleChanged {
            step: Step::Rollback,
            first: false,
            second: true,
        }) => {}
        other => panic!("{other:?}"),
    }
    let allow = Allowances {
        rollback_rerun_may_say_reversible: true,
        ..Allowances::default()
    };
    let allowed = Scratch::new("kbf-tag-rerun-allowed");
    let mut fake = ExampleFake::default();
    let summary = Conformance::new(&rerun, &mut fake, allowed.path())
        .with_allowances(allow)
        .check_all()
        .expect("declared: accepted");
    assert!(summary.kill_points > 0);
    assert_eq!(summary.scenarios, SCENARIOS);
    assert!(summary.skipped.is_empty());
}
