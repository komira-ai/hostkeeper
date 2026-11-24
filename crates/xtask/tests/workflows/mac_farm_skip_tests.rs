//! The jobs after the two compile jobs of artifacts.yml ([`super::mac_farm::after_compile`]): a
//! run of the committed file is played out job by job as GitHub decides whether a job runs, to
//! show that the job of the two compile jobs that is skipped stops nothing after it, while a
//! failed one stops everything after it; and the rule that holds those jobs' `if:` is shown to
//! refuse each job's condition without its status functions, by putting the defect into the
//! repository's file and reading the exact problem the lint reports.

use std::collections::BTreeMap;

use super::expr::{Context, holds};
use super::lint::{entries, entry, lint};
use super::mac_farm::{AFTER_COMPILE, CALLER_JOB, HOSTED_JOB, after_compile};
use super::release::{SIGN_IF, job, names, text_of};
use super::signing_run::{MACOS_SIGN_IF, SIGN_JOB_IF};
use super::yaml::{self, Node};
use super::{with_in, workflows};

/// What became of a job in a played-out run.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    Succeeded,
    Failed,
    Skipped,
}

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == "artifacts.yml")
        .map(|(_, text)| text)
        .expect("artifacts.yml exists")
}

/// Whether `condition` names a status function. GitHub reads a job's `if:` that names none as
/// `success() && (condition)`, and a missing `if:` as `success()`. (The evaluator does not know
/// `success()`, so a condition that names it fails the test by name.)
fn names_status(condition: &str) -> bool {
    ["always()", "cancelled()", "failure()"]
        .iter()
        .any(|f| condition.contains(f))
}

/// The jobs `name` needs, at any depth, in `needs`.
fn ancestors(needs: &BTreeMap<String, Vec<String>>, name: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut todo = vec![name.to_owned()];
    while let Some(next) = todo.pop() {
        for need in needs.get(&next).map_or(&[][..], Vec::as_slice) {
            if !found.contains(need) {
                found.push(need.clone());
                todo.push(need.clone());
            }
        }
    }
    found
}

/// Plays out a run of `text` (a workflow) whose expressions read `names`, as GitHub decides it:
/// a job runs when its `if:` holds, read as `success() && ...` when it names no status function,
/// where `success()` is that every job it needs, at any depth, succeeded, and `failure()` that
/// one of them failed; the run is never cancelled. A job in `failing` fails when it runs; every
/// other job that runs succeeds. A job's outcome is known once those of the jobs it needs are.
fn play(text: &str, names: &[(&str, &str)], failing: &[&str]) -> BTreeMap<String, Outcome> {
    let doc: Node = yaml::load(text).expect("the workflow parses");
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let needs: BTreeMap<String, Vec<String>> = jobs
        .iter()
        .filter_map(|(name, j)| Some((name.as_str()?.to_owned(), names_of(j))))
        .collect();
    let context_names: BTreeMap<String, String> = names
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    while outcomes.len() < needs.len() {
        let ready: Vec<&String> = needs
            .iter()
            .filter(|(name, direct)| {
                !outcomes.contains_key(*name) && direct.iter().all(|n| outcomes.contains_key(n))
            })
            .map(|(name, _)| name)
            .collect();
        assert!(!ready.is_empty(), "the needs of the workflow form a cycle");
        for name in ready {
            let before: Vec<Outcome> = ancestors(&needs, name)
                .iter()
                .map(|n| outcomes[n])
                .collect();
            let failed = before.contains(&Outcome::Failed);
            let all_succeeded = before.iter().all(|o| *o == Outcome::Succeeded);
            let context = Context {
                names: context_names.clone(),
                cancelled: false,
                failed,
            };
            let runs = match text_of(job(&doc, name), "if") {
                None => all_succeeded,
                Some(condition) => {
                    let holds =
                        holds(condition, &context).unwrap_or_else(|e| panic!("job {name}: {e}"));
                    holds && (names_status(condition) || all_succeeded)
                }
            };
            let outcome = match (runs, failing.contains(&name.as_str())) {
                (false, _) => Outcome::Skipped,
                (true, true) => Outcome::Failed,
                (true, false) => Outcome::Succeeded,
            };
            outcomes.insert(name.clone(), outcome);
        }
    }
    outcomes
}

/// The `needs:` of the job `j`, as names.
fn names_of(j: &Node) -> Vec<String> {
    names(entry(j, "needs"))
}

/// The names a push to main reads, with the farm's variable `farm`.
fn push_to_main(farm: &str) -> Vec<(&'static str, String)> {
    vec![
        ("github.event_name", "push".to_owned()),
        ("github.ref", "refs/heads/main".to_owned()),
        ("github.repository", "komira-ai/hostkeeper".to_owned()),
        ("vars.FARM_ADDRESS", farm.to_owned()),
    ]
}

/// `play` with owned names.
fn play_owned(
    text: &str,
    names: &[(&'static str, String)],
    failing: &[&str],
) -> BTreeMap<String, Outcome> {
    let borrowed: Vec<(&str, &str)> = names.iter().map(|(k, v)| (*k, v.as_str())).collect();
    play(text, &borrowed, failing)
}

/// The jobs of `outcomes` that ended as `want`, sorted.
fn ended(outcomes: &BTreeMap<String, Outcome>, want: Outcome) -> Vec<&str> {
    outcomes
        .iter()
        .filter(|(_, o)| **o == want)
        .map(|(n, _)| n.as_str())
        .collect()
}

/// The jobs after the compile jobs in the committed file, in its order.
const AFTER: [&str; 7] = [
    "sign-code",
    "build-macos",
    "sign-pkg",
    "check-pkg",
    "sums",
    "attest",
    "verify",
];

/// Catches the run on main that made no bundles: with the farm configured, compile-macos is
/// skipped, and a job after it under an `if:` with no status function (sign-pkg, check-pkg,
/// attest, verify) was skipped with it, so the release found no `bundles` artifact. On a push to
/// main every job runs but the compile job of the other half, the farm configured or not.
#[test]
fn a_push_to_main_runs_every_job_but_the_other_compile_job() {
    let text = artifacts_yml();
    for (farm, idle) in [("grpc://farm.example:8980", HOSTED_JOB), ("", CALLER_JOB)] {
        let outcomes = play_owned(&text, &push_to_main(farm), &[]);
        assert_eq!(ended(&outcomes, Outcome::Skipped), [idle], "farm {farm:?}");
        assert_eq!(ended(&outcomes, Outcome::Failed), Vec::<&str>::new());
    }
}

/// Catches a status function that lets a job run after a failure: when the compile job that runs
/// fails, every job after it is skipped (no unsigned or half-built program is signed, summed or
/// attested), and the jobs that need neither compile job still run.
#[test]
fn a_failed_compile_job_stops_every_job_after_it() {
    let text = artifacts_yml();
    for (farm, compile) in [("grpc://farm.example:8980", CALLER_JOB), ("", HOSTED_JOB)] {
        let outcomes = play_owned(&text, &push_to_main(farm), &[compile]);
        for name in AFTER {
            assert_eq!(outcomes[name], Outcome::Skipped, "{name}, farm {farm:?}");
        }
        assert_eq!(outcomes[compile], Outcome::Failed);
        for name in ["version", "build", "image"] {
            assert_eq!(outcomes[name], Outcome::Succeeded, "{name}, farm {farm:?}");
        }
    }
}

/// Catches the conditions of the signing jobs and of attest and verify losing their event or
/// ref while they gained the status functions: a run by hand on a branch signs only when asked
/// and never attests; a run by hand that does not ask still builds the macOS files and the sums.
#[test]
fn a_run_by_hand_signs_only_when_asked_and_never_attests() {
    let text = artifacts_yml();
    let by_hand = |sign: &str| {
        vec![
            ("github.event_name", "workflow_dispatch".to_owned()),
            ("github.ref", "refs/heads/topic".to_owned()),
            ("github.repository", "komira-ai/hostkeeper".to_owned()),
            ("vars.FARM_ADDRESS", "grpc://farm.example:8980".to_owned()),
            ("inputs.sign", sign.to_owned()),
        ]
    };
    let signing = play_owned(&text, &by_hand("true"), &[]);
    assert_eq!(
        ended(&signing, Outcome::Skipped),
        ["attest", HOSTED_JOB, "verify"]
    );
    let plain = play_owned(&text, &by_hand("false"), &[]);
    assert_eq!(
        ended(&plain, Outcome::Skipped),
        [
            "attest",
            "check-pkg",
            HOSTED_JOB,
            "sign-code",
            "sign-pkg",
            "verify"
        ]
    );
    assert_eq!(plain["build-macos"], Outcome::Succeeded);
    assert_eq!(plain["sums"], Outcome::Succeeded);
}

/// Catches the rule missing a job: the jobs it holds are exactly those whose needs reach a
/// compile job, at any depth (sums and verify only through others), and none that does not
/// (version, build and image).
#[test]
fn the_jobs_after_the_compile_jobs_are_those_that_need_one_at_any_depth() {
    let doc = yaml::load(&artifacts_yml()).expect("artifacts.yml parses");
    assert_eq!(after_compile(&doc), AFTER);
}

/// The problem of the job `name` whose `if:` does not start with the status functions.
fn problem(name: &str) -> String {
    format!(
        "artifacts.yml: job {name} needs {HOSTED_JOB} or {CALLER_JOB}, one of which every run \
         skips, so its `if:` must start with `{AFTER_COMPILE}`"
    )
}

/// Catches each job after the compile jobs put back under its condition without the status
/// functions, as sign-pkg, check-pkg, attest and verify were when the farm build first ran on
/// main: the lint refuses it by name, beside the rule that holds that job's exact `if:`.
#[test]
fn a_job_after_the_compile_jobs_without_the_status_functions_is_refused() {
    let text = artifacts_yml();
    let plain_sign = MACOS_SIGN_IF;
    let plain_attest = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}";
    for (name, head, condition, plain, exact) in [
        (
            "sign-pkg",
            "  sign-pkg:\n    needs: [version, build-macos]\n",
            SIGN_JOB_IF,
            plain_sign,
            "artifacts.yml: job sign-pkg needs the exact `if: ",
        ),
        (
            "check-pkg",
            "  check-pkg:\n    needs: [version, sign-pkg]\n",
            SIGN_JOB_IF,
            plain_sign,
            "artifacts.yml: job check-pkg needs the exact `if: ",
        ),
        (
            "attest",
            "  attest:\n    needs: [version, sums]\n",
            SIGN_IF,
            plain_attest,
            "artifacts.yml: job attest needs the exact `if: ",
        ),
        (
            "verify",
            "  verify:\n    needs: [version, attest]\n",
            SIGN_IF,
            plain_attest,
            "artifacts.yml: job verify needs the exact `if: ",
        ),
    ] {
        let defect = with_in(
            &text,
            &format!("{head}    if: {condition}\n"),
            &format!("{head}    if: {plain}\n"),
        );
        let found = lint("artifacts.yml", &defect);
        assert!(found.contains(&problem(name)), "{name}: {found:#?}");
        assert!(
            found.iter().any(|p| p.starts_with(exact)),
            "{name}: {found:#?}"
        );
        // And the run it describes: the job skipped on every push to main.
        let outcomes = play_owned(&defect, &push_to_main("grpc://farm.example:8980"), &[]);
        assert_eq!(outcomes[name], Outcome::Skipped, "{name}");
    }
}

/// Catches the rule passing an `if:` that only looks like the status functions: none at all, a
/// longer function name glued on, or a condition that names them later or in another order; and
/// a condition that starts with the term but widens the job with a `||`, which the exact `if:`
/// of the job refuses.
#[test]
fn an_if_that_does_not_start_with_the_whole_status_term_is_refused() {
    let text = artifacts_yml();
    let head = "  verify:\n    needs: [version, attest]\n";
    let rest = "github.event_name == 'push' && github.ref == 'refs/heads/main'";
    let from = format!("{head}    if: {SIGN_IF}\n");
    for to in [
        head.to_owned(),
        format!("{head}    if: ${{{{ !cancelled() && !failure()_x && {rest} }}}}\n"),
        format!("{head}    if: ${{{{ {rest} && !cancelled() && !failure() }}}}\n"),
        format!("{head}    if: ${{{{ !failure() && !cancelled() && {rest} }}}}\n"),
    ] {
        let found = lint("artifacts.yml", &with_in(&text, &from, &to));
        assert!(found.contains(&problem("verify")), "{to}: {found:#?}");
    }
    // The bare term, with nothing after it, is the whole condition sums and build-macos take:
    // this rule passes it (the exact `if:` of verify alone refuses it).
    let exact = format!("artifacts.yml: job verify needs the exact `if: {SIGN_IF}`");
    let bare = format!("{head}    if: ${{{{ !cancelled() && !failure() }}}}\n");
    assert_eq!(
        lint("artifacts.yml", &with_in(&text, &from, &bare)),
        std::slice::from_ref(&exact)
    );
    let widened = format!(
        "{head}    if: ${{{{ !cancelled() && !failure() && {rest} || github.event_name == 'pull_request' }}}}\n"
    );
    assert_eq!(
        lint("artifacts.yml", &with_in(&text, &from, &widened)),
        [exact]
    );
}

/// Catches the status functions widening where a job signs or attests: a push to a branch other
/// than main signs and attests nothing, and still builds the macOS files and the sums, the farm
/// configured or not.
#[test]
fn a_push_to_another_branch_signs_and_attests_nothing() {
    let text = artifacts_yml();
    for (farm, compile) in [("grpc://farm.example:8980", CALLER_JOB), ("", HOSTED_JOB)] {
        let names = vec![
            ("github.event_name", "push".to_owned()),
            ("github.ref", "refs/heads/topic".to_owned()),
            ("github.repository", "komira-ai/hostkeeper".to_owned()),
            ("vars.FARM_ADDRESS", farm.to_owned()),
        ];
        let outcomes = play_owned(&text, &names, &[]);
        for name in ["sign-code", "sign-pkg", "check-pkg", "attest", "verify"] {
            assert_eq!(outcomes[name], Outcome::Skipped, "{name}, farm {farm:?}");
        }
        for name in [compile, "build-macos", "sums"] {
            assert_eq!(outcomes[name], Outcome::Succeeded, "{name}, farm {farm:?}");
        }
    }
}

/// Catches the walk of the needs stopping at the first level, or looping on a cycle: a job that
/// reaches a compile job only through a chain of three is held, and a cycle of needs among jobs
/// that reach none ends the walk with nothing found.
#[test]
fn the_walk_of_the_needs_follows_any_depth_and_ends_on_a_cycle() {
    let doc = yaml::load(concat!(
        "jobs:\n",
        "  compile-macos:\n    runs-on: x\n",
        "  a:\n    needs: [compile-macos]\n    runs-on: x\n",
        "  b:\n    needs: a\n    runs-on: x\n",
        "  c:\n    needs: [b]\n    runs-on: x\n",
        "  d:\n    needs: [e]\n    runs-on: x\n",
        "  e:\n    needs: [d]\n    runs-on: x\n",
        "  f:\n    runs-on: x\n",
    ))
    .expect("the fixture parses");
    assert_eq!(after_compile(&doc), ["a", "b", "c"]);
}
