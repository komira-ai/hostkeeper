//! [`verify_doc`](super::verify_doc) on the real `set-desired.yml` and the real
//! `docs/deploy/verify-a-release.md`: they agree, and each case changes one of the two alone and
//! reads the exact problems.

use std::path::Path;

use super::set_desired::{COMMIT_INPUT, FILE};
use super::verify_doc::{DOC, RELEASE_DIR, VERSION_INPUT, problems};
use super::{with_in, workflows_dir, yaml};

/// The checksum step's command, as the document's step 2 gives it.
const CHECKSUM: &str = "sha256sum --check --strict --ignore-missing SHA256SUMS";
/// The attestation step's command, as the document's step 3 gives it, with the output the commit
/// step reads.
const ATTESTATION: &str = "gh attestation verify \"$RUNNER_TEMP/hostkeeper-release/$HOSTKEEPER_ASSET\" --repo komira-ai/hostkeeper --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml --source-ref refs/heads/main --source-digest \"$HOSTKEEPER_COMMIT\" --deny-self-hosted-runners --format json > \"$RUNNER_TEMP/hostkeeper-release/attestation.json\"";
/// The version step's command: the document's step 6 command, compared with its expected output.
const VERSION: &str = "test \"$(\"$RUNNER_TEMP/hostkeeper-bin/hostkeeper\" --version)\" = \"hostkeeper $HOSTKEEPER_VERSION+${HOSTKEEPER_COMMIT:0:12}\"";

fn real_doc() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DOC),
    )
    .expect("read docs/deploy/verify-a-release.md")
}

fn real_workflow() -> String {
    std::fs::read_to_string(workflows_dir().join(FILE)).expect("read set-desired.yml")
}

fn check(doc: &str, workflow: &str) -> Vec<String> {
    problems(doc, &yaml::load(workflow).expect("the workflow parses"))
}

/// The problem that step `id` runs `got` and not `want`, the command of the document's step `n`.
fn runs(id: &str, got: &str, want: &str, n: u8) -> String {
    format!("the {id} step runs `{got}`, not `{want}` of {DOC} step {n}")
}

fn tag_problem(tag: &str) -> String {
    format!("the job's `HOSTKEEPER_TAG` `{tag}` is not the tag of {DOC}")
}

fn asset_problem(asset: &str) -> String {
    format!(
        "the job's `HOSTKEEPER_ASSET` `{asset}` is not the binary tarball of {DOC} for `linux-x86_64`"
    )
}

/// The real asset name, with the version as the job's `env` names it.
const ASSET: &str = "hostkeeper-${{ inputs.hostkeeper_version }}-linux-x86_64.tar.gz";

#[test]
fn the_ci_client_runs_the_checks_of_verify_a_release() {
    // Catches: the workflow and the consumer document disagreeing today on any of the three
    // checks, the tag or the asset.
    assert_eq!(check(&real_doc(), &real_workflow()), Vec::<String>::new());
}

#[test]
fn a_check_changed_in_the_document_alone_is_refused() {
    // Catches: the document gaining, losing or changing a check the workflow does not follow,
    // or renaming the release's tag, its binary tarball or the runner's platform.
    let doc = real_doc();
    let workflow = real_workflow();
    let predicate = "--repo komira-ai/hostkeeper --predicate-type https://slsa.dev/provenance/v1";
    let cases: [(String, Vec<String>); 7] = [
        (
            with_in(
                &doc,
                &format!("cd release && {CHECKSUM}"),
                "cd release && sha256sum --check --ignore-missing SHA256SUMS",
            ),
            vec![runs(
                "checksum",
                CHECKSUM,
                "sha256sum --check --ignore-missing SHA256SUMS",
                2,
            )],
        ),
        (
            with_in(
                &doc,
                "gh attestation verify release/<asset> \\\n  --repo komira-ai/hostkeeper \\\n",
                "gh attestation verify release/<asset> \\\n  --repo komira-ai/hostkeeper \\\n  --predicate-type https://slsa.dev/provenance/v1 \\\n",
            ),
            vec![runs(
                "attestation",
                ATTESTATION,
                &ATTESTATION.replacen("--repo komira-ai/hostkeeper", predicate, 1),
                3,
            )],
        ),
        (
            with_in(
                &doc,
                "Expected output, exactly: `hostkeeper <X.Y.Z>+",
                "Expected output, exactly: `hostkeeper <X.Y.Z>-",
            ),
            vec![runs(
                "version",
                VERSION,
                &VERSION.replacen("+${", "-${", 1),
                6,
            )],
        ),
        (
            with_in(&doc, "| Tag | `v<X.Y.Z>` |", "| Tag | `<X.Y.Z>` |"),
            vec![tag_problem("v${{ inputs.hostkeeper_version }}")],
        ),
        (
            with_in(
                &doc,
                "| `hostkeeper-<version>-<platform>.tar.gz` | the daemon binary |",
                "| `hostkeeper-<version>-<platform>.tgz` | the daemon binary |",
            ),
            vec![asset_problem(ASSET)],
        ),
        (
            with_in(
                &doc,
                "`<platform>` is `linux-x86_64`, `linux-arm64` or `macos-arm64`",
                "`<platform>` is `linux-arm64` or `macos-arm64`",
            ),
            vec![asset_problem(ASSET)],
        ),
        (
            with_in(
                &doc,
                "./hostkeeper --version",
                "./hostkeeper --version --verbose",
            ),
            vec![runs(
                "version",
                VERSION,
                &VERSION.replacen("--version)", "--version --verbose)", 1),
                6,
            )],
        ),
    ];
    for (doc, want) in cases {
        assert_eq!(check(&doc, &workflow), want, "in:\n{doc}");
    }
}

#[test]
fn a_document_without_one_of_the_checks_refuses_the_workflow() {
    // Catches: a step heading renamed, a command moved out of its `sh` block, the version
    // command or its expected output dropped: each would leave a check of the workflow compared
    // with nothing.
    let doc = real_doc();
    let workflow = real_workflow();
    let cases: [(String, String); 4] = [
        (
            with_in(&doc, "\n### 2. Checksums", "\n### Checksums"),
            format!("{DOC} has no `sha256sum` command in step 2"),
        ),
        (
            with_in(
                &doc,
                "```sh\ngh attestation verify release/<asset> \\\n  --repo komira-ai/hostkeeper \\\n",
                "```\ngh attestation verify release/<asset> \\\n  --repo komira-ai/hostkeeper \\\n",
            ),
            format!("{DOC} has no `gh attestation verify` command in step 3"),
        ),
        (
            with_in(&doc, "./hostkeeper --version", "./hostkeeper version"),
            format!("{DOC} has no `--version` command in step 6"),
        ),
        (
            with_in(&doc, "Expected output, exactly: `", "Expected output: `"),
            format!("{DOC} has no expected output in step 6"),
        ),
    ];
    for (doc, want) in cases {
        assert_eq!(check(&doc, &workflow), [want], "in:\n{doc}");
    }
}

#[test]
fn a_check_changed_in_the_workflow_alone_is_refused() {
    // Catches: the workflow weakening, reordering or rewording a check of the document, running
    // it elsewhere, comparing the version with other inputs, or downloading another asset.
    let doc = real_doc();
    let workflow = real_workflow();
    let deny = "--deny-self-hosted-runners ";
    let repo = "--repo komira-ai/hostkeeper";
    let signer = "--signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml";
    let version_env = "          HOSTKEEPER_VERSION: ${{ inputs.hostkeeper_version }}\n          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}\n";
    let cases: [(String, Vec<String>); 9] = [
        (
            with_in(
                &workflow,
                &format!("        run: {CHECKSUM}\n"),
                &format!("        run: {CHECKSUM} || true\n"),
            ),
            vec![runs(
                "checksum",
                &format!("{CHECKSUM} || true"),
                CHECKSUM,
                2,
            )],
        ),
        (
            with_in(
                &workflow,
                &format!("        working-directory: {RELEASE_DIR}\n"),
                "        working-directory: ${{ runner.temp }}\n",
            ),
            vec![format!(
                "the checksum step does not run in `{RELEASE_DIR}`, the release directory of {DOC} step 2"
            )],
        ),
        (
            with_in(&workflow, "            --deny-self-hosted-runners \\\n", ""),
            vec![runs(
                "attestation",
                &ATTESTATION.replacen(deny, "", 1),
                ATTESTATION,
                3,
            )],
        ),
        (
            with_in(
                &workflow,
                &format!("            {repo} \\\n            {signer} \\\n"),
                &format!("            {signer} \\\n            {repo} \\\n"),
            ),
            vec![runs(
                "attestation",
                &ATTESTATION.replacen(&format!("{repo} {signer}"), &format!("{signer} {repo}"), 1),
                ATTESTATION,
                3,
            )],
        ),
        (
            with_in(&workflow, ":0:12}", ":0:7}"),
            vec![runs(
                "version",
                &VERSION.replacen(":0:12}", ":0:7}", 1),
                VERSION,
                6,
            )],
        ),
        (
            with_in(
                &workflow,
                version_env,
                &version_env.replacen("inputs.hostkeeper_version", "inputs.version", 1),
            ),
            vec![format!(
                "the version step's `HOSTKEEPER_VERSION` is not `{VERSION_INPUT}`"
            )],
        ),
        (
            with_in(
                &workflow,
                version_env,
                &version_env.replacen("inputs.hostkeeper_commit", "inputs.package_digest", 1),
            ),
            vec![format!(
                "the version step's `HOSTKEEPER_COMMIT` is not `{COMMIT_INPUT}`"
            )],
        ),
        (
            with_in(
                &workflow,
                "HOSTKEEPER_TAG: v${{ inputs.hostkeeper_version }}\n",
                "HOSTKEEPER_TAG: ${{ inputs.hostkeeper_version }}\n",
            ),
            vec![tag_problem("${{ inputs.hostkeeper_version }}")],
        ),
        (
            // The self-update package is an asset of the release too, but not the binary
            // tarball the workflow unpacks.
            with_in(
                &workflow,
                "-linux-x86_64.tar.gz\n",
                "-linux-x86_64.tar.zst\n",
            ),
            vec![asset_problem(&ASSET.replacen(".tar.gz", ".tar.zst", 1))],
        ),
    ];
    for (workflow, want) in cases {
        assert_eq!(check(&doc, &workflow), want, "in:\n{workflow}");
    }
}

#[test]
fn a_workflow_without_the_job_has_every_check_missing() {
    // Catches: a check that passes because its step is not there at all.
    let want = [
        tag_problem(""),
        asset_problem(""),
        runs("checksum", "", CHECKSUM, 2),
        format!(
            "the checksum step does not run in `{RELEASE_DIR}`, the release directory of {DOC} step 2"
        ),
        runs("attestation", "", ATTESTATION, 3),
        runs("version", "", VERSION, 6),
        format!("the version step's `HOSTKEEPER_VERSION` is not `{VERSION_INPUT}`"),
        format!("the version step's `HOSTKEEPER_COMMIT` is not `{COMMIT_INPUT}`"),
    ];
    assert_eq!(check(&real_doc(), "name: set-desired\n"), want);
}
