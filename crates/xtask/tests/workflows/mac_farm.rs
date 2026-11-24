//! The macOS programs of `artifacts.yml` compiled on a Mac of the build farm (docs/design/mac-farm.md).
//! Where the farm is configured, [`CALLER_JOB`] of artifacts.yml calls [`FILE`], whose one job
//! [`JOB`] builds the four programs on a farm Mac and uploads them as the artifact `sign-code`
//! signs; everywhere else [`HOSTED_JOB`] (`compile-macos`, on a GitHub-hosted Mac) does, under the
//! negation of the same condition ([`super::code_signing`] holds its upload, and the needs of
//! `sign-code` and `build-macos`).
//!
//! [`JOB`] holds `id-token: write`, for the tailnet join to the farm, and runs repository code.
//! It is in a file of its own so that its token is [`FILE`]'s: the signer of what a token signs
//! is the workflow it was minted in, so this job can sign nothing that names artifacts.yml, the
//! one signer hosts accept, and `attest` stays the one job whose token can.
//!
//! [`artifacts_problems`] refuses artifacts.yml when:
//! - [`HOSTED_JOB`]'s `if:` is not exactly [`HOSTED_IF`], or [`CALLER_JOB`]'s is not exactly
//!   [`FARM_IF`]: the two conditions are each other's negation (a test evaluates them), so exactly
//!   one of the two jobs runs, and the farm, the one place a token is granted, is used only in
//!   this repository;
//! - [`CALLER_JOB`] has a key outside [`CALLER_KEYS`] (a `secrets`, `strategy` or `runs-on`),
//!   needs other than exactly `version`, grants other rights than exactly [`PERMISSIONS`], calls
//!   another workflow than [`CALL`], or passes another `with:` than [`CALL_WITH`];
//! - a job other than [`CALLER_JOB`] calls [`CALL`]: a local call anywhere else is refused by
//!   [`super::lint`]'s pin rule, which lets this one through ([`local_call_allowed`]).
//!
//! [`file_problems`] refuses [`FILE`] when:
//! - it has a top-level key outside [`TOP_LEVEL`] (an `env:` or `defaults:` beside `jobs` would
//!   change how the job that holds the token runs);
//! - its `on` is not exactly `workflow_call` with the one input [`INPUT`] (a required string
//!   with a description): no push, dispatch or schedule runs it by itself;
//! - its jobs are not exactly [`JOB`]; [`JOB`]'s `if:` is not exactly [`FARM_IF`] (which
//!   [`super::lint::WRITE_ALLOWED`] gives its `id-token: write` under, as it gives the caller's),
//!   it runs on another runner than [`RUNNER`], has another timeout than [`TIMEOUT_MINUTES`], a
//!   job key outside [`JOB_KEYS`] (an `environment`, `container`, `services`, `outputs` or
//!   `strategy`), another `env:` than [`ENV`], or token rights other than exactly
//!   [`PERMISSIONS`];
//! - a step of [`JOB`] has a key outside [`STEP_KEYS`], or its steps are not exactly [`STEPS`],
//!   in order, each as [`shape`] writes it: the checkout with no credential kept, the toolchain,
//!   the release's version, xtask built before the tailnet join (so no build script runs on the
//!   farm's network), the pinned tailscale join as `tag:ci` with the run's OIDC token and the
//!   organisation's tailnet identity (Tailscale's own version and digest pinned, no cache), the one
//!   `cargo xtask mac-farm` that builds the four programs with the commit's `RELEASE_COMMIT` and
//!   brings each back with `--output`, its farm settings from the organisation's variables
//!   `FARM_*` through `env:`, and the upload of the four programs as `sign-code`'s artifact. So no
//!   other action or command runs while the job holds the token, and no secret is named.

use std::collections::BTreeMap;

use super::lint::{entries, entry, triggers};
use super::release::{job, names, text_of};
use super::yaml::{Node, Value};

/// The reusable workflow of the farm build.
pub const FILE: &str = "mac-farm.yml";

/// Its one job.
pub const JOB: &str = "compile";

/// The job of artifacts.yml that calls [`FILE`].
pub const CALLER_JOB: &str = "compile-macos-farm";

/// The job of artifacts.yml that compiles the programs on a GitHub-hosted Mac.
pub const HOSTED_JOB: &str = super::code_signing::COMPILE_JOB;

/// The `uses:` of [`CALLER_JOB`]: [`FILE`] of the same commit.
pub const CALL: &str = "./.github/workflows/mac-farm.yml";

/// The one input of [`FILE`]: the release version the workspace takes.
pub const INPUT: &str = "version";

/// The `with:` of [`CALLER_JOB`], exactly.
pub const CALL_WITH: [(&str, &str); 1] = [("version", "${{ needs.version.outputs.version }}")];

/// The `if:` of [`CALLER_JOB`] and of [`JOB`]: the farm is configured and the file runs in this
/// repository.
pub const FARM_IF: &str =
    "${{ vars.FARM_ADDRESS != '' && github.repository == 'komira-ai/hostkeeper' }}";

/// The `if:` of [`HOSTED_JOB`]: the negation of [`FARM_IF`].
pub const HOSTED_IF: &str =
    "${{ vars.FARM_ADDRESS == '' || github.repository != 'komira-ai/hostkeeper' }}";

/// The keys [`CALLER_JOB`] may have.
pub const CALLER_KEYS: [&str; 6] = ["if", "name", "needs", "permissions", "uses", "with"];

/// The runner of [`JOB`]: the Mac is the farm's, the job a hosted Linux runner's.
pub const RUNNER: &str = "ubuntu-24.04";

/// The timeout of [`JOB`].
pub const TIMEOUT_MINUTES: &str = "60";

/// The keys [`JOB`] may have.
pub const JOB_KEYS: [&str; 7] = [
    "env",
    "if",
    "permissions",
    "runs-on",
    "steps",
    "timeout-minutes",
    "name",
];

/// The `env:` of [`JOB`], exactly.
pub const ENV: [(&str, &str); 1] = [("RELEASE_VERSION", "${{ inputs.version }}")];

/// The token rights of [`JOB`] and of [`CALLER_JOB`], exactly: the checkout, and the OIDC token
/// of the tailnet join.
pub const PERMISSIONS: [(&str, &str); 2] = [("contents", "read"), ("id-token", "write")];

/// The keys a step of [`JOB`] may have.
pub const STEP_KEYS: [&str; 6] = ["env", "name", "run", "timeout-minutes", "uses", "with"];

/// The steps of [`JOB`], in order, as [`shape`] writes them.
pub const STEPS: [&str; 7] = [
    "uses actions/checkout with persist-credentials=false",
    "run rustup toolchain install",
    "run cargo xtask release set-version --version \"$RELEASE_VERSION\"",
    "run cargo xtask --help",
    "uses tailscale/github-action with audience=${{ vars.TS_AUDIENCE }} with oauth-client-id=${{ vars.TS_CLIENT_ID }} with sha256sum=c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32 with tags=tag:ci with use-cache=false with version=1.94.2",
    concat!(
        "run cargo xtask mac-farm --address \"$FARM_ADDRESS\" --instance \"$FARM_INSTANCE\" --tls \"$FARM_TLS\" --properties \"$FARM_PROPERTIES\" --scratch \"$RUNNER_TEMP/mac-farm\" --summary \"$GITHUB_STEP_SUMMARY\" --env \"RELEASE_COMMIT=$RELEASE_COMMIT\" --output target/release/hostkeeper --output target/release/hk --output target/release/hello --output target/release/hostkeeper-bootstrap -- build --locked --release -p hostkeeper --bin hostkeeper -p hostkeeper-hk --bin hk -p hello --bin hello -p hostkeeper-bootstrap --bin hostkeeper-bootstrap",
        " env FARM_ADDRESS=${{ vars.FARM_ADDRESS }} env FARM_INSTANCE=${{ vars.FARM_INSTANCE }} env FARM_PROPERTIES=${{ vars.FARM_DARWIN_ARM64_PROPERTIES }} env FARM_TLS=${{ vars.FARM_TLS || 'false' }} env RELEASE_COMMIT=${{ github.sha }} timeout-minutes 50",
    ),
    "uses actions/upload-artifact with if-no-files-found=error with name=macho-unsigned-macos-arm64 with path=target/release/hostkeeper\ntarget/release/hk\ntarget/release/hello\ntarget/release/hostkeeper-bootstrap\n with retention-days=90",
];

/// The top-level keys of [`FILE`], exactly: a workflow-level `env:` or `defaults:` would change
/// how every step of the job that holds the token runs.
pub const TOP_LEVEL: [&str; 4] = ["jobs", "name", "on", "permissions"];

/// What a [`FILE`] whose `on` is anything else is refused with.
pub const ON_PROBLEM: &str = "mac-farm.yml: on must be workflow_call with exactly the input `version`: `type: string`, `required: true` and a description";

/// Whether the `uses:` value `value` in `file` is the one local call the pin rule lets through:
/// [`CALL`] in artifacts.yml ([`artifacts_problems`] holds it to [`CALLER_JOB`]).
pub fn local_call_allowed(file: &str, value: &str) -> bool {
    file == "artifacts.yml" && value == CALL
}

/// Whether the job `name` of `file` is [`CALLER_JOB`] calling [`CALL`]: it has no runner of its
/// own, the called job's is held by [`file_problems`].
pub fn calls_farm(file: &str, name: &str, job: &Node) -> bool {
    file == "artifacts.yml" && name == CALLER_JOB && text_of(Some(job), "uses") == Some(CALL)
}

/// The scalar entries of the mapping `key` of `node`, sorted by key.
fn sorted(node: &Node, key: &str) -> BTreeMap<String, String> {
    entry(node, key)
        .map_or(&[][..], entries)
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), v.as_str()?.to_owned())))
        .collect()
}

/// Whether the mapping `key` of `node` is exactly `want`.
fn exactly(node: &Node, key: &str, want: &[(&str, &str)]) -> bool {
    sorted(node, key)
        == want
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
}

/// The keys of the mapping `node` outside `allowed`.
fn other_keys<'a>(node: &'a Node, allowed: &[&str]) -> Vec<&'a str> {
    entries(node)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .filter(|k| !allowed.contains(k))
        .collect()
}

/// The step as [`STEPS`] writes it: its `run:` or its action (without the pin, which
/// [`super::lint`] holds), then its `with:` and its `env:`, each sorted by key, then its
/// timeout. Its name is free.
pub fn shape(step: &Node) -> String {
    let mut shape = match text_of(Some(step), "run") {
        Some(run) => format!("run {}", run.trim_end()),
        None => {
            let action = text_of(Some(step), "uses").unwrap_or_default();
            format!(
                "uses {}",
                action.split_once('@').map_or(action, |(path, _)| path)
            )
        }
    };
    for key in ["with", "env"] {
        for (k, v) in sorted(step, key) {
            shape.push_str(&format!(" {key} {k}={v}"));
        }
    }
    if let Some(minutes) = text_of(Some(step), "timeout-minutes") {
        shape.push_str(&format!(" timeout-minutes {minutes}"));
    }
    shape
}

/// One line per rule of this module that `doc`, artifacts.yml, breaks.
pub fn artifacts_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if text_of(job(doc, HOSTED_JOB), "if") != Some(HOSTED_IF) {
        problems.push(format!(
            "artifacts.yml: job {HOSTED_JOB} needs the exact `if: {HOSTED_IF}`"
        ));
    }
    for (name, j) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        if name != CALLER_JOB && text_of(Some(j), "uses") == Some(CALL) {
            problems.push(format!(
                "artifacts.yml: job {name} calls {FILE}; only {CALLER_JOB} may"
            ));
        }
    }
    let Some(j) = job(doc, CALLER_JOB) else {
        problems.push(format!("artifacts.yml: has no job {CALLER_JOB}"));
        return problems;
    };
    let who = format!("artifacts.yml: job {CALLER_JOB}");
    if text_of(Some(j), "if") != Some(FARM_IF) {
        problems.push(format!("{who} needs the exact `if: {FARM_IF}`"));
    }
    for key in other_keys(j, &CALLER_KEYS) {
        problems.push(format!("{who} has the key {key}"));
    }
    if names(entry(j, "needs")) != ["version"] {
        problems.push(format!("{who} must need exactly [\"version\"]"));
    }
    if !exactly(j, "permissions", &PERMISSIONS) {
        problems.push(format!(
            "{who} must grant exactly `contents: read` and `id-token: write`"
        ));
    }
    if text_of(Some(j), "uses") != Some(CALL) {
        problems.push(format!("{who} must call {CALL}"));
    }
    if !exactly(j, "with", &CALL_WITH) {
        problems.push(format!("{who} must pass exactly {CALL_WITH:?}"));
    }
    problems
}

/// Whether `on` is exactly `workflow_call` with the one input [`INPUT`].
fn called_only(on: Option<&Node>) -> bool {
    let Some(on) = on.filter(|on| matches!(on.value, Value::Map(_))) else {
        return false;
    };
    let Some(call) = entry(on, "workflow_call") else {
        return false;
    };
    let inputs = entry(call, "inputs");
    let input = inputs.and_then(|i| entry(i, INPUT));
    triggers(on) == ["workflow_call"]
        && other_keys(call, &["inputs"]).is_empty()
        && inputs.is_some_and(|i| other_keys(i, &[INPUT]).is_empty())
        && input.is_some_and(|i| {
            other_keys(i, &["description", "required", "type"]).is_empty()
                && text_of(Some(i), "type") == Some("string")
                && text_of(Some(i), "required") == Some("true")
                && text_of(Some(i), "description").is_some_and(|d| !d.trim().is_empty())
        })
}

/// One line per rule of this module that `doc`, [`FILE`], breaks.
pub fn file_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !called_only(entry(doc, "on")) {
        problems.push(ON_PROBLEM.to_owned());
    }
    for key in other_keys(doc, &TOP_LEVEL) {
        problems.push(format!("{FILE}: has the top-level key {key}"));
    }
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    for (name, _) in jobs {
        let name = name.as_str().unwrap_or_default();
        if name != JOB {
            problems.push(format!("job {name} is not a job of {FILE}"));
        }
    }
    let Some(j) = job(doc, JOB) else {
        problems.push(format!("{FILE}: has no job {JOB}"));
        return problems;
    };
    let who = format!("{FILE}: job {JOB}");
    if text_of(Some(j), "if") != Some(FARM_IF) {
        problems.push(format!("{who} needs the exact `if: {FARM_IF}`"));
    }
    if text_of(Some(j), "runs-on") != Some(RUNNER) {
        problems.push(format!("{who} must run on {RUNNER}"));
    }
    if text_of(Some(j), "timeout-minutes") != Some(TIMEOUT_MINUTES) {
        problems.push(format!(
            "{who} must have `timeout-minutes: {TIMEOUT_MINUTES}`"
        ));
    }
    for key in other_keys(j, &JOB_KEYS) {
        problems.push(format!("{who} has the key {key}"));
    }
    if !exactly(j, "env", &ENV) {
        problems.push(format!("{who} must have exactly the env {ENV:?}"));
    }
    if !exactly(j, "permissions", &PERMISSIONS) {
        problems.push(format!(
            "{who} must hold exactly `contents: read` and `id-token: write`"
        ));
    }
    problems.extend(step_problems(&who, j));
    problems
}

/// What is wrong with the steps of [`JOB`], the job `j`.
fn step_problems(who: &str, j: &Node) -> Vec<String> {
    let steps = match entry(j, "steps").map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    let mut problems = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        for key in other_keys(step, &STEP_KEYS) {
            problems.push(format!("{who} step {} has the key {key}", at + 1));
        }
    }
    if steps.len() != STEPS.len() {
        problems.push(format!(
            "{who} has {} steps, not the {} of mac_farm::STEPS",
            steps.len(),
            STEPS.len()
        ));
    }
    for (at, (step, want)) in steps.iter().zip(STEPS).enumerate() {
        let got = shape(step);
        if got != want {
            problems.push(format!("{who} step {} is `{got}`, not `{want}`", at + 1));
        }
    }
    problems
}
