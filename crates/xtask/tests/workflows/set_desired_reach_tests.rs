//! The rules of [`set_desired`](super::set_desired) for the wait for the coordinator after the
//! join, the step `reach`: on the real `set-desired.yml`, and on [`CLIENT`] with one defect put
//! into it, reading the exact problems the lint reports.

use super::set_desired::{CLIENT_STEP, JOIN_STEP, REACH_RUN, REACH_STEP};
use super::set_desired_tests::{CLIENT, assert_client, moved, real, step_ids};
use super::with_in;

/// The step `reach` of [`CLIENT`], whole.
const REACH_BLOCK: &str = "      - id: reach
        if: ${{ inputs.join_tailnet }}
        env:
          COORDINATOR_URL: ${{ inputs.coordinator_url }}
        run: |
          \"$RUNNER_TEMP/hk-release/hk\" \\
            --endpoint \"$COORDINATOR_URL\" \\
            --ca-file \"$RUNNER_TEMP/hk-coordinator-ca.pem\" \\
            tailnet-wait
";

/// [`CLIENT`] with `from` in the step `reach` replaced by `to`.
fn reach_with(from: &str, to: &str) -> String {
    assert!(REACH_BLOCK.contains(from), "{from:?} is in the step");
    with_in(CLIENT, REACH_BLOCK, &REACH_BLOCK.replacen(from, to, 1))
}

/// What the lint says of a `run:` of the step that is not its one command.
const NOT_ONE: &str = "the wait step `reach` (step 10) is not the one command `$RUNNER_TEMP/hk-release/hk --endpoint $COORDINATOR_URL --ca-file $RUNNER_TEMP/hk-coordinator-ca.pem tailnet-wait`";

#[test]
fn the_real_client_waits_for_the_coordinator_after_the_join_and_the_ca() {
    // Catches: the wait dropped from the real file, or moved where it cannot read the CA or
    // comes after the first call it is there to make safe.
    let ids = step_ids(&real());
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    assert!(at(JOIN_STEP) < at(REACH_STEP), "{ids:?}");
    assert!(at("ca") < at(REACH_STEP), "{ids:?}");
    assert!(at(REACH_STEP) < at(CLIENT_STEP), "{ids:?}");
    assert_client(CLIENT, &[]);
    assert_eq!(REACH_RUN.last(), Some(&"tailnet-wait"));
}

#[test]
fn a_client_without_the_wait_or_with_two_is_refused() {
    // Catches: the join left to answer for the coordinator alone, and a second copy of the wait.
    assert_client(
        &with_in(CLIENT, REACH_BLOCK, ""),
        &["no step has `id: reach`"],
    );
    assert_client(
        &with_in(CLIENT, REACH_BLOCK, &format!("{REACH_BLOCK}{REACH_BLOCK}")),
        &["more than one step has `id: reach`"],
    );
}

#[test]
fn a_wait_that_does_not_run_exactly_with_the_join_is_refused() {
    // Catches: a wait that runs without a join (no tailnet to rejoin), that never runs, or whose
    // failure the job runs on past into the set.
    let no_if = "the wait step `reach` (step 10) does not have `if: ${{ inputs.join_tailnet }}`";
    let join_if = "        if: ${{ inputs.join_tailnet }}\n";
    for to in [
        "",
        "        if: ${{ always() }}\n",
        "        if: ${{ !inputs.join_tailnet }}\n",
    ] {
        assert_client(&reach_with(join_if, to), &[no_if]);
    }
    assert_client(
        &reach_with(
            join_if,
            &format!("{join_if}        continue-on-error: true\n"),
        ),
        &["step 10 (`reach`) has `continue-on-error`, which could run the job on past it"],
    );
}

#[test]
fn a_wait_out_of_its_place_is_refused() {
    // Catches: the wait before the join (nothing to wait for yet), before the CA it trusts, or
    // after the set it is there to make reachable.
    let before_join = moved(
        CLIENT,
        "      - id: reach\n",
        "      - id: request\n",
        "      - id: tailnet\n",
    );
    assert_client(
        &before_join,
        &[
            "the wait step `reach` (step 8) comes before the step `tailnet` (step 9)",
            "the wait step `reach` (step 8) comes before the step `ca` (step 10)",
        ],
    );
    let before_ca = moved(
        CLIENT,
        "      - id: reach\n",
        "      - id: request\n",
        "      - id: ca\n",
    );
    assert_client(
        &before_ca,
        &["the wait step `reach` (step 9) comes before the step `ca` (step 10)"],
    );
    let after_set = moved(
        CLIENT,
        "      - id: reach\n",
        "      - id: request\n",
        "      - id: wait\n",
    );
    assert_client(
        &after_set,
        &[
            "the wait step `reach` (step 12) comes after the OIDC token step `set-desired` (step 11)",
        ],
    );
}

#[test]
fn a_wait_that_is_not_the_one_command_is_refused() {
    // Catches: another program, another URL or CA than the calls', a token or answer flag, the
    // attempts given by a shell loop or chain, another verb, and output sent elsewhere.
    for (from, to) in [
        ("\"$RUNNER_TEMP/hk-release/hk\"", "hk"),
        (
            "--endpoint \"$COORDINATOR_URL\"",
            "--endpoint \"https://coordinator\"",
        ),
        (
            "--ca-file \"$RUNNER_TEMP/hk-coordinator-ca.pem\"",
            "--ca-file ca.pem",
        ),
        (
            "            tailnet-wait\n",
            "            tailnet-wait --attempts 1\n",
        ),
        (
            "            tailnet-wait\n",
            "            tailnet-wait || true\n",
        ),
        (
            "            tailnet-wait\n",
            "            tailnet-wait; tailnet-wait\n",
        ),
        (
            "            tailnet-wait\n",
            "            tailnet-wait > \"$GITHUB_OUTPUT\"\n",
        ),
        (
            "            tailnet-wait\n",
            "            releases status\n",
        ),
        (
            "            tailnet-wait\n",
            "            tailnet-wait\n          echo done\n",
        ),
        (
            "--endpoint",
            "--github-oidc-audience \"$AUDIENCE\" \\\n            --endpoint",
        ),
    ] {
        assert_client(&reach_with(from, to), &[NOT_ONE]);
    }
    assert_client(
        &reach_with("        run: |\n", "        run: |\n          sleep 30\n"),
        &[NOT_ONE],
    );
}

#[test]
fn a_wait_that_reads_another_environment_is_refused() {
    // Catches: the wait asking another address than the calls, and anything else handed to it.
    let refused = "the wait step `reach` (step 10) does not have exactly the `env` [(\"COORDINATOR_URL\", \"${{ inputs.coordinator_url }}\")]";
    let url = "          COORDINATOR_URL: ${{ inputs.coordinator_url }}\n";
    for to in [
        "          COORDINATOR_URL: ${{ inputs.tailnet_coordinator }}\n".to_owned(),
        format!("{url}          EXTRA: x\n"),
        "          OTHER: ${{ inputs.coordinator_url }}\n".to_owned(),
    ] {
        assert_client(&reach_with(url, &to), &[refused]);
    }
    assert_client(&reach_with(&format!("        env:\n{url}"), ""), &[refused]);
}
