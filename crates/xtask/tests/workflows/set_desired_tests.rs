//! The rules of [`set_desired`](super::set_desired), on the real `set-desired.yml` and on
//! [`CLIENT`], a small workflow that keeps every rule, each case adding one defect to it and
//! reading the exact problems the lint reports.

use std::path::Path;

use super::lint::{ID_TOKEN_ALLOWED, entries, entry, lint};
use super::set_desired::{FILE, JOB, POLL_LOOP, TOKEN_STEP, VERIFY_STEPS};
use super::yaml::{self, Node, Value};
use super::{with_in, workflows_dir};

/// A CI client that keeps every rule: the three verification steps, then the token, then the
/// two calls, the poll bounded by `max_polls`. Its job's `runs-on` is on line 14.
const CLIENT: &str = "\
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
permissions: {}
jobs:
  set-desired:
    runs-on: ubuntu-24.04
    permissions:
      contents: read
      id-token: write
    steps:
      - id: checksum
        run: sha256sum --check --strict asset.sha256
      - id: attestation
        env:
          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}
        run: |
          gh attestation verify a.tar.gz --bundle a.json \\
            --repo komira-ai/hostkeeper \\
            --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \\
            --source-ref refs/heads/main \\
            --source-digest \"$HOSTKEEPER_COMMIT\" > verified.json
      - id: commit
        env:
          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}
        run: test \"$(jq -r '.[0].verificationResult.signature.certificate.sourceRepositoryDigest' verified.json)\" = \"$HOSTKEEPER_COMMIT\"
      - id: oidc-token
        run: curl --header @request \"$ACTIONS_ID_TOKEN_REQUEST_URL\" > token
      - id: set-desired
        run: hostkeeper ctl --token-file token releases set-desired --service s
      - id: converge
        env:
          MAX_POLLS: ${{ inputs.max_polls }}
        run: |
          for ((poll = 1; poll <= MAX_POLLS; poll++)); do
            hostkeeper ctl --token-file token releases converge --service s
          done
";

/// The token step of [`CLIENT`].
const TOKEN_BLOCK: &str = "      - id: oidc-token\n        run: curl --header @request \"$ACTIONS_ID_TOKEN_REQUEST_URL\" > token\n";

/// The first lines of the attestation and commit steps of [`CLIENT`], with their commit.
const ATTESTATION_ENV: &str = "      - id: attestation\n        env:\n          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}\n";
const COMMIT_ENV: &str = "      - id: commit\n        env:\n          HOSTKEEPER_COMMIT: ${{ inputs.hostkeeper_commit }}\n";

fn real() -> String {
    std::fs::read_to_string(workflows_dir().join(FILE)).expect("read set-desired.yml")
}

fn assert_client(text: &str, want: &[&str]) {
    assert_eq!(lint(FILE, text), want, "in:\n{text}");
}

/// The `id` of every step of the one job, in order (`-` for a step with none).
fn step_ids(text: &str) -> Vec<String> {
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
fn moved(text: &str, from: &str, to: &str, before: &str) -> String {
    let start = text.find(from).expect("the block's first line");
    let end = start + text[start..].find(to).expect("the line after the block");
    let block = text[start..end].to_owned();
    let without = format!("{}{}", &text[..start], &text[end..]);
    with_in(&without, before, &format!("{block}{before}"))
}

#[test]
fn the_oidc_request_comes_after_every_verification_step() {
    // Catches: the token requested before the release is proven, in the real file (by
    // reordering its steps) and in a small one.
    let text = real();
    assert_client(&text, &[]);
    let ids = step_ids(&text);
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    for id in VERIFY_STEPS {
        assert!(at(TOKEN_STEP) > at(id), "{TOKEN_STEP} before {id}: {ids:?}");
    }
    assert_client(CLIENT, &[]);
    let early = with_in(CLIENT, TOKEN_BLOCK, "");
    let early = with_in(
        &early,
        "      - id: checksum\n",
        &format!("{TOKEN_BLOCK}      - id: checksum\n"),
    );
    assert_client(
        &early,
        &[
            "the OIDC token step `oidc-token` (step 1) comes before the verification step `checksum` (step 2)",
            "the OIDC token step `oidc-token` (step 1) comes before the verification step `attestation` (step 3)",
            "the OIDC token step `oidc-token` (step 1) comes before the verification step `commit` (step 4)",
            "step 1 (`oidc-token`) comes before the last verification step and names the OIDC token request",
        ],
    );
    // The real file with its token step moved to just before the checksum: the inputs and the
    // download are steps 1 and 2.
    let reordered = moved(
        &text,
        "      - name: Request the OIDC token\n",
        "      - name: Set the desired version\n",
        "      # Verification 1 of 3",
    );
    assert_client(
        &reordered,
        &[
            "the OIDC token step `oidc-token` (step 3) comes before the verification step `checksum` (step 4)",
            "the OIDC token step `oidc-token` (step 3) comes before the verification step `attestation` (step 5)",
            "the OIDC token step `oidc-token` (step 3) comes before the verification step `commit` (step 6)",
            "step 3 (`oidc-token`) comes before the last verification step and names the OIDC token request",
        ],
    );
}

#[test]
fn nothing_before_verification_touches_the_token_or_ctl() {
    // Catches: a step before the last verification that runs ctl, reads the token request
    // variables, or uses an action (which can read them); the job's `env` naming them; and a
    // verification or token step that the job could run on past.
    let commit = "      - id: commit\n";
    let cases: [(&str, &[&str]); 3] = [
        (
            "      - id: early\n        run: hostkeeper ctl releases converge --service s\n",
            &["step 3 (`early`) comes before the last verification step and runs ctl"],
        ),
        (
            "      - id: early\n        run: echo \"$ACTIONS_ID_TOKEN_REQUEST_TOKEN\"\n",
            &[
                "step 3 (`early`) comes before the last verification step and names the OIDC token request",
            ],
        ),
        (
            "      - uses: actions/github-script@60a0d83039c74a4aee543508d2ffcb1c3799cdea # v7.0.1\n        with:\n          script: core.getIDToken()\n",
            &[
                "step 3 (`unnamed`) comes before the last verification step and uses an action, which can read the OIDC token request",
                "step 3 (`unnamed`) comes before the last verification step and names the OIDC token request",
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
        &["step 3 (`commit`) has `continue-on-error`, which could run the job on past it"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "      - id: oidc-token\n",
            "      - id: oidc-token\n        if: ${{ always() }}\n",
        ),
        &["step 4 (`oidc-token`) has `if`, which could run the job on past it"],
    );
    // The order is read from the one step with the token step's id: a second one is refused.
    assert_client(
        &with_in(
            CLIENT,
            "      - id: set-desired\n",
            &format!("{TOKEN_BLOCK}      - id: set-desired\n"),
        ),
        &["more than one step has `id: oidc-token`"],
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
        &["line 15: `secrets: inherit` hands every secret of the caller on"],
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
        ["line 17: `secrets: inherit` hands every secret of the caller on"]
    );
}

#[test]
fn the_verification_step_names_the_signer_workflow_repo_and_ref() {
    // Catches: the attestation accepted from another repository, another workflow, another ref
    // or another commit, by a dropped, changed, repeated or widened flag; and the commit
    // comparison made against anything but `hostkeeper_commit`.
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
    assert_client(
        &with_in(CLIENT, "sourceRepositoryDigest", "sourceRepositoryRef"),
        &[
            "the commit step does not compare `sourceRepositoryDigest` with `\"$HOSTKEEPER_COMMIT\"`",
        ],
    );
}

#[test]
fn the_poll_is_bounded_by_an_iteration_count_input() {
    // Catches: a poll with no bound, a bound that is not `max_polls`, and an unbounded loop in
    // any other step.
    assert_client(&real(), &[]);
    assert!(real().contains(POLL_LOOP));
    let header = format!("          {POLL_LOOP}\n");
    let no_loop = format!("the poll step has no loop `{POLL_LOOP}`");
    let no_loop = no_loop.as_str();
    let other_for = format!("step 6 (`converge`) has a `for ((` loop other than `{POLL_LOOP}`");
    let other_for = other_for.as_str();
    let cases: [(&str, Vec<&str>); 4] = [
        (
            "          while true; do\n",
            vec![
                no_loop,
                "step 6 (`converge`) has a `while` loop, which no count bounds",
            ],
        ),
        (
            "          until false; do\n",
            vec![
                no_loop,
                "step 6 (`converge`) has a `until` loop, which no count bounds",
            ],
        ),
        ("          for ((;;)); do\n", vec![no_loop, other_for]),
        (
            "          for ((poll = 1; poll <= 100000; poll++)); do\n",
            vec![no_loop, other_for],
        ),
    ];
    for (line, want) in cases {
        assert_client(&with_in(CLIENT, &header, line), &want);
    }
    assert_client(
        &with_in(
            CLIENT,
            "MAX_POLLS: ${{ inputs.max_polls }}",
            "MAX_POLLS: 100000",
        ),
        &["the poll step's `MAX_POLLS` is not `${{ inputs.max_polls }}`"],
    );
    assert_client(
        &with_in(CLIENT, "        type: number\n", "        type: string\n"),
        &["no input `max_polls` of `type: number`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "run: hostkeeper ctl --token-file token releases set-desired --service s",
            "run: while true; do hostkeeper ctl --token-file token releases set-desired --service s; done",
        ),
        &["step 5 (`set-desired`) has a `while` loop, which no count bounds"],
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
