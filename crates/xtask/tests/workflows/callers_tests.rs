//! The rules of [`callers`](super::callers), on the workflows of this repository and on
//! [`hello`], a small `hello-desired.yml` that keeps every rule, each case adding one defect to
//! it and reading the exact problems the lint reports.

use super::callers::{
    CALLERS, CLIENT_COMMIT, CLIENT_RELEASE, HELLO_FILE, TAILNET_TAG, job_uses, names_set_desired,
};
use super::lint::{entries, entry, lint};
use super::yaml;
use super::{with_in, workflows};

/// A hello proof run that keeps every rule, with `<commit>` and `<release>` for the CI client's
/// revision ([`hello`] writes it in).
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
    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@<commit> # v<release>
    with:
      service: hello
      version: ${{ needs.release.outputs.version }}
      targets: ${{ needs.release.outputs.targets }}
      hostkeeper_commit: <commit>
      hostkeeper_version: <release>
      join_tailnet: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}
      tailnet_client_id: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}
      tailnet_audience: ${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}
      tailnet_coordinator: ${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}
      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG }}
  release:
    if: ${{ github.ref == 'refs/heads/main' }}
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    outputs:
      version: ${{ steps.version.outputs.version }}
      targets: ${{ steps.targets.outputs.targets }}
    steps:
      - id: targets
        run: cargo xtask release targets --service hello --version 1.2.3 --dir d --repo o/r
      - id: version
        run: echo
";

/// The CI client's revision written into `text` for `<commit>` and `<release>`.
fn at_client(text: &str) -> String {
    text.replace("<commit>", CLIENT_COMMIT)
        .replace("<release>", CLIENT_RELEASE)
}

/// [`HELLO`] at the CI client's revision.
fn hello() -> String {
    at_client(HELLO)
}

/// The `uses:` line of [`hello`], line 21.
fn uses() -> String {
    at_client(
        "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@<commit> # v<release>\n",
    )
}

/// The pin of the CI client before it ran hkctl, as hello-desired.yml had it: release v0.1.35.
const OLD_COMMIT: &str = "f04696da7e2bdcbaf9112450ab651a1ab24e61bf";

/// The listed callers, as a refusal names them.
const ONLY: &str = "only [\"hello-desired.yml/set-desired\", \"release.yml/set-desired\"] may";

fn hello_problems(text: &str) -> Vec<String> {
    lint(HELLO_FILE, text)
}

#[track_caller]
fn assert_hello(from: &str, to: &str, want: &[&str]) {
    let text = with_in(&hello(), from, to);
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
fn the_hello_proof_run_of_the_repository_is_a_dispatch_of_one_release() {
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
    assert_eq!(hello_problems(&hello()), Vec::<String>::new());
}

#[test]
fn a_job_of_another_workflow_that_calls_set_desired_is_refused() {
    // The same job in a file that is not listed: it is no caller, so the shared rules refuse its
    // token right and its missing runner too.
    assert_eq!(
        lint("other.yml", &hello()),
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
            "hello-desired.yml: job set-desired must pass `version: ${{ needs.release.outputs.version }}`",
            "hello-desired.yml: job set-desired must pass `targets: ${{ needs.release.outputs.targets }}`",
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
        &uses(),
        "    uses: ./.github/workflows/set-desired.yml\n",
        &[
            "line 21: `uses: ./.github/workflows/set-desired.yml` names a local path",
            "job set-desired calls ./.github/workflows/set-desired.yml, not komira-ai/hostkeeper/.github/workflows/set-desired.yml",
            &format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `./.github/workflows/set-desired.yml`"
            ),
            &format!(
                "job set-desired must name the CI client's release `# v{CLIENT_RELEASE}` after its `uses:`, not `# `"
            ),
        ],
    );
}

#[test]
fn a_set_desired_pinned_to_a_branch_is_refused() {
    assert_hello(
        &uses(),
        &at_client(
            "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@main # v<release>\n",
        ),
        &[
            "line 21: `uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@main` is not pinned to a 40-digit lowercase commit SHA",
            &format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `komira-ai/hostkeeper/.github/workflows/set-desired.yml@main`"
            ),
        ],
    );
}

#[test]
fn a_hostkeeper_commit_other_than_the_pin_is_refused() {
    // Catches: hkctl's attestation checked against another commit than the `set-desired.yml` the
    // job runs, so that the client downloaded is not the one that revision was written for.
    let want = format!(
        "job set-desired must pass `hostkeeper_commit: {CLIENT_COMMIT}`, the CI client's revision"
    );
    assert_hello(
        &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
        &format!("      hostkeeper_commit: {OLD_COMMIT}\n"),
        &[&want],
    );
    assert_hello(
        &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
        "",
        &[&want],
    );
}

#[test]
fn the_callers_of_the_repository_run_the_ci_clients_revision() {
    // Catches: release.yml or hello-desired.yml left on (or moved back to) a revision of
    // set-desired.yml other than the one the lint names, the CI client on hkctl.
    let pin = format!("set-desired.yml@{CLIENT_COMMIT} # v{CLIENT_RELEASE}\n");
    let version = format!("      hostkeeper_version: {CLIENT_RELEASE}\n");
    let commit = format!("      hostkeeper_commit: {CLIENT_COMMIT}\n");
    for c in CALLERS {
        let text = workflows()
            .into_iter()
            .find(|(file, _)| file == c.file)
            .map(|(_, text)| text)
            .expect("every caller's file exists");
        assert_eq!(lint(c.file, &text), Vec::<String>::new(), "{}", c.file);
        for want in [&pin, &version, &commit] {
            assert_eq!(text.matches(want.as_str()).count(), 1, "{}: {want}", c.file);
        }
    }
}

#[test]
fn the_ci_clients_revision_is_a_full_commit_and_a_release() {
    // Catches: a revision written as a short commit or a tag, which the pin rule refuses in every
    // caller, or a release that is not X.Y.Z, which no `hkctl-<version>` asset is named by.
    assert_eq!(CLIENT_COMMIT.len(), 40);
    assert!(
        CLIENT_COMMIT
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    let parts: Vec<&str> = CLIENT_RELEASE.split('.').collect();
    assert_eq!(parts.len(), 3, "{CLIENT_RELEASE}");
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    );
}

#[test]
fn a_caller_pinned_to_the_ci_client_before_hkctl_is_refused() {
    // Catches: a caller left on, or moved back to, the revision of v0.1.35 that runs
    // `hostkeeper ci`, which the deletion of that verb leaves to old releases only.
    let text = with_in(
        &with_in(
            &with_in(
                &hello(),
                &uses(),
                &format!(
                    "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{OLD_COMMIT} # v0.1.35\n"
                ),
            ),
            &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
            &format!("      hostkeeper_commit: {OLD_COMMIT}\n"),
        ),
        &format!("      hostkeeper_version: {CLIENT_RELEASE}\n"),
        "      hostkeeper_version: 0.1.35\n",
    );
    assert_eq!(
        hello_problems(&text),
        [
            format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `komira-ai/hostkeeper/.github/workflows/set-desired.yml@{OLD_COMMIT}`"
            ),
            format!(
                "job set-desired must name the CI client's release `# v{CLIENT_RELEASE}` after its `uses:`, not `# v0.1.35`"
            ),
            format!(
                "job set-desired must pass `hostkeeper_version: {CLIENT_RELEASE}`, the CI client's revision"
            ),
            format!(
                "job set-desired must pass `hostkeeper_commit: {CLIENT_COMMIT}`, the CI client's revision"
            ),
        ]
    );
}

/// The pin of the CI client before it took `tailnet_tag`, as both callers had it: release
/// v0.1.173.
const PRE_TAG_COMMIT: &str = "4859b4800897f2468c0f2131455721edfc537217";

#[test]
fn a_caller_pinned_to_the_ci_client_before_the_tailnet_tag_is_refused() {
    // Catches: a caller left on, or moved back to, v0.1.173, whose set-desired.yml declares no
    // input `tailnet_tag`: GitHub refuses the call of a workflow with an input it does not
    // declare, so the whole caller workflow fails to start.
    let text = with_in(
        &with_in(
            &with_in(
                &hello(),
                &uses(),
                &format!(
                    "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{PRE_TAG_COMMIT} # v0.1.173\n"
                ),
            ),
            &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
            &format!("      hostkeeper_commit: {PRE_TAG_COMMIT}\n"),
        ),
        &format!("      hostkeeper_version: {CLIENT_RELEASE}\n"),
        "      hostkeeper_version: 0.1.173\n",
    );
    assert_eq!(
        hello_problems(&text),
        [
            format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `komira-ai/hostkeeper/.github/workflows/set-desired.yml@{PRE_TAG_COMMIT}`"
            ),
            format!(
                "job set-desired must name the CI client's release `# v{CLIENT_RELEASE}` after its `uses:`, not `# v0.1.173`"
            ),
            format!(
                "job set-desired must pass `hostkeeper_version: {CLIENT_RELEASE}`, the CI client's revision"
            ),
            format!(
                "job set-desired must pass `hostkeeper_commit: {CLIENT_COMMIT}`, the CI client's revision"
            ),
        ]
    );
}

#[test]
fn a_caller_whose_pin_alone_is_another_commit_is_refused() {
    // Catches: the `uses:` line moved without its inputs, so that the revision of set-desired.yml
    // the job runs downloads an hkctl it was not written for.
    assert_hello(
        &uses(),
        &format!(
            "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{OLD_COMMIT} # v{CLIENT_RELEASE}\n"
        ),
        &[&format!(
            "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `komira-ai/hostkeeper/.github/workflows/set-desired.yml@{OLD_COMMIT}`"
        )],
    );
}

#[test]
fn a_version_comment_other_than_the_client_release_is_refused() {
    // Catches: a `uses:` line whose comment names another release than the commit is, so that a
    // reader of the pin is told the wrong release; the comment is compared as a whole word.
    let comment = |c: &str| {
        format!(
            "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{CLIENT_COMMIT} {c}\n"
        )
    };
    let unversioned = format!(
        "line 21: `uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{CLIENT_COMMIT}` has no version comment (`# v1.2.3`) on its line"
    );
    // Each comment, and whether the shared rule refuses it too, as no version at all.
    for (got, no_version) in [
        "v0.1.35".to_owned(),
        format!("v{CLIENT_RELEASE}0"),
        CLIENT_RELEASE.to_owned(),
        "v1".to_owned(),
    ]
    .into_iter()
    .zip([false, false, true, false])
    {
        let refused = format!(
            "job set-desired must name the CI client's release `# v{CLIENT_RELEASE}` after its `uses:`, not `# {got}`"
        );
        let mut want = Vec::new();
        if no_version {
            want.push(unversioned.as_str());
        }
        want.push(refused.as_str());
        assert_hello(&uses(), &comment(&format!("# {got}")), &want);
    }
    // Words after the release are the author's: the release is the comment's first word.
    let text = with_in(
        &hello(),
        &uses(),
        &comment(&format!("# v{CLIENT_RELEASE} (the CI client on hkctl)")),
    );
    assert_eq!(hello_problems(&text), Vec::<String>::new());
    // Quoted, the comment is read past the closing quote.
    let quoted = format!(
        "    uses: 'komira-ai/hostkeeper/.github/workflows/set-desired.yml@{CLIENT_COMMIT}' # v{CLIENT_RELEASE}\n"
    );
    let text = with_in(&hello(), &uses(), &quoted);
    assert_eq!(hello_problems(&text), Vec::<String>::new());
}

#[test]
fn a_hostkeeper_version_other_than_the_client_release_is_refused() {
    // Catches: hkctl downloaded from another release than the revision's, whose `--version`
    // the job then refuses, or which runs a client the revision was not written for.
    let want = format!(
        "job set-desired must pass `hostkeeper_version: {CLIENT_RELEASE}`, the CI client's revision"
    );
    let from = format!("      hostkeeper_version: {CLIENT_RELEASE}\n");
    for to in [
        "      hostkeeper_version: 0.1.35\n",
        &format!("      hostkeeper_version: v{CLIENT_RELEASE}\n"),
        "",
    ] {
        assert_hello(&from, to, &[&want]);
    }
}

/// The `tailnet_tag` line of a caller that passes the repository variable.
const TAG_LINE: &str = "      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n";

/// The refusal of a caller that does not pass [`TAILNET_TAG`] as `tailnet_tag`.
fn no_tag() -> String {
    format!(
        "job set-desired must pass `tailnet_tag: {TAILNET_TAG}`, the tag the tailnet's trust credential issues"
    )
}

#[test]
fn a_caller_that_joins_the_tailnet_without_the_tailnet_tag_is_refused() {
    // Catches: a caller that joins with the CI client's default tag, `tag:hk-ci`, instead of the
    // one the federation's trust credential issues, which Tailscale refuses at the join.
    // The proof run joins the tailnet when the client ID is set, as release.yml's call does.
    assert!(
        hello().contains("      join_tailnet: "),
        "the proof run joins"
    );
    assert_hello(TAG_LINE, "", &[&no_tag()]);
    // The repository's own caller that joins, release.yml, with its `tailnet_tag` taken out.
    let release = workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists");
    assert!(
        release.contains("      join_tailnet: "),
        "release.yml joins"
    );
    let text = with_in(&release, TAG_LINE, "");
    assert_eq!(lint("release.yml", &text), [no_tag()]);
}

#[test]
fn a_caller_that_passes_another_tailnet_tag_is_refused() {
    // Catches: a tag written into the caller, a fallback to another tag when the variable is
    // empty, another variable, or no tag at all on a caller that does not join (the CI client
    // checks the tag whether or not it joins).
    for to in [
        "",
        "      tailnet_tag: tag:ci\n",
        "      tailnet_tag: tag:hk-ci\n",
        "      tailnet_tag: ''\n",
        "      tailnet_tag:\n",
        "      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG || 'tag:hk-ci' }}\n",
        "      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG || 'tag:ci' }}\n",
        "      tailnet_tag: ${{ vars.TAILNET_TAG }}\n",
        "      tailnet_tag: ${{ inputs.tailnet_tag }}\n",
        "      tailnet_tag: ${{vars.HOSTKEEPER_TAILNET_TAG}}\n",
        "      tailnet_tag:\n        - ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n",
        "      tailnet-tag: ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n",
    ] {
        assert_hello(TAG_LINE, to, &[&no_tag()]);
    }
    // Quoted, it is the same value.
    assert_hello(
        TAG_LINE,
        "      tailnet_tag: \"${{ vars.HOSTKEEPER_TAILNET_TAG }}\"\n",
        &[],
    );
}

#[test]
fn the_callers_of_the_repository_pass_the_tailnet_tag_variable() {
    // Catches: release.yml or hello-desired.yml that names its tag some other way than the one
    // repository variable the deploy docs list.
    assert_eq!(TAILNET_TAG, "${{ vars.HOSTKEEPER_TAILNET_TAG }}");
    for c in CALLERS {
        let text = workflows()
            .into_iter()
            .find(|(file, _)| file == c.file)
            .map(|(_, text)| text)
            .expect("every caller's file exists");
        assert_eq!(text.matches(TAG_LINE).count(), 1, "{}", c.file);
    }
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
    // Catches: the release job run from another branch, where it would read inputs unchecked.
    assert_hello(
        "  release:\n    if: ${{ github.ref == 'refs/heads/main' }}\n",
        "  release:\n",
        &[
            "hello-desired.yml: job release needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`",
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
        &hello(),
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
    // No such input.
    assert_hello(
        "      variant:\n        type: choice\n        required: true\n        options:\n          - hello\n          - fail-ready\n",
        "",
        want,
    );
}

#[test]
fn a_proof_run_that_offers_another_input_beside_version_and_variant_is_refused() {
    // Catches: an input beside the release and the variant of the choice (a dry run, a package
    // digest) that a run of the proof would have to honour. The failing build is offered as the
    // `variant` choice, so a `variant` is allowed; before, an input of that name was refused.
    assert_hello(
        "          - fail-ready\npermissions: {}\n",
        "          - fail-ready\n      dry_run:\n        type: boolean\npermissions: {}\n",
        &[
            "hello-desired.yml: input dry_run is not one of [\"version\"] or variant; a proof run sets one release's package",
        ],
    );
}

#[test]
fn a_hello_proof_run_for_another_service_is_refused() {
    let want = &["hello-desired.yml: job set-desired must pass `service: hello`"];
    assert_hello("      service: hello\n", "      service: tierstore\n", want);
    assert_hello("      service: hello\n", "", want);
}

#[test]
fn a_hello_proof_run_that_does_not_set_the_release_jobs_targets_is_refused() {
    // Catches: hello set without targets, or with a value of each input that no step of the
    // release job checked (the dispatch's own input, another job's output).
    for input in ["version", "targets"] {
        let line = format!("      {input}: ${{{{ needs.release.outputs.{input} }}}}\n");
        let want = format!(
            "hello-desired.yml: job set-desired must pass `{input}: ${{{{ needs.release.outputs.{input} }}}}`"
        );
        assert_hello(&line, "", &[&want]);
        assert_hello(
            &line,
            &format!("      {input}: ${{{{ inputs.{input} }}}}\n"),
            &[&want],
        );
    }
}

#[test]
fn a_hello_proof_run_that_passes_an_index_is_refused() {
    // Catches: an index or its digest given beside hello's native targets, which the
    // coordinator would hold the targets to (an index of archives they no longer list).
    for input in ["release_index", "release_index_digest"] {
        assert_hello(
            "      service: hello\n",
            &format!(
                "      service: hello\n      {input}: ${{{{ needs.release.outputs.{input} }}}}\n"
            ),
            &[&format!(
                "hello-desired.yml: job set-desired passes {input}; a release is set with native entries alone, and no index"
            )],
        );
    }
}

#[test]
fn a_hello_proof_run_that_sets_one_package_digest_is_refused() {
    // Catches: hello set by one platform's package digest beside the targets (the client refuses
    // the pair) or instead of them (every host ordered one platform's package).
    assert_hello(
        "      service: hello\n",
        "      service: hello\n      package_digest: ${{ needs.release.outputs.package_digest }}\n",
        &[
            "hello-desired.yml: job set-desired passes package_digest; the targets name each platform's package",
        ],
    );
}

#[test]
fn a_hello_release_job_whose_outputs_are_not_its_steps_is_refused() {
    // Catches: the targets passed on from a value the verb did not write, an output dropped, and
    // an index written out again.
    let want = |got: &str| {
        format!(
            "hello-desired.yml: job release outputs are {got}, not [(\"targets\", \"${{{{ steps.targets.outputs.targets }}}}\"), (\"version\", \"${{{{ steps.version.outputs.version }}}}\")]"
        )
    };
    assert_hello(
        "      targets: ${{ steps.targets.outputs.targets }}\n",
        "      targets: ${{ inputs.targets }}\n",
        &[&want(
            "[(\"targets\", \"${{ inputs.targets }}\"), (\"version\", \"${{ steps.version.outputs.version }}\")]",
        )],
    );
    assert_hello(
        "      version: ${{ steps.version.outputs.version }}\n",
        "",
        &[&want(
            "[(\"targets\", \"${{ steps.targets.outputs.targets }}\")]",
        )],
    );
    assert_hello(
        "      version: ${{ steps.version.outputs.version }}\n",
        "      version: ${{ steps.version.outputs.version }}\n      release_index: ${{ steps.index.outputs.release_index }}\n",
        &[&want(
            "[(\"release_index\", \"${{ steps.index.outputs.release_index }}\"), (\"targets\", \"${{ steps.targets.outputs.targets }}\"), (\"version\", \"${{ steps.version.outputs.version }}\")]",
        )],
    );
}

#[test]
fn a_hello_release_job_whose_targets_are_not_written_by_the_verb_for_hello_is_refused() {
    // Catches: targets echoed from text instead of written by the verb that checks the release's
    // files, the verb run for hostkeeper's package instead of hello's, and the step renamed away
    // from the output that reads it.
    let want = "hello-desired.yml: job release needs a step `targets` that runs `cargo xtask release targets --service hello ...`";
    let run = "        run: cargo xtask release targets --service hello --version 1.2.3 --dir d --repo o/r\n";
    for to in [
        "        run: echo 'targets={}' >> \"$GITHUB_OUTPUT\"\n",
        "        run: cargo xtask release targets --version 1.2.3 --dir d --repo o/r\n",
    ] {
        assert_hello(run, to, &[want]);
    }
    assert_hello("      - id: targets\n", "      - id: entries\n", &[want]);
}
