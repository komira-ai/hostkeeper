//! The rules of hostkeeper's own deploy in `release.yml` ([`super::desired`]): the repository's
//! file passes, and each rule is shown to refuse its defect by putting the defect into the
//! repository's `release.yml` and reading the exact problems the lint reports.

use super::callers::{CLIENT_COMMIT, CLIENT_RELEASE};
use super::desired::{DESIRED_IF, GATE, SET_IF, TARGETS_OUTPUTS, TARGETS_STEPS};
use super::lint::lint;
use super::release::publish_problems;
use super::{with_in, workflows};

/// The repository's release.yml.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// The text of the repository's `release.yml` from its job `targets` to the comment of the job
/// `newest` that follows them: the two deploy jobs, each of which a case below changes in one
/// place.
fn deploy_jobs() -> String {
    let text = release_yml();
    let at = text
        .find("\n  targets:\n")
        .expect("release.yml has a job targets");
    let end = text
        .find("\n  # newest ")
        .expect("release.yml has the job newest after them");
    text[at..end].to_owned()
}

/// Every problem the lint finds in release.yml with the one occurrence of `from` in its deploy
/// jobs replaced by `to`.
fn problems_with(from: &str, to: &str) -> Vec<String> {
    let text = release_yml();
    let deploy = deploy_jobs();
    let changed = text.replacen(&deploy, &with_in(&deploy, from, to), 1);
    let mut found = lint("release.yml", &changed);
    found.extend(publish_problems("release.yml", &changed));
    found
}

#[track_caller]
fn assert_problems(from: &str, to: &str, want: &[&str]) {
    assert_eq!(problems_with(from, to), want, "with {from:?} as {to:?}");
}

/// The `if:` line of the job targets.
fn targets_if() -> String {
    format!("    if: {DESIRED_IF}\n")
}

/// The `if:` line of the job set-desired.
fn set_if() -> String {
    format!("    if: {SET_IF}\n")
}

/// The problem a changed step `n` (from 1) of the job targets is reported with.
fn step_problem(n: usize, got: &str) -> String {
    format!(
        "release.yml: job targets step {n} is `{got}`, not `{}`",
        TARGETS_STEPS[n - 1]
    )
}

/// The problem of the job targets whose outputs are `got`.
fn outputs_problem(got: &[(&str, &str)]) -> String {
    format!("release.yml: job targets outputs are {got:?}, not {TARGETS_OUTPUTS:?}")
}

#[test]
fn the_deploy_jobs_of_the_repository_pass() {
    // Catches: a rule that refuses release.yml as it is, so that every case below would fail for
    // that reason instead of the defect it adds.
    let text = release_yml();
    assert_eq!(lint("release.yml", &text), Vec::<String>::new());
    assert_eq!(deploy_jobs().matches(&targets_if()).count(), 1);
    assert_eq!(deploy_jobs().matches(&set_if()).count(), 1);
}

#[test]
fn the_deploy_is_turned_on_only_by_the_repository_variable_being_true() {
    // Catches: the gate written against another variable, or as "set to anything", so that the
    // variable no longer turns the deploy off.
    let gate = format!(" && vars.{GATE} == 'true'");
    assert!(DESIRED_IF.contains(&gate) && SET_IF.contains(&gate));
    let text = release_yml().replace(&gate, "");
    assert_eq!(
        lint("release.yml", &text),
        [
            format!("release.yml: job targets needs the exact `if: {DESIRED_IF}`"),
            format!("release.yml: job set-desired needs the exact `if: {SET_IF}`"),
            format!("job set-desired needs the exact `if: {SET_IF}`"),
        ]
    );
    let text = release_yml().replace(&gate, &format!(" && vars.{GATE} != ''"));
    assert_eq!(lint("release.yml", &text).len(), 3);
}

#[test]
fn the_targets_job_without_its_if_is_refused() {
    // Catches: targets written on a dispatch from another branch, or with the deploy off.
    assert_problems(
        &targets_if(),
        "",
        &[&format!(
            "release.yml: job targets needs the exact `if: {DESIRED_IF}`"
        )],
    );
}

#[test]
fn a_set_desired_job_that_runs_for_a_release_older_than_the_newest_is_refused() {
    // Catches: a late run of an older commit, released after a newer one, setting its older
    // version and moving the fleet back; and the check that says so dropped from the targets job.
    let without = format!(
        "    if: {DESIRED_IF}\n    permissions:\n      contents: read\n      id-token: write\n"
    );
    assert_problems(
        &format!(
            "{}    permissions:\n      contents: read\n      id-token: write\n",
            set_if()
        ),
        &without,
        &[
            &format!("release.yml: job set-desired needs the exact `if: {SET_IF}`"),
            &format!("job set-desired needs the exact `if: {SET_IF}`"),
        ],
    );
    assert!(SET_IF.contains("needs.targets.outputs.newest == 'true'"));
    assert!(TARGETS_STEPS[6].contains(r#"== env.VERSION)"' >> "$GITHUB_OUTPUT""#));
    assert_problems(
        "      newest: ${{ steps.latest.outputs.newest }}\n",
        "",
        &[&outputs_problem(&TARGETS_OUTPUTS[1..])],
    );
    // The highest published version compared as text of another field: never the newest.
    let got = problems_with("== env.VERSION)", "== env.TAG)");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("release.yml: job targets step 7 is "));
}

#[test]
fn a_deploy_job_that_goes_on_after_a_failure_is_refused() {
    // Catches: a failed targets step that no longer stops the call.
    for (job, after) in [
        ("targets", "    timeout-minutes: 20\n"),
        ("set-desired", "    needs: [plan, targets]\n"),
    ] {
        assert_problems(
            after,
            &format!("{after}    continue-on-error: true\n"),
            &[&format!(
                "release.yml: job {job} has `continue-on-error`: a failure must stop the deploy"
            )],
        );
    }
}

#[test]
fn a_targets_step_that_can_be_skipped_or_fail_quietly_is_refused() {
    // Catches: the targets step given an `if:` that skips it or a `continue-on-error` that
    // ignores it, which would leave the step list itself unchanged.
    let step = "      - name: The targets of the release\n";
    for (key, line) in [
        ("continue-on-error", "        continue-on-error: true\n"),
        ("shell", "        shell: sh\n"),
    ] {
        assert_problems(
            step,
            &format!("{step}{line}"),
            &[&format!(
                "release.yml: job targets step 6 has `{key}`, which a step of it may not"
            )],
        );
    }
    // An `if:` is also part of the step's shape, so the step no longer matches its entry.
    assert_problems(
        step,
        &format!("{step}        if: ${{{{ false }}}}\n"),
        &[
            "release.yml: job targets step 6 has `if`, which a step of it may not",
            &step_problem(6, &format!("if ${{{{ false }}}} {}", TARGETS_STEPS[5])),
        ],
    );
}

#[test]
fn a_targets_job_that_does_not_wait_for_every_publish_is_refused() {
    // Catches: targets (and so a set) written before the release is published, or while a
    // release of the same run failed to publish.
    assert_problems(
        "    needs: [plan, publish]\n",
        "    needs: [plan, verify]\n",
        &["release.yml: job targets needs [\"plan\", \"verify\"], not [\"plan\", \"publish\"]"],
    );
    assert_problems(
        "    needs: [plan, publish]\n",
        "    needs: plan\n",
        &["release.yml: job targets needs [\"plan\"], not [\"plan\", \"publish\"]"],
    );
}

#[test]
fn a_targets_job_with_another_token_right_than_reading_the_repository_is_refused() {
    // Catches: a write scope, or the OIDC token the old channel job's join held, in a job that
    // only reads the release's files, and the permissions left to the repository's default.
    let read = "    permissions:\n      contents: read\n    outputs:\n      newest:";
    for to in [
        "    permissions:\n      contents: write\n    outputs:\n      newest:",
        "    permissions:\n      contents: read\n      id-token: write\n    outputs:\n      newest:",
        "    permissions:\n      contents: read\n      actions: read\n    outputs:\n      newest:",
        "    outputs:\n      newest:",
    ] {
        let found = problems_with(read, to);
        assert!(
            found.contains(
                &"release.yml: job targets must hold exactly `contents: read`".to_owned()
            ),
            "{to:?}: {found:?}"
        );
    }
}

#[test]
fn a_targets_job_that_reads_the_files_from_another_artifact_is_refused() {
    // Catches: the files taken from somewhere other than the ones this run verified and
    // published (another name, another run), and a step added after the checks.
    assert_problems(
        "          name: release-${{ steps.newest.outputs.tag }}\n",
        "          name: release-latest\n",
        &[&step_problem(
            5,
            "uses actions/download-artifact with name=release-latest with path=dist",
        )],
    );
    assert_problems(
        "          path: dist\n",
        "          path: dist\n          run-id: ${{ github.event.workflow_run.id }}\n",
        &[&step_problem(
            5,
            "uses actions/download-artifact with name=release-${{ steps.newest.outputs.tag }} with path=dist with run-id=${{ github.event.workflow_run.id }}",
        )],
    );
    let last = "join(\".\") == env.VERSION)\"' >> \"$GITHUB_OUTPUT\"\n";
    assert_problems(
        last,
        &format!("{last}      - run: echo extra\n"),
        &["release.yml: job targets has 8 steps, not the 7 of desired::TARGETS_STEPS"],
    );
}

#[test]
fn a_targets_job_that_writes_out_an_index_is_refused() {
    // Catches: an index or its digest passed on again, which a set of native entries alone does
    // not give (the coordinator would hold the targets to an index of archives they lack).
    let found = problems_with(
        "      targets: ${{ steps.targets.outputs.targets }}\n",
        "      targets: ${{ steps.targets.outputs.targets }}\n      release_index: ${{ steps.index.outputs.release_index }}\n",
    );
    assert_eq!(
        found,
        [outputs_problem(&[
            TARGETS_OUTPUTS[0],
            ("release_index", "${{ steps.index.outputs.release_index }}"),
            TARGETS_OUTPUTS[1],
            TARGETS_OUTPUTS[2],
        ])]
    );
}

#[test]
fn a_set_desired_job_that_does_not_wait_for_the_targets_is_refused() {
    // Catches: the call made straight after the publish, with no targets written yet and no
    // check that this run's release is the newest.
    assert_problems(
        "    needs: [plan, targets]\n",
        "    needs: [plan, publish]\n",
        &[
            "release.yml: job set-desired needs [\"plan\", \"publish\"], not [\"plan\", \"targets\"]",
        ],
    );
}

#[test]
fn a_set_desired_job_for_another_service_or_release_is_refused() {
    // Catches: another service set from release.yml, and a version that is not the one the
    // targets job wrote the targets of.
    assert_problems(
        "      service: hostkeeper\n",
        "      service: hello\n",
        &["release.yml: job set-desired must pass `service: hostkeeper`"],
    );
    assert_problems(
        "      version: ${{ needs.targets.outputs.version }}\n",
        "      version: ${{ needs.plan.outputs.version }}\n",
        &["release.yml: job set-desired must pass `version: ${{ needs.targets.outputs.version }}`"],
    );
}

#[test]
fn a_set_desired_job_that_passes_one_package_for_every_platform_or_an_index_is_refused() {
    // Catches: a package digest beside the targets, which orders every host one platform's
    // package, and an index or its digest, which a set of native entries alone does not give
    // (the coordinator would refuse targets that do not list the index's archives).
    for input in ["package_digest", "release_index", "release_index_digest"] {
        assert_problems(
            "      wait: false\n",
            &format!(
                "      wait: false\n      {input}: ${{{{ needs.targets.outputs.{input} }}}}\n"
            ),
            &[&format!(
                "release.yml: job set-desired passes {input}; a release is set with its native entries alone, each platform's its own"
            )],
        );
    }
}

#[test]
fn a_set_desired_job_that_waits_for_the_fleet_is_refused() {
    // Catches: a release run that holds the release group for the whole roll, and fails when the
    // roll takes longer than the job's polls.
    for to in ["      wait: true\n", ""] {
        assert_problems(
            "      wait: false\n",
            to,
            &["release.yml: job set-desired must pass `wait: false`"],
        );
    }
}

/// The pin of the CI client before it ran hkctl, as release.yml had it: release v0.1.66.
const OLD_COMMIT: &str = "2321ed897b4b9269471eadf169d7f29989161b4a";

/// The `uses:` line of the job set-desired at `commit`, with the version comment `release`.
fn uses_at(commit: &str, release: &str) -> String {
    format!(
        "    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@{commit} # v{release}\n"
    )
}

#[test]
fn a_set_desired_job_whose_commit_is_not_its_pin_is_refused() {
    // Catches: the client downloaded from another commit than the `set-desired.yml` it calls.
    assert_problems(
        &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
        &format!("      hostkeeper_commit: {OLD_COMMIT}\n"),
        &[&format!(
            "job set-desired must pass `hostkeeper_commit: {CLIENT_COMMIT}`, the CI client's revision"
        )],
    );
}

#[test]
fn the_deploy_runs_the_ci_clients_revision() {
    // Catches: release.yml's deploy left on a revision of set-desired.yml other than the one the
    // lint names, the CI client on hkctl.
    let deploy = deploy_jobs();
    assert_eq!(
        deploy
            .matches(&uses_at(CLIENT_COMMIT, CLIENT_RELEASE))
            .count(),
        1
    );
    assert_eq!(
        deploy
            .matches(&format!("      hostkeeper_version: {CLIENT_RELEASE}\n"))
            .count(),
        1
    );
    assert_eq!(
        deploy
            .matches(&format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"))
            .count(),
        1
    );
}

#[test]
fn a_deploy_pinned_to_the_ci_client_before_hkctl_is_refused() {
    // Catches: release.yml moved back to the revision of v0.1.66, which runs `hostkeeper ci`
    // polling `Releases.Converge`, in one consistent edit of the line and both inputs.
    let text = release_yml();
    let deploy = deploy_jobs();
    let old = with_in(
        &with_in(
            &with_in(
                &deploy,
                &uses_at(CLIENT_COMMIT, CLIENT_RELEASE),
                &uses_at(OLD_COMMIT, "0.1.66"),
            ),
            &format!("      hostkeeper_version: {CLIENT_RELEASE}\n"),
            "      hostkeeper_version: 0.1.66\n",
        ),
        &format!("      hostkeeper_commit: {CLIENT_COMMIT}\n"),
        &format!("      hostkeeper_commit: {OLD_COMMIT}\n"),
    );
    let changed = text.replacen(&deploy, &old, 1);
    assert_eq!(
        lint("release.yml", &changed),
        [
            format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `komira-ai/hostkeeper/.github/workflows/set-desired.yml@{OLD_COMMIT}`"
            ),
            format!(
                "job set-desired must name the CI client's release `# v{CLIENT_RELEASE}` after its `uses:`, not `# v0.1.66`"
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
fn a_set_desired_job_with_another_token_right_is_refused() {
    // Catches: a write scope beside id-token in the job whose steps hold the OIDC token.
    assert_problems(
        "      contents: read\n      id-token: write\n    uses:",
        "      contents: write\n      id-token: write\n    uses:",
        &[
            "job set-desired: `contents: write` is not allowed in release.yml",
            "job set-desired: permissions must be exactly contents: read and id-token: write",
        ],
    );
}

#[test]
fn a_targets_job_without_the_targets_or_with_targets_of_other_files_is_refused() {
    // Catches: the set made without targets, and targets written from files the job did not
    // download from this run, or for another version than the one set.
    let targets = "      - name: The targets of the release\n        id: targets\n        env:\n          VERSION: ${{ steps.newest.outputs.version }}\n        run: cargo xtask release targets --version \"$VERSION\" --dir dist --repo \"$GITHUB_REPOSITORY\" --output \"$GITHUB_OUTPUT\"\n";
    assert_problems(
        targets,
        "",
        &[
            "release.yml: job targets has 6 steps, not the 7 of desired::TARGETS_STEPS",
            &step_problem(6, TARGETS_STEPS[6]),
        ],
    );
    assert_problems(
        "--version \"$VERSION\" --dir dist --repo",
        "--version \"$VERSION\" --dir \"$RUNNER_TEMP\" --repo",
        &[&step_problem(
            6,
            "run cargo xtask release targets --version \"$VERSION\" --dir \"$RUNNER_TEMP\" --repo \"$GITHUB_REPOSITORY\" --output \"$GITHUB_OUTPUT\" env VERSION=${{ steps.newest.outputs.version }}",
        )],
    );
    assert_problems(
        "          VERSION: ${{ steps.newest.outputs.version }}\n        run: cargo xtask release targets",
        "          VERSION: ${{ needs.plan.outputs.version }}\n        run: cargo xtask release targets",
        &[&step_problem(
            6,
            "run cargo xtask release targets --version \"$VERSION\" --dir dist --repo \"$GITHUB_REPOSITORY\" --output \"$GITHUB_OUTPUT\" env VERSION=${{ needs.plan.outputs.version }}",
        )],
    );
}

#[test]
fn a_targets_job_whose_xtask_is_not_the_newest_releases_commit_is_refused() {
    // Catches: targets written by the xtask of the tip of main or of another commit than the
    // release's, and a checkout that leaves the token in the repository's git config.
    assert_problems(
        "          ref: ${{ steps.newest.outputs.commit }}\n",
        "          ref: ${{ github.sha }}\n",
        &[&step_problem(
            2,
            "uses actions/checkout with persist-credentials=false with ref=${{ github.sha }}",
        )],
    );
    assert_problems(
        "          ref: ${{ steps.newest.outputs.commit }}\n          persist-credentials: false\n",
        "          ref: ${{ steps.newest.outputs.commit }}\n",
        &[&step_problem(
            2,
            "uses actions/checkout with ref=${{ steps.newest.outputs.commit }}",
        )],
    );
}

#[test]
fn a_targets_job_whose_targets_output_is_not_the_targets_step_is_refused() {
    // Catches: targets passed on from a value no step of the job wrote from the checked files.
    let found = problems_with(
        "      targets: ${{ steps.targets.outputs.targets }}\n",
        "      targets: ${{ steps.newest.outputs.targets }}\n",
    );
    assert_eq!(
        found,
        [outputs_problem(&[
            TARGETS_OUTPUTS[0],
            ("targets", "${{ steps.newest.outputs.targets }}"),
            TARGETS_OUTPUTS[2],
        ])]
    );
}

#[test]
fn a_set_desired_job_without_the_targets_jobs_targets_is_refused() {
    // Catches: the set made with no targets (no host is ordered), and targets passed from
    // another job than the one that wrote them from the checked files.
    let want =
        "release.yml: job set-desired must pass `targets: ${{ needs.targets.outputs.targets }}`";
    let line = "      targets: ${{ needs.targets.outputs.targets }}\n";
    assert_problems(line, "", &[want]);
    assert_problems(
        line,
        "      targets: ${{ needs.plan.outputs.targets }}\n",
        &[want],
    );
}
