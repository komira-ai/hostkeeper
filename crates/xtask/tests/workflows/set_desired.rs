//! The rules `set-desired.yml`, the CI client a service's release job calls (design, section
//! 4.5), is held to beyond those of every workflow. They fix what makes the client safe: the
//! hostkeeper release is proven before anything can hold an OIDC token or run hostkeeper.
//!
//! [`problems`] refuses the workflow when:
//! - `on` is anything but `workflow_call` alone, or the call declares `secrets`;
//! - it has `inputs.max_polls` of another type than `number`, or none;
//! - it has another job than the one job [`JOB`], or that job's `permissions` is not exactly
//!   `contents: read` and `id-token: write` (the top-level `permissions: {}` is a rule of every
//!   workflow);
//! - a step of [`ORDERED`] is missing or there twice (by `id`), or has an `if:` or a
//!   `continue-on-error` (either would let the job run on past a failed check);
//! - the client step [`CLIENT_STEP`], the first that requests an OIDC token, comes before a check
//!   of [`VERIFY_STEPS`];
//! - any step up to the last provenance check of [`PROVENANCE_STEPS`] uses an action (any action
//!   can read the token request variables once the job holds `id-token: write`), names the token
//!   request ([`TOKEN_WORDS`]), runs `ctl` or runs the hostkeeper program ([`PROGRAM`]); or the
//!   `env`, `defaults`, `container` or `services` of the workflow or of the job names the token
//!   request;
//! - the attestation step's `gh attestation verify` does not pass each flag of [`SIGNER`] once
//!   with exactly its value and [`DENY_SELF_HOSTED`] once, or passes a flag of [`WIDENING`], or
//!   its `HOSTKEEPER_COMMIT` is not [`COMMIT_INPUT`]; or the commit step does not compare the
//!   attested `sourceRepositoryDigest` with `env.HOSTKEEPER_COMMIT`, that same input;
//! - the client step does not run [`CLIENT_COMMAND`], or does not pass [`MAX_POLLS_FLAG`] exactly
//!   once, or its `MAX_POLLS` is not [`MAX_POLLS_INPUT`]; or any step's script has a loop
//!   (`while`, `until` or `for`): the one bounded loop is the client's, in Rust.
//!
//! Commands are read as text, line by line (a line ending in `\` continues on the next), and are
//! never run.

use super::lint::{entries, entry, has_word, scalars, triggers};
use super::yaml::{Node, Value};

/// The file these rules are for, in `.github/workflows`.
pub const FILE: &str = "set-desired.yml";
/// Its one job.
pub const JOB: &str = "set-desired";
/// The provenance checks, by `id`: the SHA256SUMS, the attestation, the attested commit. Until
/// the last of them nothing of ours may run.
pub const PROVENANCE_STEPS: [&str; 3] = ["checksum", "attestation", "commit"];
/// Every check the client step must come after: the provenance checks and the binary's version.
pub const VERIFY_STEPS: [&str; 4] = ["checksum", "attestation", "commit", "version"];
/// The step that runs the client, the first that requests an OIDC token.
pub const CLIENT_STEP: &str = "set-desired";
/// The steps the order rests on, in order. None may be skipped or run on past.
pub const ORDERED: [&str; 6] = [
    "checksum",
    "attestation",
    "commit",
    "unpack",
    "version",
    CLIENT_STEP,
];
/// The verified hostkeeper program, as the steps name it.
pub const PROGRAM: &str = "$RUNNER_TEMP/hostkeeper-bin/hostkeeper";
/// The command the client step runs: the program's CI client.
pub const CLIENT_COMMAND: [&str; 3] = [PROGRAM, "ci", "set-desired"];
/// What a step that asks for an OIDC token names: the runner's request variables, or the
/// toolkit's call.
pub const TOKEN_WORDS: [&str; 2] = ["ACTIONS_ID_TOKEN_REQUEST", "getIDToken"];
/// The flags of `gh attestation verify` that pin who signed the release and what it was built
/// from, each with its one value (docs/deploy/verify-a-release.md, step 3).
pub const SIGNER: [(&str, &str); 4] = [
    ("--repo", "komira-ai/hostkeeper"),
    (
        "--signer-workflow",
        "komira-ai/hostkeeper/.github/workflows/artifacts.yml",
    ),
    ("--source-ref", "refs/heads/main"),
    ("--source-digest", "$HOSTKEEPER_COMMIT"),
];
/// The flag of `gh attestation verify` that refuses a build on a self-hosted runner.
pub const DENY_SELF_HOSTED: &str = "--deny-self-hosted-runners";
/// Flags of `gh attestation verify` that would accept another signer than [`SIGNER`] names.
pub const WIDENING: [&str; 4] = [
    "--owner",
    "--signer-repo",
    "--cert-identity",
    "--cert-identity-regex",
];
/// The value of `HOSTKEEPER_COMMIT` in the attestation and commit steps.
pub const COMMIT_INPUT: &str = "${{ inputs.hostkeeper_commit }}";
/// The value of `MAX_POLLS` in the client step.
pub const MAX_POLLS_INPUT: &str = "${{ inputs.max_polls }}";
/// The flag that bounds the client's convergence reads, with its value.
pub const MAX_POLLS_FLAG: (&str, &str) = ("--max-polls", "$MAX_POLLS");

/// The text of a scalar entry of `map`, if there is one.
fn text<'a>(map: &'a Node, key: &str) -> Option<&'a str> {
    entry(map, key).and_then(Node::as_str)
}

/// How a step is named in a problem: its number from 1, and its `id`, else its `name`.
fn label(index: usize, step: &Node) -> String {
    let name = text(step, "id")
        .or_else(|| text(step, "name"))
        .unwrap_or("unnamed");
    format!("step {} (`{name}`)", index + 1)
}

/// The logical lines of a `run:` script: a line ending in `\` is joined with the next.
pub fn logical_lines(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = String::new();
    for line in script.lines() {
        match line.trim_end().strip_suffix('\\') {
            Some(start) => {
                open.push_str(start);
                open.push(' ');
            }
            None => {
                open.push_str(line);
                out.push(std::mem::take(&mut open));
            }
        }
    }
    if !open.is_empty() {
        out.push(open);
    }
    out
}

/// The words of a line, split on whitespace, each with one pair of surrounding quotes removed.
fn words(line: &str) -> Vec<&str> {
    line.split_whitespace()
        .map(|w| {
            ['"', '\'']
                .iter()
                .find_map(|q| w.strip_prefix(*q).and_then(|w| w.strip_suffix(*q)))
                .unwrap_or(w)
        })
        .collect()
}

/// Every value `flag` is given among `parts`, written `--flag value` or `--flag=value`.
fn values<'a>(parts: &[&'a str], flag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    for (i, w) in parts.iter().enumerate() {
        if *w == flag {
            out.push(parts.get(i + 1).copied().unwrap_or_default());
        } else if let Some(v) = w.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            out.push(v.trim_matches(['"', '\'']));
        }
    }
    out
}

/// One line per problem of the CI client workflow `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let on = entry(doc, "on");
    if on.map(triggers) != Some(vec!["workflow_call".to_owned()]) {
        problems.push("`on` is not `workflow_call` alone".to_owned());
    }
    let call = on.and_then(|on| entry(on, "workflow_call"));
    if call.and_then(|c| entry(c, "secrets")).is_some() {
        problems.push("`workflow_call` declares secrets".to_owned());
    }
    let max_polls = call
        .and_then(|c| entry(c, "inputs"))
        .and_then(|i| entry(i, "max_polls"));
    if max_polls.and_then(|m| text(m, "type")) != Some("number") {
        problems.push("no input `max_polls` of `type: number`".to_owned());
    }
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    let job = match jobs {
        [(name, job)] if name.as_str() == Some(JOB) => job,
        _ => {
            problems.push(format!("the workflow has not exactly one job, `{JOB}`"));
            return problems;
        }
    };
    let mut granted: Vec<(Option<&str>, Option<&str>)> = entry(job, "permissions")
        .map_or(&[][..], entries)
        .iter()
        .map(|(scope, level)| (scope.as_str(), level.as_str()))
        .collect();
    granted.sort();
    if granted
        != [
            (Some("contents"), Some("read")),
            (Some("id-token"), Some("write")),
        ]
    {
        problems.push(format!(
            "job {JOB}: `permissions` is not exactly `contents: read` and `id-token: write`"
        ));
    }
    for (owner, node) in [("workflow", doc), ("job", job)] {
        for key in ["env", "defaults", "container", "services"] {
            let mut values = Vec::new();
            if let Some(v) = entry(node, key) {
                scalars(v, &mut values);
            }
            if values.iter().any(|v| names_token(v)) {
                problems.push(format!(
                    "the {owner}'s `{key}` names the OIDC token request"
                ));
            }
        }
    }
    let steps = match entry(job, "steps") {
        Some(Node {
            value: Value::Seq(steps),
            ..
        }) => &steps[..],
        _ => &[][..],
    };
    problems.extend(step_problems(steps));
    problems
}

/// Whether `value` names the OIDC token request.
fn names_token(value: &str) -> bool {
    TOKEN_WORDS.iter().any(|w| value.contains(w))
}

/// The index of the one step with `id: id`, or the problem that there is not one.
fn position(steps: &[Node], id: &str) -> Result<usize, String> {
    let found: Vec<usize> = (0..steps.len())
        .filter(|&i| text(&steps[i], "id") == Some(id))
        .collect();
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no step has `id: {id}`")),
        _ => Err(format!("more than one step has `id: {id}`")),
    }
}

fn step_problems(steps: &[Node]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut at = Vec::new();
    for id in ORDERED {
        match position(steps, id) {
            Ok(i) => at.push((id, i)),
            Err(e) => problems.push(e),
        }
    }
    let index = |id: &str| at.iter().find(|(name, _)| *name == id).map(|(_, i)| *i);
    for &(_, i) in &at {
        for key in ["if", "continue-on-error"] {
            if entry(&steps[i], key).is_some() {
                problems.push(format!(
                    "{} has `{key}`, which could run the job on past it",
                    label(i, &steps[i])
                ));
            }
        }
    }
    if let Some(client) = index(CLIENT_STEP) {
        for id in VERIFY_STEPS {
            if let Some(v) = index(id).filter(|&v| v > client) {
                problems.push(format!(
                    "the OIDC token step `{CLIENT_STEP}` (step {}) comes before the verification step `{id}` (step {})",
                    client + 1,
                    v + 1
                ));
            }
        }
    }
    let proven = PROVENANCE_STEPS.iter().filter_map(|id| index(id)).max();
    for (i, step) in steps.iter().enumerate().take(proven.map_or(0, |v| v + 1)) {
        problems.extend(early_problems(&label(i, step), step));
    }
    if let Some(i) = index("attestation") {
        problems.extend(signer_problems(&steps[i]));
    }
    if let Some(i) = index("commit") {
        let step = &steps[i];
        if entry(step, "env").and_then(|e| text(e, "HOSTKEEPER_COMMIT")) != Some(COMMIT_INPUT) {
            problems.push(format!(
                "the commit step's `HOSTKEEPER_COMMIT` is not `{COMMIT_INPUT}`"
            ));
        }
        let run = text(step, "run").unwrap_or_default();
        if !(run.contains("sourceRepositoryDigest") && run.contains("env.HOSTKEEPER_COMMIT")) {
            problems.push(
                "the commit step does not compare `sourceRepositoryDigest` with `env.HOSTKEEPER_COMMIT`"
                    .to_owned(),
            );
        }
    }
    if let Some(i) = index(CLIENT_STEP) {
        problems.extend(client_problems(&steps[i]));
    }
    for (i, step) in steps.iter().enumerate() {
        let lines = logical_lines(text(step, "run").unwrap_or_default());
        for word in ["while", "until", "for"] {
            if lines.iter().any(|l| has_word(l, word)) {
                problems.push(format!(
                    "{} has a `{word}` loop: the one loop is the client's, bounded by `max_polls`",
                    label(i, step)
                ));
            }
        }
    }
    problems
}

/// What a step that runs before the provenance is proven must not do.
fn early_problems(who: &str, step: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if entry(step, "uses").is_some() {
        problems.push(format!(
            "{who} comes before the last verification step and uses an action, which can read the OIDC token request"
        ));
    }
    let mut values = Vec::new();
    scalars(step, &mut values);
    if values.iter().any(|v| names_token(v)) {
        problems.push(format!(
            "{who} comes before the last verification step and names the OIDC token request"
        ));
    }
    let lines = || values.iter().flat_map(|v| v.lines());
    if lines().any(|l| has_word(l, "ctl")) {
        problems.push(format!(
            "{who} comes before the last verification step and runs ctl"
        ));
    }
    if lines().any(|l| l.contains(PROGRAM)) {
        problems.push(format!(
            "{who} comes before the last verification step and runs the hostkeeper program"
        ));
    }
    problems
}

/// What is wrong with how the attestation step pins the signer and the source.
fn signer_problems(step: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if entry(step, "env").and_then(|e| text(e, "HOSTKEEPER_COMMIT")) != Some(COMMIT_INPUT) {
        problems.push(format!(
            "the attestation step's `HOSTKEEPER_COMMIT` is not `{COMMIT_INPUT}`"
        ));
    }
    let lines = logical_lines(text(step, "run").unwrap_or_default());
    let Some(verify) = lines.iter().find(|l| l.contains("gh attestation verify")) else {
        problems.push("the attestation step does not run `gh attestation verify`".to_owned());
        return problems;
    };
    let parts = words(verify);
    for (flag, want) in SIGNER {
        if values(&parts, flag) != [want] {
            problems.push(format!(
                "the attestation step does not pass `{flag} {want}` exactly once"
            ));
        }
    }
    if parts.iter().filter(|w| **w == DENY_SELF_HOSTED).count() != 1 {
        problems.push(format!(
            "the attestation step does not pass `{DENY_SELF_HOSTED}` exactly once"
        ));
    }
    for flag in WIDENING {
        if !values(&parts, flag).is_empty() {
            problems.push(format!(
                "the attestation step passes `{flag}`, which accepts another signer"
            ));
        }
    }
    problems
}

/// What is wrong with the client step: the command it runs and the bound of its polls.
fn client_problems(step: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let lines = logical_lines(text(step, "run").unwrap_or_default());
    let client = lines.iter().find(|l| words(l).starts_with(&CLIENT_COMMAND));
    let Some(client) = client else {
        problems.push(format!(
            "the client step does not run `{}`",
            CLIENT_COMMAND.join(" ")
        ));
        return problems;
    };
    let (flag, want) = MAX_POLLS_FLAG;
    if values(&words(client), flag) != [want] {
        problems.push(format!(
            "the client step does not pass `{flag} \"{want}\"` exactly once"
        ));
    }
    if entry(step, "env").and_then(|e| text(e, "MAX_POLLS")) != Some(MAX_POLLS_INPUT) {
        problems.push(format!(
            "the client step's `MAX_POLLS` is not `{MAX_POLLS_INPUT}`"
        ));
    }
    problems
}
