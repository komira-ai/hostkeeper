//! The rules of `farm.yml` ([`super::farm`]), the farm lane: the one job that holds
//! `id-token: write` on a pull request, and only under its exact `if:`; farm-connect pinned by
//! commit in that file alone, with exactly the variables it is given; the triggers, the runner,
//! the rights, and the build through `cargo xtask farm-build` after farm-connect. Each rule is shown to refuse its defect
//! by putting it into the committed workflow and reading the exact problems the lint reports.

use super::farm::{BUILD_RUN, CONNECT_INPUTS, FARM_CONNECT, FILE, JOB_IF, ON_PROBLEM, RUNNER};
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
    // with no version comment.
    let found = lint("ci.yml", &committed());
    let connect = line_of(FARM_CONNECT);
    for want in [
        "job build: `id-token: write` is not allowed in ci.yml".to_owned(),
        "job build: privileged on pull_request (`id-token: write`)".to_owned(),
        format!(
            "line {connect}: `uses: {FARM_CONNECT}@a26616d62db17ee8aff67ee9c12d411c2fad7609` has no version comment (`# v1.2.3`) on its line"
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
    // farm-connect at a branch instead of a commit.
    let checkout = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
    assert_eq!(
        problems_with(&format!("{checkout} # v7.0.1"), checkout),
        [format!(
            "line {}: `uses: {checkout}` has no version comment (`# v1.2.3`) on its line",
            line_of(checkout)
        )]
    );
    let unpinned = format!("{FARM_CONNECT}@main");
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@a26616d62db17ee8aff67ee9c12d411c2fad7609"),
            &unpinned
        ),
        [format!(
            "line {}: `uses: {unpinned}` is not pinned to a 40-digit lowercase commit SHA",
            line_of(FARM_CONNECT)
        )]
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
            &format!("{FARM_CONNECT}@a26616d62db17ee8aff67ee9c12d411c2fad7609"),
            "example/join@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v1.0.0"
        ),
        [
            format!("job build: exactly one step must use {FARM_CONNECT}"),
            BUILD_ORDER.to_owned(),
        ]
    );
    // A name that only starts with farm-connect's is another action: it needs a version comment,
    // and the step is no farm-connect step.
    let lookalike = format!("{FARM_CONNECT}-fork@a26616d62db17ee8aff67ee9c12d411c2fad7609");
    assert_eq!(
        problems_with(
            &format!("{FARM_CONNECT}@a26616d62db17ee8aff67ee9c12d411c2fad7609"),
            &lookalike
        ),
        [
            format!(
                "line {}: `uses: {lookalike}` has no version comment (`# v1.2.3`) on its line",
                line_of(FARM_CONNECT)
            ),
            format!("job build: exactly one step must use {FARM_CONNECT}"),
            BUILD_ORDER.to_owned(),
        ]
    );
}

/// The problem of a build step that is missing from after farm-connect's.
const BUILD_ORDER: &str = "job build: the build does not come after the farm-connect step";

#[test]
fn the_job_runs_farm_build_of_ci_after_farm_connect() {
    // Catches: a build of something other than `//:ci`, a build not run through `farm-build`
    // (whose checks refuse no workers and `local` above 0), or one run before the farm is
    // connected, which would build on the runner.
    assert_eq!(
        problems_with(BUILD_RUN, &BUILD_RUN.replace("//:ci", "//...")),
        [format!("job build: no step runs `{BUILD_RUN}`")]
    );
    assert_eq!(
        problems_with(BUILD_RUN, "buck2 build //:ci"),
        [format!("job build: no step runs `{BUILD_RUN}`")]
    );
    let text = committed();
    let step =
        format!("      - name: buck2 build //:ci on the farm, local 0\n        run: {BUILD_RUN}\n");
    let early = with_in(&text, &step, "").replacen(
        "      - name: Connect to the farm\n",
        &format!("{step}      - name: Connect to the farm\n"),
        1,
    );
    assert_eq!(lint(FILE, &early), [BUILD_ORDER]);
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
            "jobs:\n  other:\n    runs-on: ubuntu-24.04\n    permissions:\n      contents: read\n    steps:\n      - run: echo other\n"
        ),
        ["job other is not a job of farm.yml"]
    );
}
