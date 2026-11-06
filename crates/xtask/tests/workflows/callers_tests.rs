//! The rules of [`callers`](super::callers), on the workflows of this repository and on
//! [`HELLO`], a small `hello-desired.yml` that keeps every rule, each case adding one defect to
//! it and reading the exact problems the lint reports.

use super::callers::{CALLERS, HELLO_FILE, job_uses, names_set_desired};
use super::lint::{entries, entry, lint};
use super::yaml;
use super::{with_in, workflows};

/// A hello proof run that keeps every rule.
const HELLO: &str = "\
name: hello-desired
on:
  workflow_dispatch:
    inputs:
      version:
        type: string
        required: true
      variant:
        type: choice
        required: true
        options:
          - hello
          - fail-ready
permissions: {}
jobs:
  set-desired:
    if: ${{ github.ref == 'refs/heads/main' }}
    permissions:
      contents: read
      id-token: write
    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@0123456789abcdef0123456789abcdef01234567 # v0.1.0
    with:
      service: hello
      version: ${{ inputs.version }}
      hostkeeper_commit: 0123456789abcdef0123456789abcdef01234567
  digest:
    if: ${{ github.ref == 'refs/heads/main' }}
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    steps:
      - run: echo
";

/// The `uses:` line of [`HELLO`], line 21.
const USES: &str = "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@0123456789abcdef0123456789abcdef01234567 # v0.1.0\n";

/// The listed callers, as a refusal names them.
const ONLY: &str = "only [\"hello-desired.yml/set-desired\"] may";

fn hello_problems(text: &str) -> Vec<String> {
    lint(HELLO_FILE, text)
}

fn assert_hello(from: &str, to: &str, want: &[&str]) {
    let text = with_in(HELLO, from, to);
    assert_eq!(hello_problems(&text), want, "in:\n{text}");
}

#[test]
fn the_jobs_that_call_set_desired_are_exactly_the_listed_callers() {
    // Catches: a workflow of this repository that sets a desired version without being listed,
    // or a listed caller that no longer exists.
    let mut found = Vec::new();
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("every workflow parses");
        for (name, job) in entry(&doc, "jobs").map_or(&[][..], entries) {
            if job_uses(job).is_some_and(names_set_desired) {
                found.push(format!("{file}/{}", name.as_str().unwrap_or_default()));
            }
        }
    }
    let want: Vec<String> = CALLERS
        .iter()
        .map(|c| format!("{}/{}", c.file, c.job))
        .collect();
    assert_eq!(found, want);
}

#[test]
fn the_hello_proof_run_of_the_repository_is_a_dispatch_of_hello_or_fail_ready() {
    // Catches: hello-desired.yml deleted, or the real file out of its shape (the lint of every
    // workflow reads it too; this names it).
    let text = workflows()
        .into_iter()
        .find(|(file, _)| file == HELLO_FILE)
        .map(|(_, text)| text)
        .expect("hello-desired.yml exists");
    assert_eq!(hello_problems(&text), Vec::<String>::new());
}

#[test]
fn the_clean_hello_proof_run_passes() {
    assert_eq!(hello_problems(HELLO), Vec::<String>::new());
}

#[test]
fn a_job_of_another_workflow_that_calls_set_desired_is_refused() {
    // The same job in a file that is not listed: it is no caller, so the shared rules refuse its
    // token right and its missing runner too.
    assert_eq!(
        lint("other.yml", HELLO),
        [
            "job set-desired: `id-token: write` is not allowed in other.yml".to_owned(),
            "job set-desired: no `runs-on` (a job that calls a reusable workflow is not evaluated)"
                .to_owned(),
            format!("job set-desired calls set-desired.yml, and {ONLY}"),
        ]
    );
}

#[test]
fn a_renamed_caller_job_is_refused_and_the_listed_one_is_missing() {
    assert_hello(
        "  set-desired:\n    if:",
        "  set:\n    if:",
        &[
            "job set: `id-token: write` is not allowed in hello-desired.yml",
            "job set: no `runs-on` (a job that calls a reusable workflow is not evaluated)",
            &format!("job set calls set-desired.yml, and {ONLY}"),
            "hello-desired.yml: job set-desired does not call set-desired.yml",
            "hello-desired.yml: job set-desired must pass `service: hello`",
            "hello-desired.yml: job set-desired must pass the hostkeeper_commit its `uses:` is pinned to",
        ],
    );
}

#[test]
fn a_caller_of_another_reusable_workflow_is_refused() {
    // Catches: the listed job pointed at another workflow keeps no runner and no client rules.
    assert_hello(
        "/set-desired.yml@",
        "/other.yml@",
        &[
            "job set-desired: no `runs-on` (a job that calls a reusable workflow is not evaluated)",
            "hello-desired.yml: job set-desired does not call set-desired.yml",
        ],
    );
}

#[test]
fn a_set_desired_of_another_repository_is_refused() {
    assert_hello(
        "komira-ai/hostkeeper/.github",
        "someone/hostkeeper/.github",
        &[
            "job set-desired calls someone/hostkeeper/.github/workflows/set-desired.yml, not komira-ai/hostkeeper/.github/workflows/set-desired.yml",
        ],
    );
}

#[test]
fn a_local_set_desired_is_refused() {
    // A local path is code the lint does not pin; the caller rule names it too.
    assert_hello(
        USES,
        "    uses: ./.github/workflows/set-desired.yml\n",
        &[
            "line 21: `uses: ./.github/workflows/set-desired.yml` names a local path",
            "job set-desired calls ./.github/workflows/set-desired.yml, not komira-ai/hostkeeper/.github/workflows/set-desired.yml",
            "hello-desired.yml: job set-desired must pass the hostkeeper_commit its `uses:` is pinned to",
        ],
    );
}

#[test]
fn a_set_desired_pinned_to_a_branch_is_refused() {
    assert_hello(
        USES,
        "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@main # v0.1.0\n",
        &[
            "line 21: `uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@main` is not pinned to a 40-digit lowercase commit SHA",
            "hello-desired.yml: job set-desired must pass the hostkeeper_commit its `uses:` is pinned to",
        ],
    );
}

#[test]
fn a_hostkeeper_commit_other_than_the_pin_is_refused() {
    assert_hello(
        "      hostkeeper_commit: 0123456789abcdef0123456789abcdef01234567\n",
        "      hostkeeper_commit: 1123456789abcdef0123456789abcdef01234567\n",
        &[
            "hello-desired.yml: job set-desired must pass the hostkeeper_commit its `uses:` is pinned to",
        ],
    );
}

#[test]
fn a_caller_without_the_main_gate_is_refused() {
    let want = [
        "job set-desired needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`",
        "hello-desired.yml: job set-desired needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`",
    ];
    assert_hello(
        "  set-desired:\n    if: ${{ github.ref == 'refs/heads/main' }}\n",
        "  set-desired:\n",
        &want,
    );
    assert_hello(
        "  set-desired:\n    if: ${{ github.ref == 'refs/heads/main' }}\n",
        "  set-desired:\n    if: ${{ github.ref == 'refs/heads/main' || true }}\n",
        &want,
    );
}

#[test]
fn a_job_of_the_hello_proof_run_without_the_main_gate_is_refused() {
    // Catches: the digest job run from another branch, where it would read inputs unchecked.
    assert_hello(
        "  digest:\n    if: ${{ github.ref == 'refs/heads/main' }}\n",
        "  digest:\n",
        &[
            "hello-desired.yml: job digest needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`",
        ],
    );
}

#[test]
fn set_desired_is_named_by_its_file_whatever_the_owner_or_ref() {
    for (uses, calls) in [
        (
            "komira-ai/hostkeeper/.github/workflows/set-desired.yml@0123",
            true,
        ),
        ("someone/fork/.github/workflows/set-desired.yml@main", true),
        ("./.github/workflows/set-desired.yml", true),
        (
            "komira-ai/hostkeeper/.github/workflows/set-desired.yml",
            true,
        ),
        (
            "komira-ai/hostkeeper/.github/workflows/set-desired.yaml@0123",
            false,
        ),
        (
            "komira-ai/hostkeeper/.github/workflows/not-set-desired.yml@0123",
            false,
        ),
        (
            "komira-ai/hostkeeper/.github/workflows/other.yml@set-desired-yml",
            false,
        ),
        ("set-desired.yml", true),
        ("", false),
    ] {
        assert_eq!(names_set_desired(uses), calls, "{uses:?}");
    }
}

#[test]
fn a_caller_with_other_permissions_is_refused() {
    let want = "job set-desired: permissions must be exactly contents: read and id-token: write";
    // A further read scope.
    assert_hello(
        "      contents: read\n      id-token: write\n",
        "      contents: read\n      actions: read\n      id-token: write\n",
        &[want],
    );
    // No token right: the client could get no token.
    assert_hello("      id-token: write\n", "", &[want]);
    // No permissions at all: the job would take the workflow's, which are none.
    assert_hello(
        "    permissions:\n      contents: read\n      id-token: write\n",
        "",
        &[want],
    );
}

#[test]
fn a_caller_that_passes_secrets_is_refused() {
    assert_hello(
        "    with:\n",
        "    secrets:\n      token: ${{ secrets.TOKEN }}\n    with:\n",
        &["job set-desired passes secrets to set-desired.yml"],
    );
    // `inherit` is refused by the shared rule as well.
    assert_hello(
        "    with:\n",
        "    secrets: inherit\n    with:\n",
        &[
            "line 22: `secrets: inherit` hands every secret of the caller on",
            "job set-desired passes secrets to set-desired.yml",
        ],
    );
}

#[test]
fn a_hello_proof_run_on_another_trigger_is_refused() {
    let want = "hello-desired.yml: on must be workflow_dispatch only";
    assert_hello("\non:\n", "\non:\n  push:\n    branches: [main]\n", &[want]);
    // No trigger: no inputs either.
    let text = with_in(
        HELLO,
        "\non:\n  workflow_dispatch:\n",
        "\non:\n  schedule:\n    - cron: '0 0 * * *'\n  workflow_call:\n",
    );
    assert_eq!(
        hello_problems(&text),
        [
            want,
            "hello-desired.yml: input version must be a required string",
            "hello-desired.yml: input variant must be a required choice of exactly [\"hello\", \"fail-ready\"]",
        ]
    );
}

#[test]
fn a_version_input_that_is_not_a_required_string_is_refused() {
    let want = &["hello-desired.yml: input version must be a required string"];
    assert_hello(
        "      version:\n        type: string\n        required: true\n",
        "      version:\n        type: string\n        required: false\n",
        want,
    );
    assert_hello(
        "      version:\n        type: string\n",
        "      version:\n        type: number\n",
        want,
    );
    assert_hello(
        "      version:\n        type: string\n        required: true\n",
        "",
        want,
    );
}

#[test]
fn a_variant_input_other_than_the_choice_of_hello_and_fail_ready_is_refused() {
    let want = &[
        "hello-desired.yml: input variant must be a required choice of exactly [\"hello\", \"fail-ready\"]",
    ];
    // A third value.
    assert_hello(
        "          - fail-ready\n",
        "          - fail-ready\n          - other\n",
        want,
    );
    // One value missing.
    assert_hello("          - fail-ready\n", "", want);
    // The two in the other order: the first is the default a dispatch shows.
    assert_hello(
        "          - hello\n          - fail-ready\n",
        "          - fail-ready\n          - hello\n",
        want,
    );
    // Free text instead of a choice.
    assert_hello("        type: choice\n", "        type: string\n", want);
    // Not required.
    assert_hello(
        "        type: choice\n        required: true\n",
        "        type: choice\n        required: false\n",
        want,
    );
    // Options that are not a list.
    assert_hello(
        "        options:\n          - hello\n          - fail-ready\n",
        "        options: hello\n",
        want,
    );
}

#[test]
fn a_hello_proof_run_for_another_service_is_refused() {
    let want = &["hello-desired.yml: job set-desired must pass `service: hello`"];
    assert_hello("      service: hello\n", "      service: tierstore\n", want);
    assert_hello("      service: hello\n", "", want);
}
