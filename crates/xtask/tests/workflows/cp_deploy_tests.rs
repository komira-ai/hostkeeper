//! The coordinator's deploy to the control plane in the repository's `release.yml`
//! ([`super::cp_deploy`]): the committed jobs pass, their `if:` is evaluated against every state
//! of the repository variables and of the release, and the lint is shown to refuse each defect
//! put into either job, each by its exact problems.

use std::collections::BTreeMap;

use super::cp_deploy::{
    DEPLOY_IF, DEPLOY_WORKFLOW, DIGEST_JOB, DIGEST_OUTPUTS, DIGEST_STEPS, JOB, WITH, calls_updater,
    problems,
};
use super::desired::job;
use super::expr::{Context, holds};
use super::lint::lint;
use super::release_ref::FLOOR;
use super::{with_in, workflows, yaml};

fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// The text of the job `name` of `text`, from its key line to the next line indented by two
/// spaces (the next job, or the comment above it).
fn job_text<'a>(text: &'a str, name: &str) -> &'a str {
    let key = format!("\n  {name}:\n");
    let start = text.find(&key).unwrap_or_else(|| panic!("job {name}")) + 1;
    let body = start + key.len() - 1;
    let end = text[body..]
        .match_indices('\n')
        .map(|(at, _)| body + at + 1)
        .find(|&at| text[at..].starts_with("  ") && !text[at..].starts_with("   "))
        .unwrap_or(text.len());
    &text[start..end]
}

/// `release.yml` with `from` replaced by `to` inside the job `name` alone.
fn in_job(name: &str, from: &str, to: &str) -> String {
    let text = release_yml();
    let old = job_text(&text, name);
    let new = with_in(old, from, to);
    with_in(&text, old, &new)
}

fn release_problems(text: &str) -> Vec<String> {
    lint("release.yml", text)
}

#[test]
fn the_control_plane_deploy_jobs_of_the_repository_pass() {
    // Catches: the committed jobs out of the shape the lint holds them to, or the caller not
    // recognised as the one call of the updater's workflow.
    let text = release_yml();
    let doc = yaml::load(&text).expect("release.yml parses");
    assert_eq!(problems(&doc), Vec::<String>::new());
    let caller = job(&doc, JOB).expect("the caller job");
    assert!(calls_updater("release.yml", JOB, caller));
    assert!(!calls_updater("hello-desired.yml", JOB, caller));
    assert!(!calls_updater("release.yml", DIGEST_JOB, caller));
    assert_eq!(DIGEST_STEPS.len(), 6);
    assert_eq!(DIGEST_OUTPUTS.len(), 3);
    assert_eq!(WITH.len(), 12);
}

/// Whether [`DEPLOY_IF`] holds for a run whose plan says `release`, on `git_ref`, with the
/// repository variables `endpoint` and `tags`.
fn runs(release: &str, git_ref: &str, endpoint: &str, tags: &str) -> bool {
    let names: BTreeMap<String, String> = [
        ("needs.plan.outputs.release", release),
        ("github.ref", git_ref),
        ("vars.CP_DEPLOY_ENDPOINT", endpoint),
        ("vars.CP_DEPLOY_TAILSCALE_TAGS", tags),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let context = Context {
        names,
        cancelled: false,
        failed: false,
    };
    holds(DEPLOY_IF, &context).expect("the condition evaluates")
}

#[test]
fn the_deploy_runs_only_for_a_release_of_main_while_both_repository_variables_are_set() {
    // Catches: the deploy on while the endpoint (or the tailnet tags) is still unset, which
    // would fail every release's run before the variables exist; a deploy of a dispatch from
    // another branch, whose token the updater's caller rule refuses; or a deploy of a run that
    // released nothing.
    let main = "refs/heads/main";
    let endpoint = "http://cp.example:8080";
    assert!(runs("true", main, endpoint, "tag:cp-deploy"));
    assert!(!runs("true", main, "", ""));
    assert!(!runs("true", main, "", "tag:cp-deploy"));
    assert!(!runs("true", main, endpoint, ""));
    assert!(!runs("false", main, endpoint, "tag:cp-deploy"));
    assert!(!runs("", main, endpoint, "tag:cp-deploy"));
    assert!(!runs("true", "refs/heads/other", endpoint, "tag:cp-deploy"));
}

#[test]
fn a_deploy_job_under_another_if_is_refused() {
    // Catches: either job without the variable gate, so it runs before the variables are set;
    // and the OIDC token held under a wider condition than the deploy's.
    let gate = " && vars.CP_DEPLOY_ENDPOINT != ''";
    assert_eq!(
        release_problems(&in_job(JOB, gate, "")),
        [
            format!("job cp-deploy: `id-token: write` needs the exact `if: {DEPLOY_IF}`"),
            format!("release.yml: job cp-deploy needs the exact `if: {DEPLOY_IF}`"),
        ]
    );
    assert_eq!(
        release_problems(&in_job(DIGEST_JOB, gate, "")),
        [format!(
            "release.yml: job cp-deploy-digest needs the exact `if: {DEPLOY_IF}`"
        )]
    );
}

#[test]
fn a_deploy_job_with_another_token_right_is_refused() {
    // Catches: the caller handed a scope that writes to the repository beside the OIDC token,
    // the digest job asking for an OIDC token, or the caller without one (the deploy workflow
    // asks GitHub for a token for every request).
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "      id-token: write\n",
            "      id-token: write\n      contents: write\n"
        )),
        [
            "job cp-deploy: `contents: write` is not allowed in release.yml",
            "release.yml: job cp-deploy must hold exactly `id-token: write`",
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "      id-token: write\n",
            "      id-token: read\n"
        )),
        ["release.yml: job cp-deploy must hold exactly `id-token: write`"]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "      contents: read\n",
            "      contents: read\n      id-token: write\n"
        )),
        [
            "job cp-deploy-digest: `id-token: write` is not allowed in release.yml",
            "release.yml: job cp-deploy-digest must hold exactly `contents: read`",
        ]
    );
}

#[test]
fn a_deploy_of_another_pin_service_or_inputs_is_refused() {
    // Catches: the updater's workflow at a commit its allow-list does not name (or another
    // repository's copy of it); the deploy of another service, of no wait, or of a signer the
    // caller picks; and an input dropped, such as the tailnet tags.
    let pin = DEPLOY_WORKFLOW
        .split_once('@')
        .map(|(_, at)| at)
        .expect("a pin");
    let other = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        release_problems(&in_job(JOB, pin, other)),
        [
            "job cp-deploy: no `runs-on` (a job that calls a reusable workflow is not evaluated)"
                .to_owned(),
            format!(
                "release.yml: job cp-deploy must call `{DEPLOY_WORKFLOW}`, not `{}`",
                DEPLOY_WORKFLOW.replace(pin, other)
            ),
        ]
    );
    let with = |pairs: &[(&str, &str)]| {
        let mut all: Vec<(&str, &str)> = pairs.to_vec();
        all.sort_unstable();
        format!("release.yml: job cp-deploy passes {all:?}, not {WITH:?}")
    };
    fn replaced<'a>(key: &str, value: &'a str) -> Vec<(&'a str, &'a str)> {
        WITH.iter()
            .map(|&(k, v)| (k, if k == key { value } else { v }))
            .collect()
    }
    for (key, from, to) in [
        (
            "service",
            "service: hostkeeper-coordinator\n",
            "service: hostkeeper\n",
        ),
        ("wait-minutes", "wait-minutes: 60\n", "wait-minutes: 0\n"),
        (
            "environment",
            "environment: controlplane\n",
            "environment: release\n",
        ),
    ] {
        let value = to.split_once(": ").map(|(_, v)| v.trim()).expect("a value");
        assert_eq!(
            release_problems(&in_job(JOB, from, to)),
            [with(&replaced(key, value))],
            "{key}"
        );
    }
    let mut signer = WITH.to_vec();
    signer.push(("signer-repository", "komira-ai/other"));
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "      service: hostkeeper-coordinator\n",
            "      service: hostkeeper-coordinator\n      signer-repository: komira-ai/other\n"
        )),
        [with(&signer)]
    );
    let tags: Vec<(&str, &str)> = WITH
        .iter()
        .copied()
        .filter(|(k, _)| *k != "tailscale-tags")
        .collect();
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "      tailscale-tags: ${{ vars.CP_DEPLOY_TAILSCALE_TAGS }}\n",
            ""
        )),
        [with(&tags)]
    );
}

#[test]
fn a_deploy_job_that_passes_secrets_or_goes_on_after_a_failure_is_refused() {
    // Catches: a secret handed to a workflow of another repository (the OIDC token is the one
    // credential), a failed deploy reported green, or a job-level key that changes where or
    // whether it runs.
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "    permissions:\n",
            "    secrets:\n      token: ${{ secrets.DEPLOY_TOKEN }}\n    permissions:\n"
        )),
        [
            "release.yml: job cp-deploy has `secrets`, which it may not (only [\"if\", \"needs\", \"permissions\", \"uses\", \"with\"])",
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "    permissions:\n",
            "    continue-on-error: true\n    permissions:\n"
        )),
        [
            "release.yml: job cp-deploy has `continue-on-error`, which it may not (only [\"if\", \"needs\", \"permissions\", \"uses\", \"with\"])",
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "    permissions:\n",
            "    environment: release\n    permissions:\n"
        )),
        [
            "release.yml: job cp-deploy has `environment`, which it may not (only [\"if\", \"needs\", \"permissions\", \"uses\", \"with\"])",
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "    permissions:\n",
            "    continue-on-error: true\n    permissions:\n"
        )),
        [
            "release.yml: job cp-deploy-digest has `continue-on-error`, which it may not (only [\"if\", \"needs\", \"outputs\", \"permissions\", \"runs-on\", \"steps\", \"timeout-minutes\"])",
        ]
    );
}

#[test]
fn a_deploy_that_does_not_wait_for_the_newest_release_and_its_digest_is_refused() {
    // Catches: a deploy started before every publish and image of the run (`newest` needs
    // them), or before the digest it is given was read.
    assert_eq!(
        release_problems(&in_job(
            JOB,
            "    needs: [plan, newest, cp-deploy-digest]\n",
            "    needs: [plan, cp-deploy-digest]\n"
        )),
        [
            "release.yml: job cp-deploy needs [\"cp-deploy-digest\", \"plan\"], not [\"cp-deploy-digest\", \"newest\", \"plan\"]",
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "    needs: [plan, newest]\n",
            "    needs: [plan, publish]\n"
        )),
        [
            "release.yml: job cp-deploy-digest needs [\"plan\", \"publish\"], not [\"newest\", \"plan\"]",
        ]
    );
}

#[test]
fn a_digest_job_whose_steps_runner_or_outputs_differ_is_refused() {
    // Catches: the image digest read from files not checked against SHA256SUMS once more, from
    // the release's own commit's xtask (which may predate the verb), under a step `if:` that
    // could skip it; the verb's step under another id than the one the outputs read (they would
    // be empty); an output that is not the verb's line; and a runner or timeout of its own.
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "      - name: Check the files against SHA256SUMS\n        working-directory: dist\n        run: sha256sum --check --strict SHA256SUMS\n",
            ""
        )),
        [
            "release.yml: job cp-deploy-digest has 5 steps, not the 6 of cp_deploy::DIGEST_STEPS",
            &format!(
                "release.yml: job cp-deploy-digest step 5 is `{}`, not `{}`",
                DIGEST_STEPS[5], DIGEST_STEPS[4]
            ),
        ]
    );
    // The checkout gains a line above the step that runs the verb.
    let digest_line = release_yml()
        .lines()
        .position(|l| l.contains("run: cargo xtask release image-digest "))
        .expect("the step that runs the verb")
        + 1;
    let floor = FLOOR.0;
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "        with:\n          persist-credentials: false\n",
            "        with:\n          persist-credentials: false\n          ref: ${{ needs.newest.outputs.commit }}\n"
        )),
        [
            "release.yml: job cp-deploy-digest step 1 is `uses actions/checkout with persist-credentials=false with ref=${{ needs.newest.outputs.commit }}`, not `uses actions/checkout with persist-credentials=false`",
            &format!(
                "release.yml: job cp-deploy-digest step 6 (line {}) runs the xtask of `${{{{ needs.newest.outputs.commit }}}}`, which may be as old as release {floor}: `cargo xtask release image-digest dist` is not a verb of release {floor}",
                digest_line + 1
            ),
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "        id: image\n",
            "        id: image\n        if: ${{ vars.CP_DEPLOY_AUDIENCE != '' }}\n"
        )),
        [
            "release.yml: job cp-deploy-digest step 6 has `if`, which a step of it may not",
            &format!(
                "release.yml: job cp-deploy-digest step 6 is `if ${{{{ vars.CP_DEPLOY_AUDIENCE != '' }}}} {}`, not `{}`",
                DIGEST_STEPS[5], DIGEST_STEPS[5]
            ),
        ]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "        id: image\n",
            "        id: digest\n"
        )),
        [format!(
            "release.yml: job cp-deploy-digest step 6 is `{}`, not `{}`",
            DIGEST_STEPS[5].replace(" id image", " id digest"),
            DIGEST_STEPS[5]
        )]
    );
    let mut outputs = DIGEST_OUTPUTS.to_vec();
    outputs[1].1 = "${{ steps.image.outputs.index }}";
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "digest: ${{ steps.image.outputs.digest }}",
            "digest: ${{ steps.image.outputs.index }}"
        )),
        [format!(
            "release.yml: job cp-deploy-digest outputs are {outputs:?}, not {DIGEST_OUTPUTS:?}"
        )]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "    timeout-minutes: 20\n",
            "    timeout-minutes: 360\n"
        )),
        ["release.yml: job cp-deploy-digest must have `timeout-minutes: 20`"]
    );
    assert_eq!(
        release_problems(&in_job(
            DIGEST_JOB,
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: ubuntu-22.04\n"
        )),
        ["release.yml: job cp-deploy-digest must run on `ubuntu-24.04`"]
    );
}

#[test]
fn a_call_of_the_updater_from_another_job_or_workflow_is_refused() {
    // Catches: a second way to deploy to the control plane, outside the jobs held here: a job
    // of another name in release.yml, or a job of another workflow, calling the updater's
    // workflow; and the deploy jobs removed or renamed.
    let text = release_yml();
    let caller = job_text(&text, JOB);
    let renamed = with_in(
        &text,
        caller,
        &caller.replacen("  cp-deploy:\n", "  cp-deploy-2:\n", 1),
    );
    let found = release_problems(&renamed);
    assert_eq!(
        found[..3],
        [
            "job cp-deploy-2: `id-token: write` is not allowed in release.yml",
            "job cp-deploy-2: no `runs-on` (a job that calls a reusable workflow is not evaluated)",
            "release.yml: jobs are [\"check-apt\", \"check-pkg\", \"check-rpm\", \"coordinator\", \"cp-deploy-2\", \"cp-deploy-digest\", \"image\", \"newest\", \"no-downgrade\", \"plan\", \"promote-check\", \"promote-pkg\", \"publish\", \"set-desired\", \"stable-apt-index\", \"stable-apt-index-2\", \"stable-apt-index-3\", \"stable-apt-read\", \"stable-apt-read-2\", \"stable-apt-read-3\", \"stable-apt-write\", \"stable-apt-write-2\", \"stable-apt-write-3\", \"stable-rpm-index\", \"stable-rpm-index-2\", \"stable-rpm-index-3\", \"stable-rpm-read\", \"stable-rpm-read-2\", \"stable-rpm-read-3\", \"stable-rpm-write\", \"stable-rpm-write-2\", \"stable-rpm-write-3\", \"targets\", \"testing-apt-index\", \"testing-apt-index-2\", \"testing-apt-index-3\", \"testing-apt-read\", \"testing-apt-read-2\", \"testing-apt-read-3\", \"testing-apt-write\", \"testing-apt-write-2\", \"testing-apt-write-3\", \"testing-fail-apt-index\", \"testing-fail-apt-read\", \"testing-fail-apt-write\", \"testing-pkg-upload\", \"testing-rpm-index\", \"testing-rpm-index-2\", \"testing-rpm-index-3\", \"testing-rpm-read\", \"testing-rpm-read-2\", \"testing-rpm-read-3\", \"testing-rpm-write\", \"testing-rpm-write-2\", \"testing-rpm-write-3\", \"verify\"], not plan, verify, publish, image, targets, coordinator, no-downgrade, set-desired, cp-deploy-digest, cp-deploy and the jobs of the package channels ([\"check-apt\", \"check-pkg\", \"check-rpm\", \"coordinator\", \"cp-deploy\", \"cp-deploy-digest\", \"image\", \"newest\", \"no-downgrade\", \"plan\", \"promote-check\", \"promote-pkg\", \"publish\", \"set-desired\", \"stable-apt-index\", \"stable-apt-index-2\", \"stable-apt-index-3\", \"stable-apt-read\", \"stable-apt-read-2\", \"stable-apt-read-3\", \"stable-apt-write\", \"stable-apt-write-2\", \"stable-apt-write-3\", \"stable-rpm-index\", \"stable-rpm-index-2\", \"stable-rpm-index-3\", \"stable-rpm-read\", \"stable-rpm-read-2\", \"stable-rpm-read-3\", \"stable-rpm-write\", \"stable-rpm-write-2\", \"stable-rpm-write-3\", \"targets\", \"testing-apt-index\", \"testing-apt-index-2\", \"testing-apt-index-3\", \"testing-apt-read\", \"testing-apt-read-2\", \"testing-apt-read-3\", \"testing-apt-write\", \"testing-apt-write-2\", \"testing-apt-write-3\", \"testing-fail-apt-index\", \"testing-fail-apt-read\", \"testing-fail-apt-write\", \"testing-pkg-upload\", \"testing-rpm-index\", \"testing-rpm-index-2\", \"testing-rpm-index-3\", \"testing-rpm-read\", \"testing-rpm-read-2\", \"testing-rpm-read-3\", \"testing-rpm-write\", \"testing-rpm-write-2\", \"testing-rpm-write-3\", \"verify\"])",
        ]
    );
    assert!(
        found.contains(&"release.yml: job cp-deploy is missing".to_owned()),
        "{found:?}"
    );
    let removed = with_in(&text, job_text(&text, DIGEST_JOB), "");
    assert!(
        release_problems(&removed)
            .contains(&"release.yml: job cp-deploy-digest is missing".to_owned())
    );
    let hello = workflows()
        .into_iter()
        .find(|(file, _)| file == "hello-desired.yml")
        .map(|(_, text)| text)
        .expect("hello-desired.yml exists");
    let call = format!(
        "{hello}  deploy:\n    if: ${{{{ github.ref == 'refs/heads/main' }}}}\n    uses: {DEPLOY_WORKFLOW} # v0.1.3\n    with:\n      service: hello\n"
    );
    assert_eq!(
        lint("hello-desired.yml", &call),
        ["job deploy: no `runs-on` (a job that calls a reusable workflow is not evaluated)"]
    );
}

#[test]
fn the_caller_passes_all_three_tailscale_inputs_from_the_repository_variables() {
    // Catches: a caller that drops the tailnet tags (tailscale/github-action then refuses the
    // federated join: "Please provide either an auth key, OAuth secret and tags, or federated
    // identity client ID and audience with tags"), the client ID or the audience; or one that
    // writes a fixed value in place of the repository variable.
    let tailscale = [
        (
            "tailscale-oauth-client-id",
            "${{ vars.CP_DEPLOY_TAILSCALE_CLIENT_ID }}",
        ),
        (
            "tailscale-audience",
            "${{ vars.CP_DEPLOY_TAILSCALE_AUDIENCE }}",
        ),
        ("tailscale-tags", "${{ vars.CP_DEPLOY_TAILSCALE_TAGS }}"),
    ];
    for pair in tailscale {
        assert!(WITH.contains(&pair), "{pair:?}");
    }
    for (input, value) in tailscale {
        let line = format!("      {input}: {value}\n");
        let rest: Vec<(&str, &str)> = WITH.iter().copied().filter(|(k, _)| *k != input).collect();
        assert_eq!(
            release_problems(&in_job(JOB, &line, "")),
            [format!(
                "release.yml: job cp-deploy passes {rest:?}, not {WITH:?}"
            )],
            "{input} dropped"
        );
        let mut fixed: Vec<(&str, &str)> = WITH
            .iter()
            .map(|&(k, v)| (k, if k == input { "tag:ci" } else { v }))
            .collect();
        fixed.sort_unstable();
        assert_eq!(
            release_problems(&in_job(JOB, &line, &format!("      {input}: tag:ci\n"))),
            [format!(
                "release.yml: job cp-deploy passes {fixed:?}, not {WITH:?}"
            )],
            "{input} fixed"
        );
    }
    assert!(DEPLOY_IF.contains(" && vars.CP_DEPLOY_TAILSCALE_TAGS != ''"));
}
