//! The signing run of `artifacts.yml` ([`super::signing_run`]): its conditions, evaluated for
//! every kind of run, sign where they should and refuse by name everywhere else; the committed
//! file passes; and each defect put into a copy of it is refused with its own line.

use std::collections::BTreeMap;

use super::expr::{Context, holds};
use super::lint::lint;
use super::pkg_signing::{CHECK_JOB, SIGN_JOB};
use super::release::SIGN_IF;
use super::signing_run::{
    ALLOWED, INPUT_PROBLEM, MACOS_SIGN_IF, REFUSE_IF, REFUSE_NAME, REFUSE_PROBLEM, REFUSE_RUN,
    SIGN_CODE_IF,
};
use super::{with_in, workflows};

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "artifacts.yml")
        .map(|(_, text)| text)
        .expect("artifacts.yml")
}

/// Fails unless the lint reports `want`, exactly, among the problems of `text`.
#[track_caller]
fn assert_refused(text: &str, want: &str) {
    let found = lint("artifacts.yml", text);
    assert!(
        found.iter().any(|p| p == want),
        "{want:?} is not among {found:?}"
    );
}

/// One kind of run: its event, ref, repository, and the `sign` input (`None` when the event has
/// no inputs, as a push has none).
struct Run {
    event: &'static str,
    git_ref: &'static str,
    repository: &'static str,
    sign: Option<bool>,
}

const REPO: &str = "komira-ai/hostkeeper";

impl Run {
    fn context(&self) -> Context {
        let mut names = BTreeMap::from([
            ("github.event_name".to_owned(), self.event.to_owned()),
            ("github.ref".to_owned(), self.git_ref.to_owned()),
            ("github.repository".to_owned(), self.repository.to_owned()),
        ]);
        if let Some(sign) = self.sign {
            names.insert("inputs.sign".to_owned(), sign.to_string());
        }
        Context {
            names,
            cancelled: false,
            failed: false,
        }
    }

    /// Whether `condition` holds for this run.
    fn holds(&self, condition: &str) -> bool {
        holds(condition, &self.context()).expect("the condition is understood")
    }
}

/// Every kind of run, with whether it signs (the signing jobs run) and whether the refusal stops
/// it.
const RUNS: [(Run, bool, bool); 10] = [
    (
        Run {
            event: "push",
            git_ref: "refs/heads/main",
            repository: REPO,
            sign: None,
        },
        true,
        false,
    ),
    // A push elsewhere (no trigger of the file starts one, but the condition still says no).
    (
        Run {
            event: "push",
            git_ref: "refs/heads/topic",
            repository: REPO,
            sign: None,
        },
        false,
        false,
    ),
    // A run by hand that does not ask, on a branch and on main: signs nothing, as before.
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/topic",
            repository: REPO,
            sign: Some(false),
        },
        false,
        false,
    ),
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/main",
            repository: REPO,
            sign: Some(false),
        },
        false,
        false,
    ),
    // A signing run on a branch of this repository, and on main.
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/task/sign",
            repository: REPO,
            sign: Some(true),
        },
        true,
        false,
    ),
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/main",
            repository: REPO,
            sign: Some(true),
        },
        true,
        false,
    ),
    // A signing run on a tag: refused by name.
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/tags/v0.1.0",
            repository: REPO,
            sign: Some(true),
        },
        false,
        true,
    ),
    // A ref that only contains `refs/heads/` further in is not a branch.
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/tags/refs/heads/main",
            repository: REPO,
            sign: Some(true),
        },
        false,
        true,
    ),
    // A fork's copy of the file, asked to sign: refused by name; not asked: nothing.
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/main",
            repository: "someone/hostkeeper",
            sign: Some(true),
        },
        false,
        true,
    ),
    (
        Run {
            event: "workflow_dispatch",
            git_ref: "refs/heads/main",
            repository: "someone/hostkeeper",
            sign: Some(false),
        },
        false,
        false,
    ),
];

#[test]
fn a_push_to_main_and_a_signing_run_on_a_branch_sign_and_nothing_else_does() {
    // Catches: a condition that signs on a run by hand that did not ask, on a tag or in a fork,
    // or that stops signing on a push to main.
    for (run, signs, _) in &RUNS {
        assert_eq!(
            run.holds(MACOS_SIGN_IF),
            *signs,
            "{} {} {} {:?}",
            run.event,
            run.git_ref,
            run.repository,
            run.sign
        );
    }
}

#[test]
fn a_signing_run_outside_a_branch_of_this_repository_is_refused_and_no_other_run_is() {
    // Catches: a refusal that stops a push to main or a run by hand that did not ask, or that
    // lets a signing run on a tag or in a fork go green unsigned.
    for (run, _, refused) in &RUNS {
        assert_eq!(
            run.holds(REFUSE_IF),
            *refused,
            "{} {} {} {:?}",
            run.event,
            run.git_ref,
            run.repository,
            run.sign
        );
    }
}

#[test]
fn every_signing_run_either_signs_or_is_refused_and_never_both() {
    // Catches: the two conditions drifting apart, so a signing run is neither signed nor
    // refused (green and unsigned) or both.
    for (run, _, _) in &RUNS {
        let asked = run.event == "workflow_dispatch" && run.sign == Some(true);
        let signs = run.holds(MACOS_SIGN_IF);
        let refused = run.holds(REFUSE_IF);
        assert!(!(signs && refused), "{}", run.git_ref);
        if asked {
            assert!(signs || refused, "{} {}", run.git_ref, run.repository);
        }
    }
    assert!(MACOS_SIGN_IF.contains(&format!("inputs.sign && {ALLOWED})")));
    assert!(REFUSE_IF.contains(&format!("inputs.sign && !({ALLOWED})")));
}

#[test]
fn a_signing_run_never_attests() {
    // Catches: attest or verify taking the signing condition, so a branch's run would hold
    // `id-token: write` and attest as the release's signer.
    for (run, _, _) in &RUNS {
        let push_to_main = run.event == "push" && run.git_ref == "refs/heads/main";
        assert_eq!(run.holds(SIGN_IF), push_to_main, "{}", run.git_ref);
    }
    let text = artifacts_yml();
    let attest = format!("  attest:\n    needs: [version, sums]\n    if: {SIGN_IF}\n");
    let signing = with_in(&text, &attest, &attest.replace(SIGN_IF, MACOS_SIGN_IF));
    assert_refused(
        &signing,
        &format!("artifacts.yml: job attest needs the exact `if: {SIGN_IF}`"),
    );
}

#[test]
fn the_evaluator_reads_a_boolean_input_as_a_boolean_and_starts_with_ignoring_case() {
    // Catches: the evaluator taking `false` for a true string (so every run by hand would look
    // like a signing run in the tests above), or a startsWith that tests only equality.
    let context = |sign: &str| Context {
        names: BTreeMap::from([
            ("inputs.sign".to_owned(), sign.to_owned()),
            ("github.ref".to_owned(), "refs/heads/Topic".to_owned()),
            ("other.value".to_owned(), "false".to_owned()),
        ]),
        cancelled: false,
        failed: false,
    };
    assert_eq!(holds("${{ inputs.sign }}", &context("false")), Ok(false));
    assert_eq!(holds("${{ inputs.sign }}", &context("true")), Ok(true));
    // Any other name keeps the string, which is truthy.
    assert_eq!(holds("${{ other.value }}", &context("true")), Ok(true));
    let starts = |prefix: &str| {
        holds(
            &format!("${{{{ startsWith(github.ref, '{prefix}') }}}}"),
            &context("true"),
        )
    };
    assert_eq!(starts("refs/heads/"), Ok(true));
    assert_eq!(starts("REFS/HEADS/topic"), Ok(true));
    assert_eq!(starts("refs/tags/"), Ok(false));
    assert_eq!(
        holds("${{ startsWith(github.ref) }}", &context("true")),
        Err("startsWith( takes two arguments".to_owned())
    );
    assert_eq!(
        holds("${{ startsWith(github.ref, 'refs/' }}", &context("true")),
        Err("a `startsWith(` that is not closed".to_owned())
    );
    assert_eq!(
        holds("${{ startsWith(inputs.sign, 'refs/') }}", &context("true")),
        Err("startsWith(Bool(true), Text(\"refs/\")): only strings are compared here".to_owned())
    );
}

#[test]
fn the_committed_artifacts_yml_has_the_one_input_the_refusal_first_and_signs_under_the_condition() {
    // Catches: the rules refusing the committed file, or the signing condition missing from a
    // signing job or a step of build-macos that takes or checks the signed programs (two jobs
    // under it and sign-code under its own, once no compile job failed,
    // the download of the signed programs, the two records of the packaged hostkeeper's
    // signature and the two checks of the packaged programs).
    let text = artifacts_yml();
    assert_eq!(lint("artifacts.yml", &text), Vec::<String>::new());
    assert_eq!(
        text.matches(&format!("\n    if: {MACOS_SIGN_IF}\n"))
            .count(),
        2
    );
    assert_eq!(
        text.matches(&format!("\n    if: {SIGN_CODE_IF}\n")).count(),
        1
    );
    assert_eq!(
        text.matches(&format!("        if: {MACOS_SIGN_IF}\n"))
            .count(),
        5
    );
    assert_eq!(
        text.matches(&format!("        if: {REFUSE_IF}\n")).count(),
        1
    );
    assert_eq!(
        text.matches(&format!("        run: {REFUSE_RUN}\n"))
            .count(),
        1
    );
}

/// The input as the file writes it.
const INPUT_BLOCK: &str = "    inputs:\n      sign:\n        description: Sign the macOS programs and pkgs and check them (a branch of this repository only; nothing is attested or released)\n        type: boolean\n        default: false\n";

#[test]
fn a_dispatch_with_another_input_shape_is_refused() {
    // Catches: a signing run asked by a string (any text, `false` too, is truthy), one that
    // signs by default, a second input that another job could read, or an input the lint
    // does not see.
    let text = artifacts_yml();
    let cases = [
        INPUT_BLOCK.replace("type: boolean", "type: string"),
        INPUT_BLOCK.replace("default: false", "default: true"),
        INPUT_BLOCK.replace("        default: false\n", ""),
        format!("{INPUT_BLOCK}        required: true\n"),
        format!("{INPUT_BLOCK}      publish:\n        type: boolean\n        default: false\n        description: Publish\n"),
        INPUT_BLOCK.replace(
            "        description: Sign the macOS programs and pkgs and check them (a branch of this repository only; nothing is attested or released)\n",
            "",
        ),
        INPUT_BLOCK.replace(
            "description: Sign the macOS programs and pkgs and check them (a branch of this repository only; nothing is attested or released)",
            "description: ' '",
        ),
        String::new(),
    ];
    for case in cases {
        assert_refused(&with_in(&text, INPUT_BLOCK, &case), INPUT_PROBLEM);
    }
    let no_dispatch = with_in(&text, &format!("  workflow_dispatch:\n{INPUT_BLOCK}"), "");
    assert_refused(&no_dispatch, INPUT_PROBLEM);
}

/// The refusal step as the file writes it, with its comment.
fn refusal() -> String {
    format!("      - name: '{REFUSE_NAME}'\n        if: {REFUSE_IF}\n        run: {REFUSE_RUN}\n")
}

#[test]
fn a_version_job_without_the_refusal_first_is_refused() {
    // Catches: a signing run on a tag or in a fork going green unsigned because the refusal is
    // gone, holds another condition or command, or runs after the checkout (a later step could
    // be skipped by the job's other steps failing first).
    let text = artifacts_yml();
    let step = refusal();
    let gone = with_in(&text, &step, "");
    assert_refused(&gone, REFUSE_PROBLEM);
    let other_if = with_in(
        &text,
        &step,
        &step.replace(
            REFUSE_IF,
            "${{ github.event_name == 'workflow_dispatch' && inputs.sign && !startsWith(github.ref, 'refs/heads/') }}",
        ),
    );
    assert_refused(&other_if, REFUSE_PROBLEM);
    let no_exit = with_in(&text, &step, &step.replace("run: exit 1", "run: exit 0"));
    assert_refused(&no_exit, REFUSE_PROBLEM);
    // A refusal whose name no longer says why the run stops.
    let unnamed = with_in(
        &text,
        &step,
        &step.replace(&format!("'{REFUSE_NAME}'"), "Stop"),
    );
    assert_refused(&unnamed, REFUSE_PROBLEM);
    let tolerated = with_in(
        &text,
        &step,
        &format!("{step}        continue-on-error: true\n"),
    );
    assert_refused(&tolerated, REFUSE_PROBLEM);
    let anchor =
        "      - run: rustup toolchain install\n      - name: The version of this commit\n";
    let moved = with_in(
        &with_in(&text, &step, ""),
        anchor,
        &format!(
            "      - run: rustup toolchain install\n{step}      - name: The version of this commit\n"
        ),
    );
    assert_refused(&moved, REFUSE_PROBLEM);
}

#[test]
fn an_input_read_outside_the_two_conditions_is_refused() {
    // Catches: a job, step, `env:` or `with:` of the file taking the input (a deploy or upload
    // switched on by it, say) without the rules seeing it.
    let text = artifacts_yml();
    let anchor = "    env:\n      V: ${{ needs.version.outputs.version }}\n";
    let env = with_in(
        &text,
        anchor,
        &format!("{anchor}      SIGN: ${{{{ inputs.sign }}}}\n"),
    );
    assert_refused(
        &env,
        "artifacts.yml: `${{ inputs.sign }}` reads an input; only the signing conditions may",
    );
    let legacy = "${{ github.event.inputs.sign == 'true' }}";
    let step = refusal();
    let other = with_in(&text, &step, &step.replace(REFUSE_IF, legacy));
    assert_refused(
        &other,
        &format!("artifacts.yml: `{legacy}` reads an input; only the signing conditions may"),
    );
}

#[test]
fn a_signing_job_under_the_push_only_condition_is_refused() {
    // Catches: the pkg jobs left at a push to main while sign-code signs in a signing run, so
    // the run would prove the programs' signing and never the pkgs'.
    let text = artifacts_yml();
    for (job, needs) in [
        (SIGN_JOB, "[version, build-macos]"),
        (CHECK_JOB, "[version, sign-pkg]"),
    ] {
        let head = format!("  {job}:\n    needs: {needs}\n    if: {MACOS_SIGN_IF}\n");
        let push_only = with_in(&text, &head, &head.replace(MACOS_SIGN_IF, SIGN_IF));
        assert_refused(
            &push_only,
            &format!("artifacts.yml: job {job} needs the exact `if: {MACOS_SIGN_IF}`"),
        );
    }
}
