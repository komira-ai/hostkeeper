use super::*;
use crate::channels::spec::HOSTKEEPER;
use crate::testdir::TestDir;

fn all(result: &str) -> Vec<String> {
    HOSTKEEPER
        .stages
        .iter()
        .map(|s| format!("{s}={result}"))
        .collect()
}

/// A run in which everything succeeded, but for `changes` (stage, result).
fn results(changes: &[(&str, &str)]) -> Vec<String> {
    let mut base = all("success");
    for (stage, result) in changes {
        let at = HOSTKEEPER
            .stages
            .iter()
            .position(|s| s == stage)
            .expect("a stage");
        base[at] = format!("{stage}={result}");
    }
    base
}

fn refusal(results: &[String]) -> Vec<String> {
    match decide(&HOSTKEEPER, results, None) {
        Err(Error::Refused(problems)) => problems,
        other => panic!("not refused: {other:?}"),
    }
}

fn usage(results: &[String]) -> String {
    match decide(&HOSTKEEPER, results, None) {
        Err(Error::Usage(why)) => why,
        other => panic!("not a usage error: {other:?}"),
    }
}

#[test]
fn the_stages_are_the_chains_of_the_apt_rpm_and_installer_channels_and_their_checks() {
    assert_eq!(
        HOSTKEEPER.stages,
        [
            "testing-apt-write",
            "testing-apt-write-2",
            "testing-apt-write-3",
            "testing-rpm-write",
            "testing-rpm-write-2",
            "testing-rpm-write-3",
            "testing-pkg-upload",
            "check-apt",
            "check-rpm",
            "check-pkg",
        ]
    );
    // Every stage that may be skipped is a stage.
    assert!(
        HOSTKEEPER
            .may_skip
            .iter()
            .all(|s| HOSTKEEPER.stages.contains(s))
    );
}

#[test]
fn a_release_every_stage_of_which_succeeded_is_promoted_and_says_so_in_the_output() {
    let beside = std::env::current_exe().expect("test binary");
    let dir = TestDir::under(
        &beside.parent().expect("deps").join("xtask-decide-unit"),
        "promoted",
    );
    let out = dir.join("out");
    let said = decide(&HOSTKEEPER, &results(&[]), Some(&out)).expect("promoted");
    assert_eq!(said, ["promote: the release is promoted from testing"]);
    assert_eq!(
        std::fs::read_to_string(&out).expect("output"),
        "promote=true\n"
    );
}

#[test]
fn the_stages_that_may_be_skipped_are_the_later_attempts_and_the_checks() {
    // The first write did not lose the race, so its later attempts were skipped; nothing was
    // published to check, so the checks were skipped.
    for stage in HOSTKEEPER.may_skip {
        decide(&HOSTKEEPER, &results(&[(stage, "skipped")]), None)
            .unwrap_or_else(|e| panic!("{stage}: {e}"));
    }
    assert_eq!(
        HOSTKEEPER.may_skip,
        [
            "testing-apt-write-2",
            "testing-apt-write-3",
            "testing-rpm-write-2",
            "testing-rpm-write-3",
            "check-apt",
            "check-rpm",
            "check-pkg",
        ]
    );
}

#[test]
fn a_stage_that_must_succeed_and_was_skipped_holds_the_release() {
    for stage in [
        "testing-apt-write",
        "testing-rpm-write",
        "testing-pkg-upload",
    ] {
        assert_eq!(
            refusal(&results(&[(stage, "skipped")])),
            [format!("{stage} is skipped: the release does not go on")]
        );
    }
}

#[test]
fn a_failed_or_cancelled_stage_holds_the_release_and_every_one_is_named() {
    let problems = refusal(&results(&[
        ("check-apt", "failure"),
        ("check-pkg", "cancelled"),
        ("testing-rpm-write-2", "failure"),
    ]));
    assert_eq!(
        problems,
        [
            "testing-rpm-write-2 is failure: the release does not go on",
            "check-apt is failure: the release does not go on",
            "check-pkg is cancelled: the release does not go on",
        ]
    );
}

#[test]
fn results_that_do_not_fit_are_usage_errors() {
    let base = results(&[]);
    assert_eq!(
        usage(&["check-apt".to_owned()]),
        "--result \"check-apt\" is not NAME=RESULT"
    );
    let mut with_unknown = base.clone();
    with_unknown.push("farm=success".to_owned());
    assert_eq!(
        usage(&with_unknown),
        "--result names \"farm\", which is no stage"
    );
    let mut odd = base.clone();
    odd[0] = "testing-apt-write=ok".to_owned();
    assert!(usage(&odd).starts_with("--result testing-apt-write is \"ok\", not one of "));
    let mut twice = base.clone();
    twice.push("check-pkg=success".to_owned());
    assert_eq!(usage(&twice), "--result check-pkg is given twice");
    let mut missing = base;
    missing.pop();
    assert_eq!(usage(&missing), "--result check-pkg is not given");
}

/// The workflow that runs the verb.
const RELEASE_YML: &str = include_str!("../../../../../.github/workflows/release.yml");

#[test]
fn the_release_workflow_gives_decide_each_stage_and_no_other() {
    // The job `promote-check` of release.yml passes `--result NAME=$VAR` for each stage; a stage
    // it leaves out is a usage error of the verb at release time, and one it adds is too.
    let job = RELEASE_YML
        .split("\n  promote-check:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .expect("release.yml has the job promote-check");
    let given: Vec<&str> = job
        .split("--result \"")
        .skip(1)
        .filter_map(|rest| rest.split_once('=').map(|(stage, _)| stage))
        .collect();
    let mut want: Vec<&str> = HOSTKEEPER.stages.to_vec();
    let mut got = given.clone();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want);
    for stage in HOSTKEEPER.stages {
        let var = stage.to_uppercase().replace('-', "_");
        assert!(
            job.contains(&format!("{var}: ${{{{ needs.{stage}.result }}}}")),
            "{stage} is not the result of the job of its name"
        );
    }
}
