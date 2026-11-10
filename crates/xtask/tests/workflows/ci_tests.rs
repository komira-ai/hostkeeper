//! The rules of `ci.yml`'s sharded tests and coverage gate ([`super::ci`]), each shown to refuse
//! its defect by putting it into the committed workflow and reading the exact problems it
//! reports; and the gate's conditions, read from the committed file and evaluated
//! ([`super::expr`]) against every result of the shards, in a run that was cancelled and one that
//! was not.

use std::collections::BTreeMap;

use super::ci::{
    BINARIES_ARTIFACT, BINARIES_DIR, BINARIES_PATH, CHECK, FILE, FIRST_SHARD_IF, GATE, GATE_IF,
    GATE_JOB, JOBS, LINT_JOB, PACK, PROFILE_FILE_NAME, PROFILES_ARTIFACT, PROFILES_DIR,
    PROFILES_PATH, PROFILES_PATTERN, REPORT, RUNNER, SHARD_FAILED_IF, SHARD_FAILED_RUN, SHARD_JOB,
    UNPACK, problems, steps, test_run,
};
use super::expr::{Context, holds};
use super::lint::entry;
use super::net_install_tests::replace_in_job;
use super::workflows;
use super::yaml::{self, Node};

/// The committed ci.yml.
fn committed() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == FILE)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{FILE} is in the repository"))
}

fn assert_problems(text: &str, want: &[&str]) {
    assert_eq!(problems(text), want, "in:\n{text}");
}

/// The committed ci.yml with `from` replaced by `to` in the job `job`.
fn edited(job: &str, from: &str, to: &str) -> String {
    replace_in_job(&committed(), job, from, to)
}

const SHARD: &str = "job test-shard";
const COVERAGE: &str = "job coverage";

fn strategy_problem() -> String {
    format!("{SHARD}: strategy must be exactly fail-fast: false and the matrix shard: [1, 2]")
}

fn test_step_problem() -> String {
    format!("{SHARD}: the test step must run `{}`", test_run())
}

/// Proves: the committed ci.yml keeps every rule of its shards and gate, and its jobs are the
/// three of [`JOBS`], the gate's check being [`CHECK`].
/// Catches: a rule that refuses the committed workflow, so every case below would fail for that
/// reason and not for the defect it adds.
#[test]
fn the_committed_ci_yml_keeps_the_shard_and_gate_rules() {
    assert_problems(&committed(), &[]);
    let doc = yaml::load(&committed()).expect("load");
    let gate = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, GATE_JOB))
        .expect("the gate job");
    assert_eq!(entry(gate, "name").and_then(Node::as_str), Some(CHECK));
    let jobs: Vec<&str> = entry(&doc, "jobs")
        .map(super::lint::entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(jobs, JOBS);
}

/// Proves: a matrix that leaves a half of the tests out (`[1]`), adds a shard the partition does
/// not have (`[1, 2, 3]`), lists the shards in another order, adds a second matrix key or an
/// `include`, stops at the first failing shard (`fail-fast: true`, or no `fail-fast`, whose
/// default is true), or has no strategy at all, is refused with one message.
/// Catches: tests that no shard runs while the gate passes on the rest, and a run that hides the
/// other shard's failures.
#[test]
fn a_matrix_other_than_every_shard_without_fail_fast_is_refused() {
    let matrix = "        shard: [1, 2]\n";
    for bad in [
        "        shard: [1]\n",
        "        shard: [1, 2, 3]\n",
        "        shard: [2, 1]\n",
        "        shard: [1, 2]\n        os: [a]\n",
        "        shard: [1, 2]\n        include:\n          - shard: 3\n",
        "        shard: ['1', x]\n",
        "        shard: ${{ fromJSON('[1, 2]') }}\n",
    ] {
        assert_problems(&edited(SHARD_JOB, matrix, bad), &[&strategy_problem()]);
    }
    for bad in ["      fail-fast: true\n", ""] {
        assert_problems(
            &edited(SHARD_JOB, "      fail-fast: false\n", bad),
            &[&strategy_problem()],
        );
    }
    let no_strategy = edited(
        SHARD_JOB,
        "    strategy:\n      fail-fast: false\n      matrix:\n        shard: [1, 2]\n",
        "",
    );
    assert_problems(&no_strategy, &[&strategy_problem()]);
    assert_problems(
        &edited(
            SHARD_JOB,
            "      fail-fast: false\n",
            "      fail-fast: false\n      max-parallel: 1\n",
        ),
        &[&strategy_problem()],
    );
}

/// Proves: a test step with another partition (a fixed shard, another count, a count-based
/// split), with a filter that leaves tests out, or without `--no-fail-fast` is refused; so are no
/// test step and two of them; and a test step that leaves `LLVM_PROFILE_FILE_NAME` out or names
/// the profiles the same in every shard.
/// Catches: a matrix whose shards run the same half or skip tests, profiles of the two shards
/// that overwrite each other when they are put together, and a run stopping at its first failure.
#[test]
fn a_test_step_other_than_this_shards_half_of_every_test_is_refused() {
    let partition = "--partition hash:${{ matrix.shard }}/2";
    for bad in [
        "--partition hash:1/2",
        "--partition hash:${{ matrix.shard }}/3",
        "--partition count:${{ matrix.shard }}/2",
        "--partition hash:${{ matrix.shard }}/2 -E 'not test(slow)'",
        "",
    ] {
        assert_problems(&edited(SHARD_JOB, partition, bad), &[&test_step_problem()]);
    }
    assert_problems(
        &edited(SHARD_JOB, " --no-fail-fast --partition", " --partition"),
        &[&test_step_problem()],
    );
    let name = format!("LLVM_PROFILE_FILE_NAME: {PROFILE_FILE_NAME}");
    let name_problem =
        format!("{SHARD}: the test step must set LLVM_PROFILE_FILE_NAME to `{PROFILE_FILE_NAME}`");
    for bad in [
        "LLVM_PROFILE_FILE_NAME: hostkeeper-%p-%4m.profraw",
        "OTHER: x",
    ] {
        assert_problems(&edited(SHARD_JOB, &name, bad), &[&name_problem]);
    }
    let run = format!("        run: {}\n", test_run());
    let twice = edited(
        SHARD_JOB,
        &run,
        &format!("{run}      - run: cargo llvm-cov nextest --workspace\n"),
    );
    assert_problems(
        &twice,
        &[&format!("{SHARD}: 2 steps run llvm-cov nextest, not one")],
    );
    let none = edited(SHARD_JOB, &run, "        run: cargo test --workspace\n");
    assert_problems(
        &none,
        &[&format!("{SHARD}: 0 steps run llvm-cov nextest, not one")],
    );
}

/// Proves: a shard or a gate on another runner than [`RUNNER`] (`ubuntu-latest`, which GitHub
/// moves to a new image, or another fixed one) is refused, and so is a shard with no Postgres.
/// Catches: binaries built on one image read on another, and a shard whose coordinator store
/// tests fail for want of a server.
#[test]
fn shards_and_gate_off_the_one_runner_and_a_shard_without_postgres_are_refused() {
    let runner = format!("    runs-on: {RUNNER}\n");
    for (job, who) in [(SHARD_JOB, SHARD), (GATE_JOB, COVERAGE)] {
        for bad in ["ubuntu-latest", "ubuntu-22.04"] {
            assert_problems(
                &edited(job, &runner, &format!("    runs-on: {bad}\n")),
                &[&format!(
                    "{who}: runs-on must be {RUNNER}, the one image of the shards and the gate"
                )],
            );
        }
    }
    assert_problems(
        &edited(SHARD_JOB, "      postgres:\n", "      postgresql:\n"),
        &[&format!("{SHARD}: no service postgres")],
    );
}

/// Proves: profiles uploaded under another name or from another path, or before the tests ran;
/// a pack with another command, without the first shard's `if:`, or missing; and an upload of
/// the pack under another name or not after the pack, are each refused with their own message.
/// Catches: a hand-over the gate's downloads do not find, which would make a correct pull
/// request red or measure one shard alone.
#[test]
fn a_shard_hand_over_the_gate_does_not_find_is_refused() {
    let profiles =
        format!("{SHARD}: no upload of {PROFILES_PATH} as {PROFILES_ARTIFACT} after the test step");
    for (from, to) in [
        (
            "name: coverage-profiles-${{ matrix.shard }}",
            "name: coverage-profiles",
        ),
        (
            "path: target/llvm-cov-target/*.profraw",
            "path: target/*.profraw",
        ),
    ] {
        assert_problems(&edited(SHARD_JOB, from, to), &[&profiles]);
    }
    let pack = format!("{SHARD}: no step `{PACK}` under `{FIRST_SHARD_IF}` after the test step");
    let binaries = format!(
        "{SHARD}: no upload of {BINARIES_PATH} as {BINARIES_ARTIFACT} under `{FIRST_SHARD_IF}` after the pack"
    );
    let pack_step = format!("        if: {FIRST_SHARD_IF}\n        run: {PACK}\n");
    for bad in [
        format!("        run: {PACK}\n"),
        format!(
            "        if: {FIRST_SHARD_IF}\n        run: {}\n",
            PACK.replace(" --exclude='*.*'", "")
        ),
        format!("        if: ${{{{ matrix.shard == 2 }}}}\n        run: {PACK}\n"),
    ] {
        assert_problems(&edited(SHARD_JOB, &pack_step, &bad), &[&pack, &binaries]);
    }
    assert_problems(
        &edited(SHARD_JOB, "name: coverage-binaries", "name: binaries"),
        &[&binaries],
    );
    // The profiles' upload moved before the test step: the same steps in another order.
    let text = committed();
    let upload = "      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: coverage-profiles-${{ matrix.shard }}\n          path: target/llvm-cov-target/*.profraw\n          if-no-files-found: error\n          compression-level: 1\n          retention-days: 1\n";
    let test = "      - name: Run this shard's half of the tests under coverage\n";
    let moved = replace_in_job(
        &replace_in_job(&text, SHARD_JOB, upload, ""),
        SHARD_JOB,
        test,
        &format!("{upload}{test}"),
    );
    assert_problems(&moved, &[&profiles]);
}

/// Proves: a gate not named [`CHECK`], one that needs another job too or not the shards, one
/// under another `if:` (the default, `!cancelled()`, `success()`), and one with a matrix are each
/// refused.
/// Catches: a required check `test` that no job reports any more, or that a shard's failure or a
/// cancelled run skips (a skipped required check passes).
#[test]
fn a_gate_with_another_name_needs_or_condition_is_refused() {
    let name = format!("    name: {CHECK}\n");
    let named = format!("{COVERAGE}: name must be {CHECK}, the required check");
    for bad in ["    name: coverage\n", ""] {
        assert_problems(&edited(GATE_JOB, &name, bad), &[&named]);
    }
    let needs = "    needs: [test-shard]\n";
    assert_problems(&edited(GATE_JOB, needs, "    needs: test-shard\n"), &[]);
    let needs_problem = format!("{COVERAGE}: needs must be [{SHARD_JOB}] alone");
    for bad in ["    needs: [lint, test-shard]\n", "    needs: [lint]\n", ""] {
        assert_problems(&edited(GATE_JOB, needs, bad), &[&needs_problem]);
    }
    let condition = format!("    if: {GATE_IF}\n");
    let if_problem = format!("{COVERAGE}: if must be {GATE_IF}");
    for bad in [
        "",
        "    if: ${{ !cancelled() }}\n",
        "    if: ${{ success() }}\n",
    ] {
        assert_problems(&edited(GATE_JOB, &condition, bad), &[&if_problem]);
    }
    assert_problems(
        &edited(
            GATE_JOB,
            &condition,
            &format!("{condition}    strategy:\n      matrix:\n        a: [1]\n"),
        ),
        &[&format!(
            "{COVERAGE}: has a strategy, so it is more than one check"
        )],
    );
}

/// Proves: a gate whose first step is missing, passes (`exit 0`), or fails on fewer results than
/// every one but `success` (`== 'failure'` lets a cancelled or skipped shard through; without
/// `always()` it never runs after a cancellation) is refused; so is that step moved after the
/// checkout.
/// Catches: a gate that reports coverage of the shards that ran and passes while one failed.
#[test]
fn a_gate_that_does_not_first_fail_on_a_shard_that_did_not_pass_is_refused() {
    let step = format!(
        "      - name: A test shard did not pass\n        if: {SHARD_FAILED_IF}\n        run: {SHARD_FAILED_RUN}\n"
    );
    let first = format!(
        "{COVERAGE}: its first step must be `{SHARD_FAILED_RUN}` under `{SHARD_FAILED_IF}`"
    );
    for bad in [
        String::new(),
        step.replace("exit 1", "exit 0"),
        step.replace("!= 'success'", "== 'failure'"),
        step.replace("always() && ", ""),
    ] {
        assert_problems(&edited(GATE_JOB, &step, &bad), &[&first]);
    }
    let checkout = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          persist-credentials: false\n";
    let moved = edited(
        GATE_JOB,
        &format!("{step}{checkout}"),
        &format!("{checkout}{step}"),
    );
    assert_problems(&moved, &[&first]);
}

/// Proves: a report or a gate other than the commands of the one job before the shards (no
/// `--branch`, another ignore pattern, another lcov path, no summary) is refused, as is a gate
/// with no report before it; and a gate that does not download every shard's profiles into the
/// directory the report reads (one shard's artifact, no `merge-multiple`, another directory) or
/// does not unpack the binaries before the report.
/// Catches: a gate that measures something else than the merged run, or applies another floor.
#[test]
fn a_gate_that_reports_or_gates_otherwise_or_reads_no_merged_run_is_refused() {
    let report = format!("{COVERAGE}: no step runs `{REPORT}`");
    let gate = format!("{COVERAGE}: no step runs `{GATE}` after the report");
    for bad in [
        REPORT.replace(" --branch", ""),
        REPORT.replace("|benches)", "|benches|src)"),
        REPORT.replace("target/coverage/lcov.info", "lcov.info"),
    ] {
        assert_problems(&edited(GATE_JOB, REPORT, &bad), &[&report]);
    }
    let summary = r#" --summary-file "$GITHUB_STEP_SUMMARY""#;
    assert_problems(&edited(GATE_JOB, summary, ""), &[&gate]);
    assert_problems(
        &edited(
            GATE_JOB,
            "--lcov target/coverage/lcov.info --root",
            "--lcov target/other/lcov.info --root",
        ),
        &[&gate],
    );
    let profiles = format!(
        "{COVERAGE}: no download of {PROFILES_PATTERN} (merge-multiple) into {PROFILES_DIR} before the report"
    );
    for (from, to) in [
        (
            "pattern: coverage-profiles-*",
            "pattern: coverage-profiles-1",
        ),
        ("merge-multiple: true", "merge-multiple: false"),
        (
            "          path: target/llvm-cov-target\n",
            "          path: target\n",
        ),
    ] {
        assert_problems(&edited(GATE_JOB, from, to), &[&profiles]);
    }
    let binaries = format!(
        "{COVERAGE}: no download of {BINARIES_ARTIFACT} into {BINARIES_DIR}, then `{UNPACK}`, before the report"
    );
    for (from, to) in [
        (
            UNPACK,
            "tar --list --zstd --file \"$RUNNER_TEMP/coverage-binaries.tar.zst\"",
        ),
        ("name: coverage-binaries", "name: binaries"),
    ] {
        assert_problems(&edited(GATE_JOB, from, to), &[&binaries]);
    }
}

/// Proves: `continue-on-error` on a shard, on its test step, on the gate or on its gate step is
/// refused, naming where.
/// Catches: a failing test or a crate under 99% that the run reports as a pass.
#[test]
fn continue_on_error_in_a_shard_or_the_gate_is_refused() {
    let run = format!("        run: {}\n", test_run());
    assert_problems(
        &edited(
            SHARD_JOB,
            &run,
            &format!("{run}        continue-on-error: true\n"),
        ),
        &[&format!("{SHARD}: continue-on-error is set on step 16")],
    );
    let runner = format!("    runs-on: {RUNNER}\n");
    assert_problems(
        &edited(
            SHARD_JOB,
            &runner,
            &format!("{runner}    continue-on-error: true\n"),
        ),
        &[&format!("{SHARD}: continue-on-error is set on the job")],
    );
    assert_problems(
        &edited(
            GATE_JOB,
            &runner,
            &format!("{runner}    continue-on-error: true\n"),
        ),
        &[&format!("{COVERAGE}: continue-on-error is set on the job")],
    );
    let gate = format!("        run: {GATE}\n");
    assert_problems(
        &edited(
            GATE_JOB,
            &gate,
            &format!("{gate}        continue-on-error: true\n"),
        ),
        &[&format!("{COVERAGE}: continue-on-error is set on step 15")],
    );
}

/// Proves: a `shell` or a `working-directory` on a step of a shard or of the gate, `defaults` on
/// either job, and `defaults` on the workflow are each refused, naming where.
/// Catches: a pinned `run` made to do nothing, such as the gate's `exit 1` under `shell: echo {0}`
/// (a failed shard then passes) or the 99% gate run in another directory.
#[test]
fn a_shell_working_directory_or_defaults_that_changes_a_pinned_run_is_refused() {
    let first = format!("        run: {SHARD_FAILED_RUN}\n");
    assert_problems(
        &edited(
            GATE_JOB,
            &first,
            &format!("{first}        shell: echo {{0}}\n"),
        ),
        &[&format!("{COVERAGE}: shell is set on step 1")],
    );
    let gate = format!("        run: {GATE}\n");
    assert_problems(
        &edited(
            GATE_JOB,
            &gate,
            &format!("{gate}        working-directory: crates\n"),
        ),
        &[&format!("{COVERAGE}: working-directory is set on step 15")],
    );
    let run = format!("        run: {}\n", test_run());
    assert_problems(
        &edited(
            SHARD_JOB,
            &run,
            &format!("{run}        shell: sh -c 'exit 0' {{0}}\n"),
        ),
        &[&format!("{SHARD}: shell is set on step 16")],
    );
    let runner = format!("    runs-on: {RUNNER}\n");
    let defaults = "    defaults:\n      run:\n        shell: echo {0}\n";
    for (job, who) in [(SHARD_JOB, SHARD), (GATE_JOB, COVERAGE)] {
        assert_problems(
            &edited(job, &runner, &format!("{runner}{defaults}")),
            &[&format!("{who}: defaults is set on the job")],
        );
    }
    let top = with_top_defaults(&committed());
    assert_problems(
        &top,
        &["defaults is set on the workflow, which changes how every step's run runs"],
    );
}

/// `text` with a top-level `defaults` that runs every step under `echo`.
fn with_top_defaults(text: &str) -> String {
    let at = "\npermissions: {}\n";
    assert_eq!(text.matches(at).count(), 1);
    text.replacen(
        at,
        "\npermissions: {}\ndefaults:\n  run:\n    shell: echo {0}\n",
        1,
    )
}

/// Proves: a job added, removed, renamed or moved is refused, naming the jobs found; another job
/// with the check name [`CHECK`] (by its `name` or its id) is refused; and a file that is not
/// YAML is reported as such.
/// Catches: a second check `test` that could pass in place of the gate, and a gate or shard job
/// the rules no longer read.
#[test]
fn other_jobs_another_check_named_test_and_not_yaml_are_refused() {
    let text = committed();
    let renamed = text.replacen("\n  coverage:\n", "\n  gate:\n", 1);
    assert_problems(
        &renamed,
        &[
            r#"jobs are ["lint", "test-shard", "gate"], not exactly ["lint", "test-shard", "coverage"] in that order"#,
            "job gate: its check is named test, the check of coverage alone",
        ],
    );
    let added =
        format!("{text}\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: exit 0\n");
    assert_problems(
        &added,
        &[
            r#"jobs are ["lint", "test-shard", "coverage", "test"], not exactly ["lint", "test-shard", "coverage"] in that order"#,
            "job test: its check is named test, the check of coverage alone",
        ],
    );
    let lint_named = replace_in_job(
        &text,
        LINT_JOB,
        "  lint:\n    runs-on:",
        "  lint:\n    name: test\n    runs-on:",
    );
    assert_problems(
        &lint_named,
        &["job lint: its check is named test, the check of coverage alone"],
    );
    let bad = problems("jobs: [");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("not valid YAML"), "{bad:?}");
}

/// The conditions of a gate: the job's `if:` (`None`: GitHub's default, `success()`), the first
/// step's, and the gate step's (`None`: `success()`).
struct Conditions {
    job: Option<String>,
    first: Option<String>,
    gate: Option<String>,
}

fn conditions(text: &str) -> Conditions {
    let doc = yaml::load(text).expect("load");
    let job = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, GATE_JOB))
        .expect("the gate job");
    let steps = steps(job);
    let if_of = |node: &Node| entry(node, "if").and_then(Node::as_str).map(str::to_owned);
    let gate = steps
        .iter()
        .find(|s| entry(s, "run").and_then(Node::as_str) == Some(GATE))
        .expect("the gate step");
    Conditions {
        job: if_of(job),
        first: steps.first().and_then(if_of),
        gate: if_of(gate),
    }
}

/// The results GitHub gives a matrix job in `needs`: `success` only when every shard passed.
const RESULTS: [&str; 4] = ["success", "failure", "cancelled", "skipped"];

/// Whether a step under `condition` runs: one with no status function (`always()`,
/// `cancelled()`), or none, is GitHub's `success() && (...)`, false in a cancelled run and after
/// a failed step.
fn step_runs(condition: Option<&str>, context: &Context, failed: bool) -> bool {
    match condition {
        None => !context.cancelled && !failed,
        Some(c) => {
            let status = c.contains("always()") || c.contains("cancelled()");
            (status || (!context.cancelled && !failed)) && holds(c, context).expect("a step's if")
        }
    }
}

/// Whether the check passes when the shards end as `result` in a run that is `cancelled` or not:
/// a skipped gate passes (a skipped required check does); one that runs passes when its first
/// step did not fail and its gate step ran (and found every crate at 99%).
fn passes(c: &Conditions, result: &str, cancelled: bool) -> bool {
    let context = Context {
        names: BTreeMap::from([("needs.test-shard.result".to_owned(), result.to_owned())]),
        cancelled,
        failed: false,
    };
    let runs = match &c.job {
        // GitHub's default: success() of the needs, false in a cancelled run.
        None => !cancelled && result == "success",
        Some(condition) => holds(condition, &context).expect("the gate's if"),
    };
    if !runs {
        return true;
    }
    let failed = c
        .first
        .as_deref()
        .is_some_and(|first| step_runs(Some(first), &context, false));
    !failed && step_runs(c.gate.as_deref(), &context, failed)
}

/// The `(result, cancelled)` outcomes in which the check of `text` passes.
fn passing(text: &str) -> Vec<(&'static str, bool)> {
    let c = conditions(text);
    let mut found = Vec::new();
    for cancelled in [false, true] {
        for result in RESULTS {
            if passes(&c, result, cancelled) {
                found.push((result, cancelled));
            }
        }
    }
    found
}

/// Proves: over every result of the shards (`success`, `failure`, `cancelled`, `skipped`), in a
/// run cancelled and one not, the committed gate runs every time and its check passes in exactly
/// one outcome: every shard passed and the run was not cancelled. The same evaluation of the
/// gate under `if: ${{ !cancelled() }}`, or under no `if:` (GitHub's default), finds the check
/// passing as a skipped job: in a cancelled run for the first, and also when a shard failed,
/// was cancelled or skipped for the second.
/// Catches: a gate that a failed, cancelled or skipped shard, or a cancelled run, leaves passing.
#[test]
fn the_gate_check_passes_only_when_every_shard_passed_in_a_run_not_cancelled() {
    let text = committed();
    let c = conditions(&text);
    for cancelled in [false, true] {
        for result in RESULTS {
            let context = Context {
                names: BTreeMap::from([("needs.test-shard.result".to_owned(), result.to_owned())]),
                cancelled,
                failed: false,
            };
            assert!(
                holds(c.job.as_deref().expect("an if"), &context).expect("the gate's if"),
                "the gate is skipped on {result}, cancelled {cancelled}"
            );
        }
    }
    assert_eq!(passing(&text), [("success", false)]);
    let condition = format!("    if: {GATE_IF}\n");
    let not_cancelled =
        replace_in_job(&text, GATE_JOB, &condition, "    if: ${{ !cancelled() }}\n");
    assert_eq!(
        passing(&not_cancelled),
        [
            ("success", false),
            ("success", true),
            ("failure", true),
            ("cancelled", true),
            ("skipped", true),
        ]
    );
    let default = replace_in_job(&text, GATE_JOB, &condition, "");
    assert_eq!(
        passing(&default),
        [
            ("success", false),
            ("failure", false),
            ("cancelled", false),
            ("skipped", false),
            ("success", true),
            ("failure", true),
            ("cancelled", true),
            ("skipped", true),
        ]
    );
}
