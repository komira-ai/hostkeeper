//! The rules of hostkeeper's own deploy in `release.yml` ([`super::desired`]): the repository's
//! file passes, and each rule is shown to refuse its defect by putting the defect into the
//! repository's `release.yml` and reading the exact problems the lint reports.

use super::desired::{CHANNEL_STEPS, DESIRED_IF, GATE, SET_IF};
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

/// The text of the repository's `release.yml` from its job `channel` to its end: the two deploy
/// jobs, each of which a case below changes in one place.
fn deploy_jobs() -> String {
    let text = release_yml();
    let at = text
        .find("\n  channel:\n")
        .expect("release.yml has a job channel");
    text[at..].to_owned()
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

/// The `if:` line of the job channel.
fn channel_if() -> String {
    format!("    if: {DESIRED_IF}\n")
}

/// The `if:` line of the job set-desired.
fn set_if() -> String {
    format!("    if: {SET_IF}\n")
}

/// The problem a changed step `n` (from 1) of the job channel is reported with.
fn step_problem(n: usize, got: &str) -> String {
    format!(
        "release.yml: job channel step {n} is `{got}`, not `{}`",
        CHANNEL_STEPS[n - 1]
    )
}

#[test]
fn the_deploy_jobs_of_the_repository_pass() {
    // Catches: a rule that refuses release.yml as it is, so that every case below would fail for
    // that reason instead of the defect it adds.
    let text = release_yml();
    assert_eq!(lint("release.yml", &text), Vec::<String>::new());
    assert_eq!(deploy_jobs().matches(&channel_if()).count(), 1);
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
            format!("release.yml: job channel needs the exact `if: {DESIRED_IF}`"),
            format!("release.yml: job set-desired needs the exact `if: {SET_IF}`"),
            format!("job set-desired needs the exact `if: {SET_IF}`"),
        ]
    );
    let text = release_yml().replace(&gate, &format!(" && vars.{GATE} != ''"));
    assert_eq!(lint("release.yml", &text).len(), 3);
}

#[test]
fn the_channel_job_without_its_if_is_refused() {
    // Catches: a channel wait that runs on a dispatch from another branch, or with the deploy off.
    assert_problems(
        &channel_if(),
        "",
        &[&format!(
            "release.yml: job channel needs the exact `if: {DESIRED_IF}`"
        )],
    );
}

#[test]
fn a_set_desired_job_that_runs_for_a_release_older_than_the_newest_is_refused() {
    // Catches: a late run of an older commit, released after a newer one, setting its older
    // version and moving the fleet back; and the check that says so dropped from the channel job.
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
    assert!(SET_IF.contains("needs.channel.outputs.newest == 'true'"));
    assert!(CHANNEL_STEPS[5].contains(r#"== env.VERSION)"' >> "$GITHUB_OUTPUT""#));
    assert_problems(
        "      newest: ${{ steps.latest.outputs.newest }}\n",
        "",
        &[
            "release.yml: job channel outputs are [(\"release_index\", \"${{ steps.index.outputs.release_index }}\"), (\"release_index_digest\", \"${{ steps.index.outputs.release_index_digest }}\"), (\"version\", \"${{ steps.newest.outputs.version }}\")], not [(\"newest\", \"${{ steps.latest.outputs.newest }}\"), (\"release_index\", \"${{ steps.index.outputs.release_index }}\"), (\"release_index_digest\", \"${{ steps.index.outputs.release_index_digest }}\"), (\"version\", \"${{ steps.newest.outputs.version }}\")]",
        ],
    );
    // The highest published version compared as text of another field: never the newest.
    let got = problems_with("== env.VERSION)", "== env.TAG)");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("release.yml: job channel step 6 is "));
}

#[test]
fn a_deploy_job_that_goes_on_after_a_failure_is_refused() {
    // Catches: a failed wait or comparison that no longer stops the call.
    for (job, after) in [
        ("channel", "    timeout-minutes: 40\n"),
        ("set-desired", "    needs: [plan, channel]\n"),
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
fn a_channel_step_that_can_be_skipped_or_fail_quietly_is_refused() {
    // Catches: the wait or the byte comparison given an `if:` that skips it or a
    // `continue-on-error` that ignores it, which would leave the step list itself unchanged.
    let compare = "      - name: The channel serves the published index, byte for byte\n";
    for (key, line) in [
        ("continue-on-error", "        continue-on-error: true\n"),
        ("if", "        if: ${{ false }}\n"),
        ("shell", "        shell: sh\n"),
    ] {
        assert_problems(
            compare,
            &format!("{compare}{line}"),
            &[&format!(
                "release.yml: job channel step 4 has `{key}`, which a step of it may not"
            )],
        );
    }
}

#[test]
fn a_channel_job_that_does_not_wait_for_every_publish_is_refused() {
    // Catches: a wait (and so a set) that starts before the release is published, or while a
    // release of the same run failed to publish.
    assert_problems(
        "    needs: [plan, publish]\n",
        "    needs: [plan, verify]\n",
        &["release.yml: job channel needs [\"plan\", \"verify\"], not [\"plan\", \"publish\"]"],
    );
    assert_problems(
        "    needs: [plan, publish]\n",
        "    needs: plan\n",
        &["release.yml: job channel needs [\"plan\"], not [\"plan\", \"publish\"]"],
    );
}

#[test]
fn a_channel_job_with_another_token_right_than_reading_the_repository_is_refused() {
    // Catches: a write scope in the job that reads an outside origin, and the permissions left
    // to the repository's default.
    let read = "    permissions:\n      contents: read\n    outputs:\n";
    for to in [
        "    permissions:\n      contents: write\n    outputs:\n",
        "    permissions:\n      contents: read\n      actions: read\n    outputs:\n",
        "    outputs:\n",
    ] {
        let found = problems_with(read, to);
        assert!(
            found.contains(
                &"release.yml: job channel must hold exactly `contents: read`".to_owned()
            ),
            "{to:?}: {found:?}"
        );
    }
}

#[test]
fn a_channel_wait_that_gives_up_on_a_missing_version_or_reads_another_channel_is_refused() {
    // Catches: a wait that fails at the first 404 (the sync has not run yet), a read of the
    // channel over plain http, and a wait on another channel or version than the one set.
    let wait = "--retry-max-time 1800 --retry-all-errors --output";
    // The download rule (`downloads.rs`) refuses the first two as well, on the step's line.
    let line = release_yml()
        .lines()
        .position(|l| l.contains("--retry-max-time 1800"))
        .map(|at| at + 1)
        .expect("the channel wait");
    for (from, to, lacks) in [
        (
            wait,
            "--retry-max-time 1800 --output",
            Some("`--retry-all-errors`"),
        ),
        (
            "curl --proto =https --proto-redir =https --tlsv1.2",
            "curl --proto-redir =https --tlsv1.2",
            Some("`--proto =https`"),
        ),
        (
            "RELEASE_CHANNEL: ${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}",
            "RELEASE_CHANNEL: https://other.example",
            None,
        ),
    ] {
        let got = problems_with(from, to);
        assert_eq!(got.len(), 1 + usize::from(lacks.is_some()), "{got:?}");
        assert!(
            got[0].starts_with("release.yml: job channel step 3 is `run curl "),
            "{got:?}"
        );
        if from == wait {
            assert!(
                got[0].ends_with(&format!(", not `{}`", CHANNEL_STEPS[2])),
                "{got:?}"
            );
        }
        if let Some(lacks) = lacks {
            assert_eq!(got[1], format!("line {line}: a curl lacks {lacks}"));
        }
    }
}

#[test]
fn a_channel_job_that_does_not_compare_the_served_index_is_refused() {
    // Catches: a set made from the published index while the channel serves other bytes (which
    // it never replaces), and the comparison dropped.
    let compare = "      - name: The channel serves the published index, byte for byte\n        run: cmp dist/index.json \"$RUNNER_TEMP/served-index.json\"\n";
    assert_problems(
        compare,
        "",
        &[
            "release.yml: job channel has 5 steps, not the 6 of desired::CHANNEL_STEPS",
            &step_problem(4, CHANNEL_STEPS[4]),
            &step_problem(5, CHANNEL_STEPS[5]),
        ],
    );
    assert_problems(
        "cmp dist/index.json",
        "cmp --quiet dist/index.json",
        &[&step_problem(
            4,
            "run cmp --quiet dist/index.json \"$RUNNER_TEMP/served-index.json\"",
        )],
    );
}

#[test]
fn a_channel_job_that_reads_the_index_from_another_artifact_is_refused() {
    // Catches: the index taken from somewhere other than the files this run verified and
    // published (another name, another run), and a step added after the checks.
    assert_problems(
        "          name: release-${{ steps.newest.outputs.tag }}\n",
        "          name: release-latest\n",
        &[&step_problem(
            2,
            "uses actions/download-artifact with name=release-latest with path=dist",
        )],
    );
    assert_problems(
        "          path: dist\n",
        "          path: dist\n          run-id: ${{ github.event.workflow_run.id }}\n",
        &[&step_problem(
            2,
            "uses actions/download-artifact with name=release-${{ steps.newest.outputs.tag }} with path=dist with run-id=${{ github.event.workflow_run.id }}",
        )],
    );
    let last = "join(\".\") == env.VERSION)\"' >> \"$GITHUB_OUTPUT\"\n";
    assert_problems(
        last,
        &format!("{last}      - run: echo extra\n"),
        &["release.yml: job channel has 7 steps, not the 6 of desired::CHANNEL_STEPS"],
    );
}

#[test]
fn a_channel_job_whose_outputs_are_not_its_checked_steps_is_refused() {
    // Catches: the index or its digest passed on from a value the job did not compare with the
    // channel.
    assert_problems(
        "      release_index_digest: ${{ steps.index.outputs.release_index_digest }}\n",
        "      release_index_digest: ${{ steps.newest.outputs.release_index_digest }}\n",
        &[
            "release.yml: job channel outputs are [(\"newest\", \"${{ steps.latest.outputs.newest }}\"), (\"release_index\", \"${{ steps.index.outputs.release_index }}\"), (\"release_index_digest\", \"${{ steps.newest.outputs.release_index_digest }}\"), (\"version\", \"${{ steps.newest.outputs.version }}\")], not [(\"newest\", \"${{ steps.latest.outputs.newest }}\"), (\"release_index\", \"${{ steps.index.outputs.release_index }}\"), (\"release_index_digest\", \"${{ steps.index.outputs.release_index_digest }}\"), (\"version\", \"${{ steps.newest.outputs.version }}\")]",
        ],
    );
}

#[test]
fn a_set_desired_job_that_does_not_wait_for_the_channel_is_refused() {
    // Catches: the call made straight after the publish, which the coordinator refuses
    // (`not_in_channel`) until the sync has run.
    assert_problems(
        "    needs: [plan, channel]\n",
        "    needs: [plan, publish]\n",
        &[
            "release.yml: job set-desired needs [\"plan\", \"publish\"], not [\"channel\", \"plan\"]",
        ],
    );
}

#[test]
fn a_set_desired_job_for_another_service_or_release_is_refused() {
    // Catches: another service set from release.yml, and a version, index or digest that is not
    // the one the channel job checked.
    assert_problems(
        "      service: hostkeeper\n",
        "      service: hello\n",
        &["release.yml: job set-desired must pass `service: hostkeeper`"],
    );
    for input in ["version", "release_index", "release_index_digest"] {
        let from = format!("      {input}: ${{{{ needs.channel.outputs.{input} }}}}\n");
        let to = format!("      {input}: ${{{{ needs.plan.outputs.{input} }}}}\n");
        assert_problems(
            &from,
            &to,
            &[&format!(
                "release.yml: job set-desired must pass `{input}: ${{{{ needs.channel.outputs.{input} }}}}`"
            )],
        );
    }
}

#[test]
fn a_set_desired_job_that_passes_one_package_for_every_platform_is_refused() {
    // Catches: a package digest beside the index, which the client refuses, or in place of it,
    // which orders every host one platform's package.
    assert_problems(
        "      wait: false\n",
        "      wait: false\n      package_digest: ${{ needs.channel.outputs.digest }}\n",
        &[
            "release.yml: job set-desired passes package_digest; the release index names each platform's package",
        ],
    );
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

#[test]
fn a_set_desired_job_whose_commit_is_not_its_pin_is_refused() {
    // Catches: the client downloaded from another commit than the `set-desired.yml` it calls.
    let text = deploy_jobs();
    let line = text
        .lines()
        .find(|l| l.starts_with("      hostkeeper_commit: "))
        .expect("the job passes hostkeeper_commit");
    assert_problems(
        &format!("{line}\n"),
        "      hostkeeper_commit: 0123456789abcdef0123456789abcdef01234567\n",
        &["release.yml: job set-desired must pass the hostkeeper_commit its `uses:` is pinned to"],
    );
}

#[test]
fn a_set_desired_job_with_another_token_right_is_refused() {
    // Catches: a write scope beside id-token in the job whose steps hold the OIDC token.
    assert_problems(
        "      contents: read\n      id-token: write\n",
        "      contents: write\n      id-token: write\n",
        &[
            "job set-desired: `contents: write` is not allowed in release.yml",
            "job set-desired: permissions must be exactly contents: read and id-token: write",
        ],
    );
}
