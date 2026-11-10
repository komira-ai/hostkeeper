//! The rules of the job `coordinator` of `release.yml` ([`super::coordinator_wait`]): the
//! repository's file passes, and each rule is shown to refuse its defect by putting the defect
//! into the job and reading the exact problems the lint reports.

use super::coordinator_wait::{JOB, STEPS};
use super::desired::SET_IF;
use super::lint::{ID_TOKEN_ALLOWED, WRITE_ALLOWED, lint};
use super::{with_in, workflows};

/// The repository's release.yml.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// The text of the job from its line to the comment of `set-desired` that follows it.
fn coordinator_job() -> String {
    let text = release_yml();
    let at = text
        .find("\n  coordinator:\n")
        .expect("release.yml has a job coordinator");
    let end = text
        .find("\n  # set-desired ")
        .expect("release.yml has the comment of set-desired after it");
    text[at..end].to_owned()
}

/// Every problem the lint finds in release.yml with the one occurrence of `from` in the job
/// `coordinator` replaced by `to`.
fn found(from: &str, to: &str) -> Vec<String> {
    let text = release_yml();
    let job = coordinator_job();
    lint(
        "release.yml",
        &text.replacen(&job, &with_in(&job, from, to), 1),
    )
}

/// The problems of `found` that are about the job's shape (not the write-scope table's).
fn about_the_job(problems: Vec<String>) -> Vec<String> {
    problems
        .into_iter()
        .filter(|p| p.starts_with("release.yml: job coordinator"))
        .collect()
}

/// The problem a step `n` (from 1) that is `got` is reported with.
fn step_problem(n: usize, got: &str) -> String {
    format!(
        "release.yml: job coordinator step {n} is `{got}`, not `{}`",
        STEPS[n - 1]
    )
}

#[test]
fn the_coordinator_job_of_the_repository_passes() {
    // Catches: a rule that refuses release.yml as it is, so that every case below would fail for
    // that reason instead of the defect it adds.
    assert_eq!(lint("release.yml", &release_yml()), Vec::<String>::new());
    assert_eq!(JOB, "coordinator");
}

#[test]
fn id_token_write_is_allowed_to_the_coordinator_job_alone_and_under_the_set_desired_if() {
    // Catches: the OIDC scope granted to the whole table of release.yml's jobs, a second scope
    // for the coordinator job, or a condition other than set-desired's.
    let mine: Vec<_> = WRITE_ALLOWED.iter().filter(|a| a.job == JOB).collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(
        (mine[0].file, mine[0].scope, mine[0].condition),
        ("release.yml", "id-token", SET_IF)
    );
    assert!(
        ID_TOKEN_ALLOWED
            .iter()
            .all(|(file, _)| *file != "release.yml")
    );
}

#[test]
fn a_coordinator_job_under_another_if_is_refused() {
    // Catches: the wait run on a branch other than main, with the deploy off, or for a release
    // that is not the newest, any of which would hold the OIDC scope under a wider condition.
    let line = format!("    if: {SET_IF}\n");
    let narrower = "    if: ${{ needs.plan.outputs.release == 'true' }}\n";
    let got = found(&line, narrower);
    assert_eq!(
        got,
        [
            format!("job coordinator: `id-token: write` needs the exact `if: {SET_IF}`"),
            format!("release.yml: job coordinator needs the exact `if: {SET_IF}`"),
        ]
    );
    let without = found(&line, "");
    assert_eq!(
        about_the_job(without),
        [format!(
            "release.yml: job coordinator needs the exact `if: {SET_IF}`"
        )]
    );
}

#[test]
fn a_coordinator_job_with_another_token_right_than_the_oidc_token_is_refused() {
    // Catches: the job given a repository right, a write scope or another read scope beside the
    // tailnet join's OIDC token, or none, so that it falls back to the repository's default.
    let exactly = "release.yml: job coordinator must hold exactly `id-token: write`".to_owned();
    let perms = "    permissions:\n      id-token: write\n";
    for to in [
        "    permissions:\n      id-token: write\n      contents: read\n",
        "    permissions:\n      id-token: write\n      actions: read\n",
        "    permissions:\n      contents: read\n",
        "    permissions:\n      id-token: read\n",
        "",
    ] {
        assert_eq!(
            about_the_job(found(perms, to)),
            std::slice::from_ref(&exactly),
            "{to:?}"
        );
    }
    assert_eq!(
        found(
            perms,
            "    permissions:\n      id-token: write\n      contents: write\n"
        ),
        [
            "job coordinator: `contents: write` is not allowed in release.yml".to_owned(),
            exactly.clone()
        ]
    );
    assert_eq!(
        found(
            perms,
            "    permissions:\n      id-token: write\n      attestations: write\n"
        ),
        [
            "job coordinator: `attestations: write` is not allowed in release.yml".to_owned(),
            exactly
        ]
    );
}

#[test]
fn a_coordinator_job_that_does_not_wait_for_the_targets_is_refused() {
    // Catches: the wait started before the targets job decided whether this run's release is
    // the newest (the version it asks for is the targets job's output), or after nothing.
    for (to, got) in [
        ("[plan]", "[\"plan\"]"),
        ("[plan, publish]", "[\"plan\", \"publish\"]"),
        ("targets", "[\"targets\"]"),
    ] {
        assert_eq!(
            about_the_job(found(
                "    needs: [plan, targets]\n",
                &format!("    needs: {to}\n")
            )),
            [format!(
                "release.yml: job coordinator needs {got}, not [\"plan\", \"targets\"]"
            )],
            "{to}"
        );
    }
}

#[test]
fn a_coordinator_job_on_another_runner_or_with_another_timeout_is_refused() {
    // Catches: a runner label that moves under us, and a wait that is cut short by the job's
    // timeout or left to hold the release group for hours.
    assert_eq!(
        about_the_job(found(
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: ubuntu-latest\n"
        )),
        ["release.yml: job coordinator must run on `ubuntu-24.04`"]
    );
    for minutes in ["25", "360"] {
        assert_eq!(
            about_the_job(found(
                "    timeout-minutes: 40\n",
                &format!("    timeout-minutes: {minutes}\n")
            )),
            ["release.yml: job coordinator must have `timeout-minutes: 40`"],
            "{minutes}"
        );
    }
}

#[test]
fn a_coordinator_job_with_a_key_that_could_carry_other_code_is_refused() {
    // Catches: a job-level `env`, `container`, `services`, `outputs` or `defaults` (a shell that
    // is not bash) added to a job that holds an OIDC token.
    for (key, line) in [
        ("env", "    env:\n      A: b\n"),
        ("container", "    container: alpine\n"),
        ("outputs", "    outputs:\n      a: b\n"),
        ("defaults", "    defaults:\n      run:\n        shell: sh\n"),
    ] {
        assert_eq!(
            about_the_job(found(
                "    timeout-minutes: 40\n",
                &format!("    timeout-minutes: 40\n{line}")
            )),
            [format!(
                "release.yml: job coordinator has `{key}`, which it may not"
            )],
            "{key}"
        );
    }
}

#[test]
fn a_coordinator_job_that_goes_on_after_a_failure_is_refused() {
    // Catches: a failed wait that no longer stops the set, at the job (the deploy rule of
    // desired.rs) or on the wait step.
    assert_eq!(
        about_the_job(found(
            "    timeout-minutes: 40\n",
            "    timeout-minutes: 40\n    continue-on-error: true\n"
        )),
        ["release.yml: job coordinator has `continue-on-error`: a failure must stop the deploy"]
    );
    assert_eq!(
        about_the_job(found(
            "        id: wait\n",
            "        id: wait\n        continue-on-error: true\n"
        )),
        ["release.yml: job coordinator step 4 has `continue-on-error`, which a step of it may not"]
    );
}

#[test]
fn a_wait_that_is_not_the_one_exact_request_is_refused() {
    // Catches: the request without the pinned CA (any certificate accepted, or the runner's
    // trust store), over a plain-HTTP or older TLS, with a token header (the route takes none),
    // asking another route or no version, with fewer retries than the 30 minutes, or ignoring
    // the answer's status.
    let wait = STEPS[3].split(" env ").next().expect("the command");
    let command = wait.strip_prefix("run ").expect("a run step");
    let tail = " env COORDINATOR_URL=${{ vars.HOSTKEEPER_COORDINATOR_URL }} env VERSION=${{ needs.targets.outputs.version }}";
    for (from, to) in [
        (" --cacert \"$RUNNER_TEMP/coordinator-ca.pem\"", ""),
        (
            " --cacert \"$RUNNER_TEMP/coordinator-ca.pem\"",
            " --insecure",
        ),
        (" --proto =https", ""),
        (" --tlsv1.2", ""),
        (" --fail-with-body", ""),
        (" --retry 180", " --retry 6"),
        (" --retry-max-time 1800", " --retry-max-time 60"),
        (" --retry-all-errors", ""),
        (
            " --silent",
            " --silent --header \"Authorization:Bearer $TOKEN\"",
        ),
        ("?at_least=$VERSION", ""),
        ("/v1/version?", "/v1/health?"),
    ] {
        let changed = command.replacen(from, to, 1);
        assert_ne!(changed, command, "{from:?} is in the command");
        assert_eq!(
            about_the_job(found(command, &changed)),
            [step_problem(4, &format!("run {changed}{tail}"))],
            "{from:?} as {to:?}"
        );
    }
}

#[test]
fn a_wait_whose_values_do_not_come_from_the_variables_and_the_targets_job_is_refused() {
    // Catches: the URL or the version taken from somewhere else, or an expression written into
    // the script, where a value holding shell syntax would run.
    let url = "          COORDINATOR_URL: ${{ vars.HOSTKEEPER_COORDINATOR_URL }}\n";
    assert_eq!(
        about_the_job(found(
            url,
            "          COORDINATOR_URL: ${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}\n"
        )),
        [step_problem(
            4,
            &STEPS[3].replace("HOSTKEEPER_COORDINATOR_URL", "HOSTKEEPER_RELEASE_CHANNEL")
        )]
    );
    let version = "          VERSION: ${{ needs.targets.outputs.version }}\n        run: curl";
    assert_eq!(
        about_the_job(found(
            version,
            "          VERSION: ${{ needs.plan.outputs.version }}\n        run: curl"
        )),
        [step_problem(
            4,
            &STEPS[3].replace("needs.targets.outputs", "needs.plan.outputs")
        )]
    );
    let inline = "--url \"$COORDINATOR_URL/v1/version";
    let got = about_the_job(found(
        inline,
        "--url \"${{ vars.HOSTKEEPER_COORDINATOR_URL }}/v1/version",
    ));
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(
        got[0],
        "release.yml: job coordinator step 4 writes an expression into its `run:`; pass the value through `env:`"
    );
    assert!(got[1].starts_with("release.yml: job coordinator step 4 is `run curl "));
}

#[test]
fn a_coordinator_job_that_runs_another_action_or_our_code_is_refused() {
    // Catches: a checkout, a download, a cargo command or any other action added to a job that
    // holds an OIDC token (only curl, jq and the pinned tailscale action run), and the join
    // replaced by another action or an unpinned version of the same one.
    let join =
        "        uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0\n";
    let checkout = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          persist-credentials: false\n";
    let ca = "      - name: Write the coordinator's CA\n";
    let got = about_the_job(found(ca, &format!("{checkout}{ca}")));
    assert_eq!(got.len(), 5, "{got:?}");
    assert_eq!(
        got[0],
        "release.yml: job coordinator has 6 steps, not the 5 of coordinator_wait::STEPS"
    );
    assert!(got[1].starts_with("release.yml: job coordinator step 3 is `uses actions/checkout "));
    assert_eq!(
        got[4],
        format!(
            "release.yml: job coordinator step ids are {:?}, not [\"tailnet-tag\", \"tailnet\", \"ca\", \"wait\", \"\"]",
            ["tailnet-tag", "tailnet", "", "ca", "wait", ""]
        )
    );
    for run in [
        "      - run: cargo xtask --help\n",
        "      - run: gh release download v1\n",
    ] {
        let got = about_the_job(found(ca, &format!("{run}{ca}")));
        assert_eq!(got.len(), 5, "{run:?}: {got:?}");
        assert!(got[0].contains("has 6 steps"), "{got:?}");
    }
    assert_eq!(
        about_the_job(found(
            join,
            "        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n"
        )),
        [step_problem(
            2,
            &STEPS[1].replace("tailscale/github-action", "actions/checkout")
        )]
    );
}

#[test]
fn a_tailnet_join_that_differs_from_the_ci_clients_is_refused() {
    // Catches: another Tailscale version or digest, the cache turned on, no ping of the
    // coordinator, another tag, or a join that does not wait for the client ID variable.
    let join_line = "          version: 1.94.2\n";
    assert_eq!(
        about_the_job(found(join_line, "          version: 1.94.3\n")),
        [step_problem(2, &STEPS[1].replace("1.94.2", "1.94.3"))]
    );
    assert_eq!(
        about_the_job(found(
            "          use-cache: false\n",
            "          use-cache: true\n"
        )),
        [step_problem(
            2,
            &STEPS[1].replace("use-cache=false", "use-cache=true")
        )]
    );
    assert_eq!(
        about_the_job(found(
            "          ping: ${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}\n",
            ""
        )),
        [step_problem(
            2,
            &STEPS[1].replace(" with ping=${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}", "")
        )]
    );
    assert_eq!(
        about_the_job(found(
            "          tags: ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n",
            "          tags: tag:ci\n"
        )),
        [step_problem(
            2,
            &STEPS[1].replace("tags=${{ vars.HOSTKEEPER_TAILNET_TAG }}", "tags=tag:ci")
        )]
    );
    let condition = "        if: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}\n";
    let shape = STEPS[1]
        .strip_prefix("if ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} ")
        .expect("the join has its if");
    assert_eq!(
        about_the_job(found(condition, "")),
        [step_problem(2, shape)]
    );
    assert_eq!(
        about_the_job(found(condition, "        if: ${{ always() }}\n")),
        [step_problem(
            2,
            &STEPS[1].replace(
                "${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} uses",
                "${{ always() }} uses"
            )
        )]
    );
}

#[test]
fn a_coordinator_job_whose_first_step_is_not_the_tag_check_is_refused() {
    // Catches: the tag check dropped or changed, so the join could ask for any tag.
    let check = "      - name: Check the tailnet tag\n";
    let at = coordinator_job().find(check).expect("the check");
    let job = coordinator_job();
    let step = &job[at..job
        .find("      - name: Join the tailnet\n")
        .expect("the join")];
    let got = about_the_job(found(step, ""));
    assert!(got[0].contains("has 4 steps, not the 5"), "{got:?}");
    assert_eq!(got.len(), 6, "{got:?}");
    assert_eq!(
        about_the_job(found(
            "          TAILNET_TAG: ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n",
            "          TAILNET_TAG: tag:ci\n"
        )),
        [step_problem(
            1,
            &STEPS[0].replace("${{ vars.HOSTKEEPER_TAILNET_TAG }}", "tag:ci")
        )]
    );
}

#[test]
fn a_coordinator_job_whose_steps_have_other_ids_is_refused() {
    // Catches: the wait step renamed, so that the error step reads an outcome that is never
    // there and says nothing when the wait fails.
    let ids = "release.yml: job coordinator step ids are [\"tailnet-tag\", \"tailnet\", \"ca\", \"waited\", \"\"], not [\"tailnet-tag\", \"tailnet\", \"ca\", \"wait\", \"\"]";
    assert_eq!(
        about_the_job(found("        id: wait\n", "        id: waited\n")),
        [ids]
    );
}

#[test]
fn a_coordinator_job_whose_error_step_can_be_skipped_is_refused() {
    // Catches: the message that says why the wait failed gone, run on success, or run on any
    // failure (including the tag check's) with a wrong word.
    let condition = "        if: ${{ failure() && steps.wait.outcome == 'failure' }}\n";
    assert_eq!(
        about_the_job(found(condition, "")),
        [step_problem(
            5,
            STEPS[4]
                .strip_prefix("if ${{ failure() && steps.wait.outcome == 'failure' }} ")
                .expect("the error step has its if")
        )]
    );
    assert_eq!(
        about_the_job(found(condition, "        if: ${{ always() }}\n")),
        [step_problem(
            5,
            &STEPS[4].replace(
                "${{ failure() && steps.wait.outcome == 'failure' }}",
                "${{ always() }}"
            )
        )]
    );
    assert_eq!(
        about_the_job(found("within 30 minutes.", "within 3 minutes.")),
        [step_problem(5, &STEPS[4].replace("within 30", "within 3"))]
    );
}

#[test]
fn a_coordinator_job_missing_is_refused() {
    // Catches: the job deleted, with set-desired left to call a coordinator that may be older.
    let text = release_yml();
    let job = coordinator_job();
    let got = lint("release.yml", &text.replacen(&job, "", 1));
    let mine = about_the_job(got);
    assert_eq!(
        mine,
        [
            format!("release.yml: job coordinator needs the exact `if: {SET_IF}`"),
            "release.yml: job coordinator is missing".to_owned(),
        ]
    );
}
