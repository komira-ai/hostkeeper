//! The rules of `farm.yml` ([`super::farm`]), the farm lane: the one job that holds
//! `id-token: write` on a pull request, and only under its exact `if:`; farm-connect and
//! buck2-farm pinned by commit in that file alone, one komira commit for both and for the tool's
//! sources; farm-connect with exactly the variables it is given; the triggers, the runner, the
//! rights; every buck2 build on the farm through buck2-farm, and no other buck2 command but the
//! three that build nothing on it; the checks and builds in their order after farm-connect; the
//! tool built from komira's sources before the builds; the daemon's stop after the build; and the
//! upload of a stale table's patch after a failure. Each rule is shown to refuse its defect by
//! putting it into the committed workflow and reading the exact problems the lint reports.

use super::farm::{
    BUCK2_FARM, CI_ARGS, COMPILE_RUN, CONNECT_INPUTS, FARM_CONNECT, FILE, JOB_IF, KILL_IF,
    ON_PROBLEM, ORDER_PROBLEM, PATCH_ARTIFACT, PINS_PROBLEM, RUNNER, TABLE_RUN, TOOL_ARGS,
    UPLOAD_IF, WORKERS_RUN, farm_steps_problem, kill_problem, names_buck2, outside_problem,
    patch_file, tool_problem, upload_inputs,
};
use super::lint::lint;
use super::{with_in, workflows};

/// The committed farm.yml.
fn committed() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == FILE)
        .expect("farm.yml is in the repository")
        .1
}

/// The problems of the committed farm.yml with the one occurrence of `from` replaced by `to`.
fn problems_with(from: &str, to: &str) -> Vec<String> {
    lint(FILE, &with_in(&committed(), from, to))
}

/// The line (from 1) of the committed farm.yml that holds `needle`.
fn line_of(needle: &str) -> usize {
    committed()
        .lines()
        .position(|l| l.contains(needle))
        .expect("the text is in farm.yml")
        + 1
}

/// The problem of the permissions of the job `build` that are not exactly its two.
const PERMISSIONS: &str =
    "job build: permissions must be exactly contents: read and id-token: write";

#[test]
fn the_committed_farm_yml_passes_every_rule() {
    // Catches: a farm.yml that the rules below would refuse, or rules that refuse the file they
    // were written for.
    assert_eq!(lint(FILE, &committed()), Vec::<String>::new());
}

#[test]
fn the_token_is_held_only_under_the_jobs_exact_if() {
    // Catches: the job's `if:` loosened (a fork's pull request let in, Dependabot's branches let
    // in, the condition dropped) while it still holds `id-token: write`.
    let wrong = [
        "${{ github.event_name == 'push' || github.event_name == 'pull_request' }}",
        "${{ github.event_name == 'push' || github.event.pull_request.head.repo.full_name == github.repository }}",
        "always()",
    ];
    let dropped = format!("    if: {JOB_IF}\n");
    assert_eq!(
        problems_with(&dropped, ""),
        [format!(
            "job build: `id-token: write` needs the exact `if: {JOB_IF}`"
        )],
        "no if:"
    );
    for condition in wrong {
        assert_eq!(
            problems_with(JOB_IF, condition),
            [format!(
                "job build: `id-token: write` needs the exact `if: {JOB_IF}`"
            )],
            "{condition}"
        );
    }
}

#[test]
fn the_token_in_another_job_of_the_file_is_refused() {
    // Catches: the exception keyed on the file alone, so that any job of farm.yml, or a second
    // job beside `build`, could hold the token on a pull request.
    assert_eq!(
        problems_with("\n  build:\n", "\n  lane:\n"),
        [
            "job lane: `id-token: write` is not allowed in farm.yml",
            "job lane: privileged on pull_request (`id-token: write`)",
            "farm.yml: no job build",
            "job lane is not a job of farm.yml",
        ]
    );
}

#[test]
fn the_same_job_in_another_pull_request_workflow_is_refused() {
    // Catches: the exception keyed on the job's name alone, so that a job `build` of ci.yml (or
    // any other workflow that runs for a pull request) could hold the token and pin farm-connect
    // or buck2-farm with no version comment.
    let found = lint("ci.yml", &committed());
    let connect = line_of(FARM_CONNECT);
    let farm = line_of(BUCK2_FARM);
    for want in [
        "job build: `id-token: write` is not allowed in ci.yml".to_owned(),
        "job build: privileged on pull_request (`id-token: write`)".to_owned(),
        format!(
            "line {connect}: `uses: {FARM_CONNECT}@b2080fe989248adf9b47d4bd14c0ecefa0802f79` has no version comment (`# v1.2.3`) on its line"
        ),
        format!(
            "line {farm}: `uses: {BUCK2_FARM}@b2080fe989248adf9b47d4bd14c0ecefa0802f79` has no version comment (`# v1.2.3`) on its line"
        ),
    ] {
        assert!(found.contains(&want), "{want} is not in {found:#?}");
    }
}

#[test]
fn no_other_write_scope_is_allowed_in_the_job() {
    // Catches: a write scope beside the token (the repository's contents, issues) let into a job
    // that runs pull-request code, or the rights read as allowed because the token is.
    assert_eq!(
        problems_with("      contents: read\n", "      contents: write\n"),
        [
            "job build: `contents: write` is not allowed in farm.yml",
            "job build: privileged on pull_request (`contents: write`)",
            PERMISSIONS,
        ]
    );
    assert_eq!(
        problems_with(
            "      contents: read\n",
            "      contents: read\n      actions: read\n"
        ),
        [PERMISSIONS]
    );
    assert_eq!(
        problems_with(
            "    permissions:\n      contents: read\n      # For farm-connect's tailnet join alone: the OIDC token the farm's network trusts.\n      id-token: write\n",
            "    permissions: read-all\n"
        ),
        [
            "job build: `permissions` is not a mapping of scopes",
            PERMISSIONS
        ]
    );
}

#[test]
fn only_farm_connect_goes_without_a_version_comment_and_only_pinned() {
    // Catches: the exception to the version comment widened to every action of farm.yml, or to
    // farm-connect at a branch instead of a commit (which is then also not the commit of
    // buck2-farm and of the tool's sources).
    let checkout = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
    assert_eq!(
        problems_with(
            &format!("- uses: {checkout} # v7.0.1"),
            &format!("- uses: {checkout}")
        ),
        [format!(
            "line {}: `uses: {checkout}` has no version comment (`# v1.2.3`) on its line",
            line_of(checkout)
        )]
    );
    let unpinned = format!("{FARM_CONNECT}@main");
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@b2080fe989248adf9b47d4bd14c0ecefa0802f79"),
            &unpinned
        ),
        [
            format!(
                "line {}: `uses: {unpinned}` is not pinned to a 40-digit lowercase commit SHA",
                line_of(FARM_CONNECT)
            ),
            PINS_PROBLEM.to_owned(),
        ]
    );
}

#[test]
fn farm_connect_takes_exactly_its_variables() {
    // Catches: an input added (a header, another platform's workers), one taken from somewhere
    // a pull request controls instead of the organisation's variable of its name, one missing,
    // or one that is not a plain value.
    let inputs =
        format!("job build: the farm-connect step's `with:` must be exactly {CONNECT_INPUTS:?}");
    let tls = "          tls: ${{ vars.FARM_TLS || 'false' }}\n";
    let cases = [
        format!("{tls}          farm-qos: batch\n"),
        "          tls: ${{ github.event.pull_request.title }}\n".to_owned(),
        String::new(),
        "          tls: [true]\n".to_owned(),
    ];
    for to in cases {
        assert_eq!(problems_with(tls, &to), [inputs.as_str()], "{to}");
    }
}

#[test]
fn exactly_one_step_uses_farm_connect() {
    // Catches: the farm reached by some other join than the reviewed action, whose inputs the
    // rule above would then not see.
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@b2080fe989248adf9b47d4bd14c0ecefa0802f79"),
            "example/join@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v1.0.0"
        ),
        [
            format!("job build: exactly one step must use {FARM_CONNECT}"),
            ORDER_PROBLEM.to_owned(),
        ]
    );
    // A name that only starts with farm-connect's is another action: it needs a version comment,
    // and the step is no farm-connect step.
    let lookalike = format!("{FARM_CONNECT}-fork@b2080fe989248adf9b47d4bd14c0ecefa0802f79");
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@b2080fe989248adf9b47d4bd14c0ecefa0802f79"),
            &lookalike
        ),
        [
            format!(
                "line {}: `uses: {lookalike}` has no version comment (`# v1.2.3`) on its line",
                line_of(FARM_CONNECT)
            ),
            format!("job build: exactly one step must use {FARM_CONNECT}"),
            ORDER_PROBLEM.to_owned(),
        ]
    );
}

/// The step of the committed file that builds `//:ci`.
const CI_STEP: &str = "      - name: buck2 build //:ci on the farm, local 0
        timeout-minutes: 150
        uses: komira-ai/komira/.github/actions/buck2-farm@b2080fe989248adf9b47d4bd14c0ecefa0802f79
        with:
          buck2: ${{ runner.temp }}/bin/buck2
          assert-remote: ${{ runner.temp }}/bin/assert_remote
          args: build //:ci
";

/// The step of the committed file that checks the worker property set.
const WORKERS_STEP: &str = "      - name: buck2 has the farm's workers
        timeout-minutes: 10
        run: cargo xtask farm-workers --buck2 \"$RUNNER_TEMP/bin/buck2\"
";

/// The step of the committed file that builds the tool.
const COMPILE_STEP: &str = "      - name: Build assert_remote, the buck2-farm action's tool
        timeout-minutes: 15
";

/// The step of the committed file that stops buck2's daemon.
const KILL_STEP: &str = "      - name: Stop buck2's daemon
        if: ${{ always() && steps.buck2-check.outcome == 'success' }}
        timeout-minutes: 5
        run: '\"$RUNNER_TEMP/bin/buck2\" kill'
";

/// The committed file with the step text `step` taken out and put back before the line
/// `before`.
fn moved(step: &str, before: &str) -> String {
    with_in(&committed(), step, "").replacen(before, &format!("{step}{before}"), 1)
}

#[test]
fn the_committed_steps_are_the_ones_the_rules_name() {
    // Catches: tests below that change a step the rules do not read, because the committed text
    // and the rules' constants drifted apart.
    let text = committed();
    for (step, want) in [
        (CI_STEP, CI_ARGS),
        (WORKERS_STEP, WORKERS_RUN),
        (KILL_STEP, KILL_IF),
    ] {
        assert!(text.contains(step), "{step}");
        assert!(step.contains(want), "{want}");
    }
    assert!(text.contains(&format!("{COMPILE_STEP}        run: '{COMPILE_RUN}'\n")));
    assert!(text.contains(&format!("          args: {TOOL_ARGS}\n")));
    assert!(text.contains(&format!("        run: {TABLE_RUN}\n")));
}

#[test]
fn the_builds_go_through_buck2_farm_with_exactly_their_inputs() {
    // Catches: a build of something other than `//:ci` or the buildrules command line through the
    // action, one without the pinned buck2 or the built tool (the action would run `./buck2`,
    // which this repository has not, or no assert_remote), and a third buck2-farm step the rules
    // would not read.
    let farm = farm_steps_problem();
    let twice = CI_STEP.repeat(2);
    for (from, to) in [
        (
            "          args: build //:ci\n",
            "          args: build //...\n",
        ),
        (
            "--out ${{ runner.temp }}/buildrules-cli\n",
            "--out ${{ runner.temp }}/elsewhere\n",
        ),
        (
            "          assert-remote: ${{ runner.temp }}/bin/assert_remote\n          args: build //:ci\n",
            "          args: build //:ci\n",
        ),
        (CI_STEP, twice.as_str()),
    ] {
        assert_eq!(problems_with(from, to), [farm.as_str()], "{to:?}");
    }
}

#[test]
fn a_buck2_command_outside_the_action_is_refused() {
    // Catches: the farm build run by buck2 itself in a `run:`, where no watchdog ends a run hung
    // on a dead connection, whether it replaces the action's step or sits beside it, after other
    // words of its command, or as the buck2 a verb is handed; and a check whose `run:` changed,
    // which the rules no longer know to build nothing on the farm.
    let plain = "\"$RUNNER_TEMP/bin/buck2\" build //:ci";
    let replaced = format!(
        "      - name: buck2 build //:ci on the farm, local 0\n        timeout-minutes: 150\n        run: '{plain}'\n"
    );
    assert_eq!(
        problems_with(CI_STEP, &replaced),
        [farm_steps_problem(), outside_problem(plain)]
    );
    for run in [
        "env BUCK_CONSOLE=simple buck2 build //:ci",
        "cd \"$GITHUB_WORKSPACE\" && buck2 build //:ci",
        "true; \"$RUNNER_TEMP/bin/buck2\" test //:ci",
    ] {
        let beside = format!("{CI_STEP}      - timeout-minutes: 150\n        run: '{run}'\n");
        assert_eq!(
            problems_with(CI_STEP, &beside),
            [outside_problem(run)],
            "{run}"
        );
    }
    let table = TABLE_RUN.replace("--repo \"$GITHUB_REPOSITORY\"", "--repo someone/else");
    assert_eq!(
        problems_with(TABLE_RUN, &table),
        [ORDER_PROBLEM.to_owned(), outside_problem(&table)]
    );
}

#[test]
fn buck2_is_named_by_its_word_or_path_but_not_as_where_it_is_installed() {
    // Catches: a reading of a script that misses buck2 behind a path or a variable, or calls the
    // pinned buck2's install a buck2 command and so refuses the job's own install step.
    for run in [
        "buck2 build //:ci",
        "\"$RUNNER_TEMP/bin/buck2\" kill",
        "cargo xtask farm-table --buck2 \"$RUNNER_TEMP/bin/buck2\"",
        "cd x && ./buck2 build //:ci",
        "env A=1 /usr/local/bin/buck2 bxl //:x",
    ] {
        assert!(names_buck2(run), "{run}");
    }
    for run in [
        "cargo xtask install-tool --download \"$RUNNER_TEMP/buck2.zst\" --sha256 \"$BUCK2_SHA256\" --out \"$RUNNER_TEMP/bin/buck2\" --extract zst",
        "echo \"::error::buck2.zst is not in the actions cache\"",
        "curl --output \"$RUNNER_TEMP/buck2.zst\" https://example.com/buck2-x86_64.zst",
        "cargo xtask --help",
    ] {
        assert!(!names_buck2(run), "{run}");
    }
}

#[test]
fn the_checks_and_builds_run_in_their_order_after_farm_connect() {
    // Catches: the worker check after a build (which then builds with no farm workers), or gone;
    // the table check before the build of its command line, or after the build of `//:ci`; and a
    // build before farm-connect, which would build on the runner.
    let order = [ORDER_PROBLEM];
    assert_eq!(
        lint(
            FILE,
            &moved(
                WORKERS_STEP,
                "      - name: The generated build table is fresh\n"
            )
        ),
        order
    );
    assert_eq!(problems_with(WORKERS_STEP, ""), order);
    let table_step = format!(
        "      - name: The generated build table is fresh\n        timeout-minutes: 30\n        run: {TABLE_RUN}\n"
    );
    assert_eq!(
        lint(
            FILE,
            &moved(
                &table_step,
                "      - name: Build the buildrules command line on the farm, local 0\n"
            )
        ),
        order
    );
    assert_eq!(
        lint(
            FILE,
            &moved(&table_step, "      - name: Stop buck2's daemon\n")
        ),
        order
    );
    assert_eq!(
        lint(FILE, &moved(CI_STEP, "      - name: Connect to the farm\n")),
        order
    );
}

#[test]
fn the_tool_is_built_from_komiras_sources_at_the_actions_commit() {
    // Catches: the tool's sources from another commit than the action's (the action's command
    // line and its tool's can differ), or farm-connect at another commit than buck2-farm; a
    // checkout of more or other files, or one that keeps its credentials; and a tool not built,
    // built otherwise than komira builds it, or built after the first build that runs it.
    let other = "0123456789abcdef0123456789abcdef01234567";
    let pins = [PINS_PROBLEM];
    assert_eq!(
        problems_with(
            "          ref: b2080fe989248adf9b47d4bd14c0ecefa0802f79\n",
            &format!("          ref: {other}\n")
        ),
        pins
    );
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@b2080fe989248adf9b47d4bd14c0ecefa0802f79"),
            &format!("{FARM_CONNECT}@{other}")
        ),
        pins
    );
    let tool = tool_problem();
    for (from, to) in [
        (
            "          sparse-checkout: /tools/build/ci/assert_remote/*.zig\n",
            "          sparse-checkout: /tools\n",
        ),
        (
            "          filter: blob:none\n          persist-credentials: false\n",
            "          filter: blob:none\n          persist-credentials: true\n",
        ),
        (
            "          path: buck-out/komira\n",
            "          path: komira\n",
        ),
        (" -OReleaseSafe ", " -ODebug "),
    ] {
        assert_eq!(problems_with(from, to), [tool.as_str()], "{to:?}");
    }
    let compile = format!("{COMPILE_STEP}        run: '{COMPILE_RUN}'\n");
    assert_eq!(problems_with(&compile, ""), [tool.as_str()]);
    assert_eq!(
        lint(
            FILE,
            &moved(&compile, "      - name: Stop buck2's daemon\n")
        ),
        [tool.as_str()]
    );
}

#[test]
fn the_daemon_is_stopped_after_the_build_however_the_job_went() {
    // Catches: buck2's daemon left running when a step failed or the job was cancelled, its stop
    // gone, or run before the build it is to stop.
    let kill = [kill_problem()];
    assert_eq!(
        problems_with(
            &format!("        if: {KILL_IF}\n"),
            "        if: ${{ success() }}\n"
        ),
        kill
    );
    assert_eq!(problems_with(KILL_STEP, ""), kill);
    assert_eq!(
        lint(
            FILE,
            &moved(
                KILL_STEP,
                "      - name: buck2 build //:ci on the farm, local 0\n"
            )
        ),
        kill
    );
}

#[test]
fn the_triggers_are_every_pull_request_and_a_push_to_main() {
    // Catches: a filter that skips pull requests (paths, branches), a push from any branch (whose
    // token the farm would trust as main's), or another trigger.
    let cases = [
        (
            "  pull_request:\n",
            "  pull_request:\n    paths: [crates/**]\n",
        ),
        ("    branches: [main]\n", "    branches: [main, dev]\n"),
        ("    branches: [main]\n", "    branches: main\n"),
        (
            "    branches: [main]\n",
            "    branches: [main]\n    paths: [crates/**]\n",
        ),
        (
            "  push:\n    branches: [main]\n",
            "  push:\n    branches: [main]\n  workflow_dispatch:\n",
        ),
        (
            "on:\n  pull_request:\n  push:\n    branches: [main]\n",
            "on: [pull_request, push]\n",
        ),
        ("    branches: [main]\n", "    branches: [dev]\n"),
        ("    branches: [main]\n", "    tags: [main]\n"),
    ];
    for (from, to) in cases {
        assert_eq!(problems_with(from, to), [ON_PROBLEM], "{to}");
    }
}

#[test]
fn the_job_runs_on_its_one_runner() {
    // Catches: a runner label that moves under us (`-latest`), where the farm's network policy
    // and the measured numbers assume one image.
    assert_eq!(
        problems_with(&format!("runs-on: {RUNNER}"), "runs-on: ubuntu-latest"),
        [format!("job build: runs-on must be {RUNNER}")]
    );
}

#[test]
fn a_second_job_is_refused() {
    // Catches: a job beside the farm build in the one pull-request workflow whose rights the
    // lint widens.
    assert_eq!(
        problems_with(
            "jobs:\n",
            "jobs:\n  other:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 5\n    permissions:\n      contents: read\n    steps:\n      - run: echo other\n        timeout-minutes: 1\n"
        ),
        ["job other is not a job of farm.yml"]
    );
}

/// The upload step of the committed file.
const UPLOAD_STEP: &str = "      - name: Upload the regenerated build table as a patch
        if: ${{ failure() }}
        timeout-minutes: 10
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: buildrules-sync
          path: ${{ runner.temp }}/buildrules-sync/buildrules-sync.patch
          if-no-files-found: ignore
          retention-days: 30
";

#[test]
fn the_build_writes_the_patch_the_upload_takes() {
    // Catches: the table check writing the patch to one file and the upload taking another, so a
    // stale table's summary names an artifact that holds nothing; and the check reading another
    // command line than the one the build before it wrote.
    assert!(TABLE_RUN.contains(&format!("--patch \"{}\"", patch_file(true))));
    assert!(TOOL_ARGS.ends_with(" --out ${{ runner.temp }}/buildrules-cli"));
    assert!(TABLE_RUN.contains(" --buildrules \"$RUNNER_TEMP/buildrules-cli\" "));
    assert_eq!(
        upload_inputs().get("path").map(String::as_str),
        Some(patch_file(false).as_str())
    );
    assert_eq!(
        patch_file(false),
        "${{ runner.temp }}/buildrules-sync/buildrules-sync.patch"
    );
    assert_eq!(
        patch_file(true),
        "$RUNNER_TEMP/buildrules-sync/buildrules-sync.patch"
    );
}

#[test]
fn the_patch_is_uploaded_once_after_a_failed_build_under_its_name() {
    // Catches: no upload of the patch, two of them, one under another artifact name (the summary's
    // command would download nothing), one with another action, one before the build (there is
    // no patch yet), one that runs on a passing build only or always, and one of another file or
    // kept another time.
    let none = format!(
        "job build: exactly one step must upload the artifact {PATCH_ARTIFACT} with \
         actions/upload-artifact@<commit>"
    );
    assert_eq!(problems_with(UPLOAD_STEP, ""), std::slice::from_ref(&none));
    assert_eq!(
        problems_with(UPLOAD_STEP, &UPLOAD_STEP.repeat(2)),
        std::slice::from_ref(&none)
    );
    assert_eq!(
        problems_with("name: buildrules-sync\n", "name: patch\n"),
        std::slice::from_ref(&none)
    );
    assert_eq!(
        problems_with(
            "uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            "uses: actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
        ),
        std::slice::from_ref(&none)
    );
    let moved = with_in(&committed(), UPLOAD_STEP, "").replacen(
        "      - name: Connect to the farm\n",
        &format!("{UPLOAD_STEP}      - name: Connect to the farm\n"),
        1,
    );
    assert_eq!(
        lint(FILE, &moved),
        [format!(
            "job build: the upload of {PATCH_ARTIFACT} does not come after the build"
        )]
    );
    let wrong_if = format!(
        "job build: the upload of {PATCH_ARTIFACT} must run under exactly `if: {UPLOAD_IF}`"
    );
    for to in [
        "        if: ${{ always() }}\n",
        "        if: ${{ success() }}\n",
        "",
    ] {
        assert_eq!(
            problems_with("        if: ${{ failure() }}\n", to),
            std::slice::from_ref(&wrong_if),
            "{to:?}"
        );
    }
    let wanted = upload_inputs();
    let wanted: std::collections::BTreeMap<&str, &str> =
        wanted.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let wrong_with =
        format!("job build: the upload of {PATCH_ARTIFACT}'s `with:` must be exactly {wanted:?}");
    for (from, to) in [
        (
            "path: ${{ runner.temp }}/buildrules-sync/buildrules-sync.patch",
            "path: ${{ runner.temp }}/buildrules-sync/",
        ),
        ("if-no-files-found: ignore", "if-no-files-found: error"),
        ("retention-days: 30", "retention-days: 90"),
        ("          retention-days: 30\n", ""),
    ] {
        assert_eq!(
            problems_with(from, to),
            std::slice::from_ref(&wrong_with),
            "{to:?}"
        );
    }
}
