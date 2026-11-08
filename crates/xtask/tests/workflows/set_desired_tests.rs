//! The rules of [`set_desired`](super::set_desired), on the real `set-desired.yml` and on
//! [`CLIENT`], a small workflow that keeps every rule, each case adding one defect to it and
//! reading the exact problems the lint reports.

use std::path::Path;

use super::lint::{ID_TOKEN_ALLOWED, entries, entry, lint};
use super::set_desired::{CLIENT_STEP, FILE, JOB, JOIN_INPUT, JOIN_STEP, VERIFY_STEPS};
use super::yaml::{self, Node, Value};
use super::{with_in, workflows_dir};

/// A CI client that keeps every rule: the download, the three provenance checks, the install and
/// the version check, then the join, the CA, the request, the two calls and the outputs. Its
/// job's `runs-on` is on line 24; its steps are, from 1: download, checksum, attestation, commit,
/// install, version, tailnet, ca, request, set-desired, wait, outputs.
pub(super) const CLIENT: &str = "\
name: set-desired
on:
  workflow_call:
    inputs:
      hostkeeper_commit:
        type: string
        required: true
      max_polls:
        type: number
        default: 180
      join_tailnet:
        type: boolean
        default: false
    outputs:
      desired_version:
        value: ${{ jobs.set-desired.outputs.desired_version }}
      converged:
        value: ${{ jobs.set-desired.outputs.converged }}
      reason:
        value: ${{ jobs.set-desired.outputs.reason }}
permissions: {}
jobs:
  set-desired:
    runs-on: ubuntu-24.04
    permissions:
      contents: read
      id-token: write
    outputs:
      desired_version: ${{ steps.outputs.outputs.desired_version }}
      converged: ${{ steps.outputs.outputs.converged || 'false' }}
      reason: ${{ steps.outputs.outputs.reason }}
    steps:
      - id: download
        run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors --remote-name https://github.com/komira-ai/hostkeeper/releases/download/v1/hkctl-1
      - id: checksum
        run: sha256sum --check --strict --ignore-missing SHA256SUMS
      - id: attestation
        env:
          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}
        run: |
          gh attestation verify hkctl-1 \\
            --repo komira-ai/hostkeeper \\
            --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \\
            --source-ref refs/heads/main \\
            --source-digest \"$HOSTKEEPER_COMMIT\" \\
            --deny-self-hosted-runners \\
            --format json > verified.json
      - id: commit
        env:
          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}
        run: jq --exit-status '[.[].verificationResult.signature.certificate.sourceRepositoryDigest] | unique == [env.HOSTKEEPER_COMMIT]' verified.json
      - id: install
        run: install -D --mode 0755 hkctl-1 \"$RUNNER_TEMP/hkctl-bin/hkctl\"
      - id: version
        run: test \"$(\"$RUNNER_TEMP/hkctl-bin/hkctl\" --version)\" = \"hkctl 1.0.0+0123456789ab\"
      - id: tailnet
        if: ${{ inputs.join_tailnet }}
        uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0
        with:
          oauth-client-id: ${{ inputs.tailnet_client_id }}
          audience: ${{ inputs.tailnet_audience }}
          tags: tag:hk-ci
          version: 1.94.2
          sha256sum: c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32
          ping: ${{ inputs.tailnet_coordinator }}
      - id: ca
        run: printf '%s\\n' \"$CA\" > \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\"
      - id: request
        run: jq --null-input '{}' > \"$RUNNER_TEMP/hkctl-request.json\"
      - id: set-desired
        continue-on-error: true
        env:
          SERVICE: ${{ inputs.service }}
          COORDINATOR_URL: ${{ inputs.coordinator_url }}
          AUDIENCE: ${{ inputs.audience }}
        run: |
          \"$RUNNER_TEMP/hkctl-bin/hkctl\" \\
            --endpoint \"$COORDINATOR_URL\" \\
            --ca-file \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\" \\
            --github-oidc-audience \"$AUDIENCE\" \\
            --response-file \"$RUNNER_TEMP/hkctl-set.json\" \\
            releases set-desired \\
            --service \"$SERVICE\" \\
            --request \"$RUNNER_TEMP/hkctl-request.json\" \\
            2> \"$RUNNER_TEMP/hkctl-set.err\"
      - id: wait
        if: ${{ inputs.wait && steps.set-desired.outcome == 'success' }}
        continue-on-error: true
        env:
          SERVICE: ${{ inputs.service }}
          VERSION: ${{ inputs.version }}
          COORDINATOR_URL: ${{ inputs.coordinator_url }}
          AUDIENCE: ${{ inputs.audience }}
          MAX_POLLS: ${{ inputs.max_polls }}
        run: |
          \"$RUNNER_TEMP/hkctl-bin/hkctl\" \\
            --endpoint \"$COORDINATOR_URL\" \\
            --ca-file \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\" \\
            --github-oidc-audience \"$AUDIENCE\" \\
            --response-file \"$RUNNER_TEMP/hkctl-wait.json\" \\
            releases wait-converged \\
            --service \"$SERVICE\" \\
            --version \"$VERSION\" \\
            --wait-secs \"${MAX_POLLS}0\" \\
            2> \"$RUNNER_TEMP/hkctl-wait.err\"
      - id: outputs
        if: ${{ always() }}
        env:
          SERVICE: ${{ inputs.service }}
          VERSION: ${{ inputs.version }}
          WAIT: ${{ inputs.wait }}
          SET_OUTCOME: ${{ steps.set-desired.outcome }}
          WAIT_OUTCOME: ${{ steps.wait.outcome }}
          SET_BODY: ${{ steps.set-desired.outcome == 'success' && format('{0}/hkctl-set.json', runner.temp) || '/dev/null' }}
          SET_ERR: ${{ steps.set-desired.outcome == 'failure' && format('{0}/hkctl-set.err', runner.temp) || '/dev/null' }}
          WAIT_BODY: ${{ steps.wait.outcome == 'success' && format('{0}/hkctl-wait.json', runner.temp) || '/dev/null' }}
          WAIT_ERR: ${{ steps.wait.outcome == 'failure' && format('{0}/hkctl-wait.err', runner.temp) || '/dev/null' }}
          OUTPUTS_PROGRAM: '\"reason=converged\"'
        run: |
          jq --null-input --raw-output \\
            --arg service \"$SERVICE\" \\
            --arg version \"$VERSION\" \\
            --arg wait \"$WAIT\" \\
            --arg set_outcome \"$SET_OUTCOME\" \\
            --arg wait_outcome \"$WAIT_OUTCOME\" \\
            --rawfile set_body \"$SET_BODY\" \\
            --rawfile set_err \"$SET_ERR\" \\
            --rawfile wait_body \"$WAIT_BODY\" \\
            --rawfile wait_err \"$WAIT_ERR\" \\
            \"$OUTPUTS_PROGRAM\" >> \"$GITHUB_OUTPUT\"
";

/// The set step of [`CLIENT`], whole.
pub(super) const CLIENT_BLOCK: &str = "      - id: set-desired
        continue-on-error: true
        env:
          SERVICE: ${{ inputs.service }}
          COORDINATOR_URL: ${{ inputs.coordinator_url }}
          AUDIENCE: ${{ inputs.audience }}
        run: |
          \"$RUNNER_TEMP/hkctl-bin/hkctl\" \\
            --endpoint \"$COORDINATOR_URL\" \\
            --ca-file \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\" \\
            --github-oidc-audience \"$AUDIENCE\" \\
            --response-file \"$RUNNER_TEMP/hkctl-set.json\" \\
            releases set-desired \\
            --service \"$SERVICE\" \\
            --request \"$RUNNER_TEMP/hkctl-request.json\" \\
            2> \"$RUNNER_TEMP/hkctl-set.err\"
";

/// The join step of [`CLIENT`], whole.
pub(super) const JOIN_BLOCK: &str = "      - id: tailnet
        if: ${{ inputs.join_tailnet }}
        uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0
        with:
          oauth-client-id: ${{ inputs.tailnet_client_id }}
          audience: ${{ inputs.tailnet_audience }}
          tags: tag:hk-ci
          version: 1.94.2
          sha256sum: c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32
          ping: ${{ inputs.tailnet_coordinator }}
";

/// The input of [`CLIENT`] that turns the join on.
const JOIN_INPUT_BLOCK: &str =
    "      join_tailnet:\n        type: boolean\n        default: false\n";

/// The first lines of the attestation and commit steps of [`CLIENT`], with their commit.
const ATTESTATION_ENV: &str = "      - id: attestation\n        env:\n          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}\n";
const COMMIT_ENV: &str = "      - id: commit\n        env:\n          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}\n";

pub(super) fn real() -> String {
    std::fs::read_to_string(workflows_dir().join(FILE)).expect("read set-desired.yml")
}

pub(super) fn assert_client(text: &str, want: &[&str]) {
    assert_eq!(lint(FILE, text), want, "in:\n{text}");
}

/// The `id` of every step of the one job, in order (`-` for a step with none).
pub(super) fn step_ids(text: &str) -> Vec<String> {
    let doc = yaml::load(text).expect("the workflow parses");
    let jobs = entry(&doc, "jobs").expect("jobs");
    let job = entry(jobs, JOB).expect("the job");
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps
            .iter()
            .map(|s| {
                entry(s, "id")
                    .and_then(Node::as_str)
                    .unwrap_or("-")
                    .to_owned()
            })
            .collect(),
        _ => panic!("the job has no steps"),
    }
}

/// `text` with the block from the line `from` up to (not including) the line `to` moved to just
/// before the line `before`.
pub(super) fn moved(text: &str, from: &str, to: &str, before: &str) -> String {
    let start = text.find(from).expect("the block's first line");
    let end = start + text[start..].find(to).expect("the line after the block");
    let block = text[start..end].to_owned();
    let without = format!("{}{}", &text[..start], &text[end..]);
    with_in(&without, before, &format!("{block}{before}"))
}

#[test]
fn the_oidc_request_comes_after_every_verification_step() {
    // Catches: the client, the first step that requests a token, run before the release is
    // proven and its version checked, in the real file (by reordering its steps) and in a small
    // one.
    let text = real();
    assert_client(&text, &[]);
    let ids = step_ids(&text);
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    for id in VERIFY_STEPS {
        assert!(
            at(CLIENT_STEP) > at(id),
            "{CLIENT_STEP} before {id}: {ids:?}"
        );
    }
    assert_client(CLIENT, &[]);
    let early = with_in(CLIENT, CLIENT_BLOCK, "");
    let early = with_in(
        &early,
        "      - id: checksum\n",
        &format!("{CLIENT_BLOCK}      - id: checksum\n"),
    );
    // The set step at 2 and the checksum at 3: every later step is one further on.
    let early_want = [
        "the OIDC token step `set-desired` (step 2) comes before the verification step `checksum` (step 3)",
        "the OIDC token step `set-desired` (step 2) comes before the verification step `attestation` (step 4)",
        "the OIDC token step `set-desired` (step 2) comes before the verification step `commit` (step 5)",
        "the OIDC token step `set-desired` (step 2) comes before the verification step `version` (step 7)",
        "the set step `set-desired` (step 2) comes before `ca` (step 9), whose file it reads",
        "the set step `set-desired` (step 2) comes before `request` (step 10), whose file it reads",
        "step 2 (`set-desired`) comes before the last verification step and runs hkctl",
        "the join step `tailnet` (step 8) comes after the OIDC token step `set-desired` (step 2)",
    ];
    assert_client(&early, &early_want);
    // The real file with its set step moved to just before the checksum: the download is step 1.
    let reordered = moved(
        &text,
        "      # The first step that requests an OIDC token for the coordinator",
        "      # One held call",
        "      # Verification 1 of 3",
    );
    assert_client(&reordered, &early_want);
}

#[test]
fn nothing_before_verification_touches_the_token_or_ctl() {
    // Catches: a step before the last provenance check that runs ctl or the hostkeeper program,
    // reads the token request variables, or uses an action (which can read them); the job's
    // `env` naming them; and a check or the client that the job could run on past.
    let commit = "      - id: commit\n";
    let cases: [(&str, &[&str]); 4] = [
        (
            "      - id: early\n        run: hostkeeper ctl releases converge --service s\n",
            &["step 4 (`early`) comes before the last verification step and runs ctl"],
        ),
        (
            "      - id: early\n        run: exec \"$RUNNER_TEMP/hkctl-bin/hkctl\" --version\n",
            &["step 4 (`early`) comes before the last verification step and runs hkctl"],
        ),
        (
            "      - id: early\n        run: echo \"$ACTIONS_ID_TOKEN_REQUEST_TOKEN\"\n",
            &[
                "step 4 (`early`) comes before the last verification step and names the OIDC token request",
            ],
        ),
        (
            "      - uses: actions/github-script@60a0d83039c74a4aee543508d2ffcb1c3799cdea # v7.0.1\n        with:\n          script: core.getIDToken()\n",
            &[
                "step 4 (`unnamed`) comes before the last verification step and uses an action, which can read the OIDC token request",
                "step 4 (`unnamed`) comes before the last verification step and names the OIDC token request",
            ],
        ),
    ];
    for (step, want) in cases {
        assert_client(&with_in(CLIENT, commit, &format!("{step}{commit}")), want);
    }
    let runner = "    runs-on: ubuntu-24.04\n";
    assert_client(
        &with_in(
            CLIENT,
            runner,
            &format!("{runner}    env:\n      URL: ${{{{ env.ACTIONS_ID_TOKEN_REQUEST_URL }}}}\n"),
        ),
        &["the job's `env` names the OIDC token request"],
    );
    assert_client(
        &with_in(
            CLIENT,
            commit,
            &format!("{commit}        continue-on-error: true\n"),
        ),
        &["step 4 (`commit`) has `continue-on-error`, which could run the job on past it"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "      - id: set-desired\n",
            "      - id: set-desired\n        if: ${{ always() }}\n",
        ),
        &["step 10 (`set-desired`) has `if`, which could run the job on past it"],
    );
    // The order is read from the one step with the client's id: a second one is refused.
    assert_client(
        &with_in(
            CLIENT,
            CLIENT_BLOCK,
            &format!("{CLIENT_BLOCK}{CLIENT_BLOCK}"),
        ),
        &["more than one step has `id: set-desired`"],
    );
}

#[test]
fn id_token_write_is_granted_to_the_job_only() {
    // Catches: `id-token: write` granted to the whole workflow, the job holding more or less
    // than `contents: read` and `id-token: write`, and the scope allowed in another workflow.
    let doc = yaml::load(&real()).expect("parses");
    let top = entry(&doc, "permissions").expect("top-level permissions");
    assert!(top.is_empty_map(), "top-level permissions is not {{}}");
    assert_eq!(ID_TOKEN_ALLOWED, [(FILE, JOB)]);
    let exactly =
        "job set-desired: `permissions` is not exactly `contents: read` and `id-token: write`";
    assert_client(
        &with_in(
            CLIENT,
            "permissions: {}\n",
            "permissions:\n  id-token: write\n",
        ),
        &["no top-level `permissions: {}`"],
    );
    assert_client(&with_in(CLIENT, "      id-token: write\n", ""), &[exactly]);
    assert_client(
        &with_in(
            CLIENT,
            "      id-token: write\n",
            "      id-token: write\n      actions: read\n",
        ),
        &[exactly],
    );
    assert_client(
        &with_in(CLIENT, "      contents: read\n", "      contents: write\n"),
        &[
            "job set-desired: `contents: write` is not allowed in set-desired.yml",
            exactly,
        ],
    );
    // The same workflow under another name: the scope is not allowed there, and the CI
    // client's own rules do not apply.
    assert_eq!(
        lint("ci.yml", CLIENT),
        ["job set-desired: `id-token: write` is not allowed in ci.yml"]
    );
}

#[test]
fn the_workflow_has_no_pull_request_target_and_no_inherited_secrets() {
    // Catches: the client run by another trigger than a call (pull_request_target runs fork
    // code with the base repository's rights), and secrets reaching it.
    assert_client(
        &with_in(CLIENT, "  workflow_call:\n", "  pull_request_target:\n"),
        &[
            "`on` names pull_request_target",
            "the document names pull_request_target",
            "job set-desired: privileged on pull_request (`id-token: write`)",
            "`on` is not `workflow_call` alone",
            "no input `max_polls` of `type: number`",
            "the output `desired_version` of `workflow_call` is not `${{ jobs.set-desired.outputs.desired_version }}`",
            "the output `converged` of `workflow_call` is not `${{ jobs.set-desired.outputs.converged }}`",
            "the output `reason` of `workflow_call` is not `${{ jobs.set-desired.outputs.reason }}`",
            "no input `join_tailnet` of `type: boolean` with `default: false`",
        ],
    );
    assert_client(
        &with_in(CLIENT, "  workflow_call:\n", "  push:\n  workflow_call:\n"),
        &["`on` is not `workflow_call` alone"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: ubuntu-24.04\n    secrets: inherit\n",
        ),
        &["line 25: `secrets: inherit` hands every secret of the caller on"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "  workflow_call:\n",
            "  workflow_call:\n    secrets:\n      token:\n        required: true\n",
        ),
        &["`workflow_call` declares secrets"],
    );
    // Every workflow is held to the `secrets: inherit` rule, not only this one.
    assert_eq!(
        lint(
            "ci.yml",
            &with_in(CLIENT, "      id-token: write\n", "    secrets: inherit\n")
        ),
        ["line 27: `secrets: inherit` hands every secret of the caller on"]
    );
}

#[test]
fn the_verification_step_names_the_signer_workflow_repo_and_ref() {
    // Catches: the attestation accepted from another repository, another workflow, another ref,
    // another commit or a self-hosted runner, by a dropped, changed, repeated or widened flag;
    // and the commit comparison made against anything but `hostkeeper_commit`.
    assert_client(&real(), &[]);
    let repo = "            --repo komira-ai/hostkeeper \\\n";
    let no_repo = "the attestation step does not pass `--repo komira-ai/hostkeeper` exactly once";
    assert_client(&with_in(CLIENT, repo, ""), &[no_repo]);
    assert_client(
        &with_in(CLIENT, repo, "            --owner komira-ai \\\n"),
        &[
            no_repo,
            "the attestation step passes `--owner`, which accepts another signer",
        ],
    );
    assert_client(
        &with_in(
            CLIENT,
            "workflows/artifacts.yml \\",
            "workflows/release.yml \\",
        ),
        &[
            "the attestation step does not pass `--signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml` exactly once",
        ],
    );
    let no_ref = "the attestation step does not pass `--source-ref refs/heads/main` exactly once";
    assert_client(
        &with_in(CLIENT, "refs/heads/main", "refs/heads/*"),
        &[no_ref],
    );
    assert_client(
        &with_in(
            CLIENT,
            repo,
            &format!("{repo}            --source-ref refs/tags/v1 \\\n"),
        ),
        &[no_ref],
    );
    assert_client(
        &with_in(
            CLIENT,
            "--source-ref refs/heads/main",
            "--source-ref=refs/heads/main",
        ),
        &[],
    );
    assert_client(
        &with_in(
            CLIENT,
            repo,
            &format!("{repo}            --cert-identity-regex .* \\\n"),
        ),
        &["the attestation step passes `--cert-identity-regex`, which accepts another signer"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "--source-digest \"$HOSTKEEPER_COMMIT\"",
            "--source-digest \"$OTHER\"",
        ),
        &["the attestation step does not pass `--source-digest $HOSTKEEPER_COMMIT` exactly once"],
    );
    assert_client(
        &with_in(CLIENT, "            --deny-self-hosted-runners \\\n", ""),
        &["the attestation step does not pass `--deny-self-hosted-runners` exactly once"],
    );
    assert_client(
        &with_in(CLIENT, "gh attestation verify", "gh attestation download"),
        &["the attestation step does not run `gh attestation verify`"],
    );
    let version = "${{ inputs.version }}";
    assert_client(
        &with_in(
            CLIENT,
            ATTESTATION_ENV,
            &ATTESTATION_ENV.replace("${{ inputs.hostkeeper_commit }}", version),
        ),
        &["the attestation step's `HOSTKEEPER_COMMIT` is not `${{ inputs.hostkeeper_commit }}`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            COMMIT_ENV,
            &COMMIT_ENV.replace("${{ inputs.hostkeeper_commit }}", version),
        ),
        &["the commit step's `HOSTKEEPER_COMMIT` is not `${{ inputs.hostkeeper_commit }}`"],
    );
    let compare =
        "the commit step does not compare `sourceRepositoryDigest` with `env.HOSTKEEPER_COMMIT`";
    assert_client(
        &with_in(CLIENT, "sourceRepositoryDigest", "sourceRepositoryRef"),
        &[compare],
    );
    assert_client(
        &with_in(CLIENT, "[env.HOSTKEEPER_COMMIT]", "[env.OTHER]"),
        &[compare],
    );
}

/// One input as the YAML declares it or the table documents it: name, type, whether it is
/// required, default.
type Input = (String, String, bool, String);

/// The inputs and outputs `set-desired.yml` declares.
fn declared(text: &str) -> (Vec<Input>, Vec<String>) {
    let doc = yaml::load(text).expect("parses");
    let call = entry(&doc, "on")
        .and_then(|on| entry(on, "workflow_call"))
        .expect("on.workflow_call");
    let field = |node: &Node, key: &str| {
        entry(node, key)
            .and_then(Node::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let inputs = entry(call, "inputs")
        .map_or(&[][..], entries)
        .iter()
        .map(|(name, input)| {
            (
                name.as_str().unwrap_or_default().to_owned(),
                field(input, "type"),
                field(input, "required") == "true",
                field(input, "default"),
            )
        })
        .collect();
    let outputs = entry(call, "outputs")
        .map_or(&[][..], entries)
        .iter()
        .map(|(name, _)| name.as_str().unwrap_or_default().to_owned())
        .collect();
    (inputs, outputs)
}

/// The cells of each row of the first markdown table whose header row starts `| <first> |`.
fn table(markdown: &str, first: &str) -> Vec<Vec<String>> {
    let head = format!("| {first} |");
    let mut lines = markdown.lines().skip_while(|l| !l.starts_with(&head));
    assert!(lines.next().is_some(), "no table `{head}`");
    lines
        .skip(1)
        .take_while(|l| l.starts_with('|'))
        .map(|l| {
            l.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches('`').to_owned())
                .collect()
        })
        .collect()
}

/// The inputs and outputs `docs/deploy/ci-client.md` documents.
fn documented(markdown: &str) -> (Vec<Input>, Vec<String>) {
    let inputs = table(markdown, "input")
        .into_iter()
        .map(|row| {
            let cell = |i: usize| row.get(i).cloned().unwrap_or_default();
            (cell(0), cell(1), cell(2) == "yes", cell(3))
        })
        .collect();
    let outputs = table(markdown, "output")
        .into_iter()
        .map(|row| row.first().cloned().unwrap_or_default())
        .collect();
    (inputs, outputs)
}

#[test]
fn the_inputs_and_outputs_match_the_documented_table() {
    // Catches: an input or output added, removed, retyped or given another default in the
    // workflow without the docs, or in the docs without the workflow.
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/deploy/ci-client.md");
    let markdown = std::fs::read_to_string(docs).expect("read docs/deploy/ci-client.md");
    let text = real();
    let (inputs, outputs) = declared(&text);
    assert_eq!(documented(&markdown), (inputs.clone(), outputs.clone()));
    let names: Vec<&str> = inputs.iter().map(|i| i.0.as_str()).collect();
    for name in [
        "service",
        "version",
        "package_digest",
        "release_index",
        "release_index_digest",
        "hostkeeper_version",
        "hostkeeper_commit",
        "coordinator_url",
        "wait",
        "max_polls",
    ] {
        assert!(names.contains(&name), "no input {name}");
    }
    assert_eq!(outputs, ["desired_version", "converged", "reason"]);
    // Either side changed alone no longer matches.
    let row = "| `max_polls` | number | no | `180` |";
    let docs_changed = with_in(&markdown, row, "| `max_polls` | number | no | `120` |");
    assert_ne!(documented(&docs_changed), declared(&text));
    let yaml_changed = with_in(&text, "        default: 180\n", "        default: 120\n");
    assert_ne!(documented(&markdown), declared(&yaml_changed));
    let output_row = "| `reason` |";
    let start = markdown.find(output_row).expect("the reason row");
    let end = start + markdown[start..].find('\n').expect("the row ends");
    let docs_dropped = format!("{}{}", &markdown[..start], &markdown[end + 1..]);
    assert_ne!(documented(&docs_dropped), declared(&text));
    let yaml_added = with_in(
        &text,
        "      wait:\n",
        "      dry_run:\n        type: boolean\n        required: false\n      wait:\n",
    );
    assert_ne!(documented(&markdown), declared(&yaml_added));
}

#[test]
fn without_the_join_input_and_step_the_workflow_is_refused() {
    // Catches: the client shipped without a way for a hosted runner to reach the private
    // listener, or with the join on by default; in the real file and in a small one.
    let text = real();
    let doc = yaml::load(&text).expect("parses");
    let input = entry(&doc, "on")
        .and_then(|on| entry(on, "workflow_call"))
        .and_then(|c| entry(c, "inputs"))
        .and_then(|i| entry(i, JOIN_INPUT))
        .expect("the join input");
    assert_eq!(entry(input, "type").and_then(Node::as_str), Some("boolean"));
    assert_eq!(
        entry(input, "default").and_then(Node::as_str),
        Some("false")
    );
    let no_input = "no input `join_tailnet` of `type: boolean` with `default: false`";
    let no_step = "no step has `id: tailnet`";
    // The real file as it was before the join: no input, no step.
    let start = text.find("      join_tailnet:\n").expect("the input");
    let end = start + text[start..].find("    outputs:\n").expect("the outputs");
    let without_input = format!("{}{}", &text[..start], &text[end..]);
    let start = without_input
        .find("      # Only with `join_tailnet`")
        .expect("the join step");
    let end = start
        + without_input[start..]
            .find("      - name: Write the coordinator's CA\n")
            .expect("the CA step");
    let before = format!("{}{}", &without_input[..start], &without_input[end..]);
    assert_client(&before, &[no_input, no_step]);
    assert_client(&with_in(CLIENT, JOIN_INPUT_BLOCK, ""), &[no_input]);
    assert_client(&with_in(CLIENT, JOIN_BLOCK, ""), &[no_step]);
    for (from, to) in [
        ("        default: false\n", "        default: true\n"),
        ("        default: false\n", ""),
        ("        type: boolean\n", "        type: string\n"),
    ] {
        let changed = with_in(
            CLIENT,
            JOIN_INPUT_BLOCK,
            &JOIN_INPUT_BLOCK.replace(from, to),
        );
        assert_client(&changed, &[no_input]);
    }
    assert_client(
        &with_in(CLIENT, JOIN_BLOCK, &format!("{JOIN_BLOCK}{JOIN_BLOCK}")),
        &["more than one step has `id: tailnet`"],
    );
}

#[test]
fn the_join_runs_only_when_asked_after_every_check_and_before_the_token_step() {
    // Catches: a join that runs whatever the input says, that the job runs on past, or that sits
    // before a verification step or after the client; in the real file and in a small one.
    let text = real();
    let ids = step_ids(&text);
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    for id in VERIFY_STEPS {
        assert!(at(JOIN_STEP) > at(id), "{JOIN_STEP} before {id}: {ids:?}");
    }
    assert!(at(JOIN_STEP) < at(CLIENT_STEP), "{ids:?}");
    let join_if = "        if: ${{ inputs.join_tailnet }}\n";
    let no_if = "the join step `tailnet` (step 7) does not have `if: ${{ inputs.join_tailnet }}`";
    assert_client(&with_in(CLIENT, join_if, ""), &[no_if]);
    assert_client(
        &with_in(CLIENT, join_if, "        if: ${{ always() }}\n"),
        &[no_if],
    );
    assert_client(
        &with_in(
            CLIENT,
            join_if,
            &format!("{join_if}        continue-on-error: true\n"),
        ),
        &[
            "the join step `tailnet` (step 7) has `continue-on-error`, which could run the job on past it",
        ],
    );
    // Before the first check: every check comes after it.
    let first = moved(
        CLIENT,
        "      - id: tailnet\n",
        "      - id: ca\n",
        "      - id: checksum\n",
    );
    assert_client(
        &first,
        &[
            "step 2 (`tailnet`) comes before the last verification step and uses an action, which can read the OIDC token request",
            "the join step `tailnet` (step 2) comes before the verification step `checksum` (step 3)",
            "the join step `tailnet` (step 2) comes before the verification step `attestation` (step 4)",
            "the join step `tailnet` (step 2) comes before the verification step `commit` (step 5)",
            "the join step `tailnet` (step 2) comes before the verification step `version` (step 7)",
        ],
    );
    // After the provenance checks, before the version check.
    let between = moved(
        CLIENT,
        "      - id: tailnet\n",
        "      - id: ca\n",
        "      - id: install\n",
    );
    assert_client(
        &between,
        &["the join step `tailnet` (step 5) comes before the verification step `version` (step 7)"],
    );
    // After the calls, last.
    let late = format!("{}{JOIN_BLOCK}", with_in(CLIENT, JOIN_BLOCK, ""));
    assert_client(
        &late,
        &[
            "the outputs step `outputs` (step 11) is not the last step",
            "the join step `tailnet` (step 12) comes after the OIDC token step `set-desired` (step 9)",
        ],
    );
    // The real file with its join moved to just before the attestation check.
    let reordered = moved(
        &text,
        "      # Only with `join_tailnet`",
        "      - name: Write the coordinator's CA\n",
        "      # Verification 2 of 3",
    );
    assert_client(
        &reordered,
        &[
            "step 3 (`tailnet`) comes before the last verification step and uses an action, which can read the OIDC token request",
            "the join step `tailnet` (step 3) comes before the verification step `attestation` (step 4)",
            "the join step `tailnet` (step 3) comes before the verification step `commit` (step 5)",
            "the join step `tailnet` (step 3) comes before the verification step `version` (step 7)",
        ],
    );
}

#[test]
fn no_action_runs_before_the_join() {
    // Catches: a third-party or GitHub action placed after the checks but before the join, so
    // the join is no longer the first action of the job; a plain `run:` step there is allowed.
    let join = "      - id: tailnet\n";
    let action = "      - id: other\n        uses: actions/github-script@60a0d83039c74a4aee543508d2ffcb1c3799cdea # v7.0.1\n";
    assert_client(
        &with_in(CLIENT, join, &format!("{action}{join}")),
        &["step 7 (`other`) comes before the join step and uses an action"],
    );
    let plain = "      - id: other\n        run: echo ready\n";
    assert_client(&with_in(CLIENT, join, &format!("{plain}{join}")), &[]);
    // After the join, the client may follow other steps of its own.
    assert_client(
        &with_in(
            CLIENT,
            "      - id: set-desired\n",
            &format!("{plain}      - id: set-desired\n"),
        ),
        &[],
    );
}

#[test]
fn the_join_action_is_pinned_federated_and_names_no_secret() {
    // Catches: the join action moved to a tag, a short SHA or another repository; a stored
    // credential or any `secrets.` reference in the join; another tag or identity than the
    // inputs name; and the Tailscale tarball left to float.
    let pin = "tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd";
    let unpinned = "the join step `tailnet` (step 7) does not use `tailscale/github-action` pinned to a 40-digit lowercase commit SHA";
    let cases: [(&str, &[&str]); 3] = [
        (
            "tailscale/github-action@v4",
            &[
                "line 58: `uses: tailscale/github-action@v4` is not pinned to a 40-digit lowercase commit SHA",
                unpinned,
            ],
        ),
        (
            "tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be39005",
            &[
                "line 58: `uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be39005` is not pinned to a 40-digit lowercase commit SHA",
                unpinned,
            ],
        ),
        (
            "someone/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd",
            &[unpinned],
        ),
    ];
    for (to, want) in cases {
        assert_client(&with_in(CLIENT, pin, to), want);
    }
    let tags = "          tags: tag:hk-ci\n";
    let secret = "the join step `tailnet` (step 7) names `secrets.`";
    assert_client(
        &with_in(
            CLIENT,
            tags,
            &format!("{tags}          oauth-secret: ${{{{ secrets.TS_OAUTH_SECRET }}}}\n"),
        ),
        &[
            secret,
            "the join step `tailnet` (step 7) passes `oauth-secret`, a stored credential",
        ],
    );
    assert_client(
        &with_in(
            CLIENT,
            tags,
            &format!("{tags}          authkey: tskey-auth-k0123\n"),
        ),
        &["the join step `tailnet` (step 7) passes `authkey`, a stored credential"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "audience: ${{ inputs.tailnet_audience }}",
            "audience: ${{ secrets.TS_AUDIENCE }}",
        ),
        &[
            secret,
            "the join step `tailnet` (step 7) does not pass `audience: ${{ inputs.tailnet_audience }}`",
        ],
    );
    let join_if = "        if: ${{ inputs.join_tailnet }}\n";
    assert_client(
        &with_in(
            CLIENT,
            join_if,
            &format!("{join_if}        env:\n          TS: ${{{{ secrets.TS }}}}\n"),
        ),
        &[secret],
    );
    assert_client(
        &with_in(CLIENT, tags, "          tags: tag:ci,tag:admin\n"),
        &["the join step `tailnet` (step 7) does not pass `tags: tag:hk-ci`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "oauth-client-id: ${{ inputs.tailnet_client_id }}",
            "oauth-client-id: kAbCd",
        ),
        &[
            "the join step `tailnet` (step 7) does not pass `oauth-client-id: ${{ inputs.tailnet_client_id }}`",
        ],
    );
    // The coordinator the join waits for is the caller's input, in the real file too.
    let doc = yaml::load(&real()).expect("parses");
    let steps = entry(&doc, "jobs")
        .and_then(|j| entry(j, JOB))
        .and_then(|j| entry(j, "steps"))
        .map(|s| &s.value);
    let Some(Value::Seq(steps)) = steps else {
        panic!("the job has no steps")
    };
    let join = steps
        .iter()
        .find(|s| entry(s, "id").and_then(Node::as_str) == Some(JOIN_STEP))
        .expect("the join step");
    assert_eq!(
        entry(join, "with")
            .and_then(|w| entry(w, "ping"))
            .and_then(Node::as_str),
        Some("${{ inputs.tailnet_coordinator }}")
    );
    let ping = "          ping: ${{ inputs.tailnet_coordinator }}\n";
    let no_ping =
        "the join step `tailnet` (step 7) does not pass `ping: ${{ inputs.tailnet_coordinator }}`";
    for to in [
        "",
        "          ping: hk-coordinator\n",
        "          ping: ${{ inputs.coordinator_url }}\n",
    ] {
        assert_client(&with_in(CLIENT, ping, to), &[no_ping]);
    }
    let floating = "the join step `tailnet` (step 7) does not pin the Tailscale tarball by an `X.Y.Z` `version` and a 64-digit lowercase `sha256sum`";
    let sha =
        "          sha256sum: c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32\n";
    for (from, to) in [
        ("          version: 1.94.2\n", "          version: latest\n"),
        ("          version: 1.94.2\n", "          version: 1.94.\n"),
        ("          version: 1.94.2\n", ""),
        (sha, ""),
        (
            sha,
            "          sha256sum: C6F99A5D774C7783B56902188D69E9756FC3DDDFB08AC6BE4CB2585F3FECDC32\n",
        ),
        (sha, "          sha256sum: c6f99a5d\n"),
    ] {
        assert_client(&with_in(CLIENT, from, to), &[floating]);
    }
}
