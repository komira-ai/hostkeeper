//! The rules of `farm.yml` ([`FILE`]), the farm lane: `buck2 build //:ci` on the build farm,
//! additive and advisory beside ci.yml, which stays the pull-request gate.
//!
//! It is the one workflow that runs for a pull request with a job that holds a write scope: its
//! job [`JOB`] holds `id-token: write`, so that komira's farm-connect action ([`FARM_CONNECT`])
//! can join the farm's private network with the job's OIDC token (workload identity federation:
//! no stored secret). [`super::lint`] allows that scope to this job alone, under its exact `if:`
//! ([`JOB_IF`]: a push to main, or a pull request from a branch of this repository that is not
//! Dependabot's; a fork's pull request gets no token and is skipped), and lets this file, and no
//! other, pin farm-connect by commit with no version comment (komira tags no release of it).
//! Every other rule of [`super::lint`] holds for the file as for any pull-request workflow: no
//! secret and no token named, no other write scope, no root, no macOS, a Linux x86_64 runner.
//!
//! [`problems`] holds the file to its shape: the triggers, the one job, its rights, farm-connect's
//! inputs and the check of the build. It does not fix the other steps of the job, each of which
//! could ask for the token; a change to them is reviewed like any change to a workflow:
//! - its `on` is exactly `pull_request` with no filter and `push` to `branches: [main]`;
//! - its jobs are exactly [`JOB`], on [`RUNNER`], with `permissions` exactly `contents: read`
//!   and `id-token: write`;
//! - farm-connect is used by exactly one step, whose `with:` is exactly [`CONNECT_INPUTS`]: the
//!   tailnet identity (`TS_CLIENT_ID`, `TS_AUDIENCE`) and the farm's address, instance, worker
//!   property set and TLS switch, each from the organisation's variable of its name, and nothing
//!   else (no secret, no token, no input a pull request could set);
//! - a step after farm-connect's runs exactly [`BUILD_RUN`]: `cargo xtask farm-build` of `//:ci`,
//!   which refuses a build with no farm workers, a stale generated build table (whose fix it
//!   writes as a patch to the file [`patch_file`] names) or a build whose summary line does not say
//!   `local: 0`, and runs a build whose only failures are cut connections once more
//!   (crates/xtask/src/farm.rs);
//! - exactly one step after the build uploads the artifact [`PATCH_ARTIFACT`], under exactly
//!   [`UPLOAD_IF`] (the build failed: the patch exists only then), with `actions/upload-artifact`
//!   and a `with:` of exactly [`upload_inputs`]: that artifact name and the build's patch file, so
//!   the command the job summary prints downloads what was uploaded.

use std::collections::BTreeMap;

use super::lint::{entries, entry, triggers};
use super::yaml::{Node, Value};

/// The workflow.
pub const FILE: &str = "farm.yml";
/// Its one job, the one that may hold `id-token: write` on a pull request.
pub const JOB: &str = "build";
/// The runner of [`JOB`].
pub const RUNNER: &str = "ubuntu-24.04";
/// The exact `if:` of [`JOB`].
pub const JOB_IF: &str = "${{ github.event_name == 'push' || (github.event.pull_request.head.repo.full_name == github.repository && !startsWith(github.head_ref, 'dependabot/')) }}";
/// komira's action that joins the farm's network and writes the farm's buckconfig.
pub const FARM_CONNECT: &str = "komira-ai/komira/.github/actions/farm-connect";
/// The `with:` of the farm-connect step, exactly.
pub const CONNECT_INPUTS: [(&str, &str); 6] = [
    ("ts-client-id", "${{ vars.TS_CLIENT_ID }}"),
    ("ts-audience", "${{ vars.TS_AUDIENCE }}"),
    ("farm-address", "${{ vars.FARM_ADDRESS }}"),
    ("farm-instance", "${{ vars.FARM_INSTANCE }}"),
    (
        "farm-properties",
        "${{ vars.FARM_LINUX_X86_64_PROPERTIES }}",
    ),
    ("tls", "${{ vars.FARM_TLS || 'false' }}"),
];
/// The build step's `run:`.
pub const BUILD_RUN: &str = "cargo xtask farm-build --buck2 \"$RUNNER_TEMP/bin/buck2\" --target //:ci --patch \"$RUNNER_TEMP/buildrules-sync/buildrules-sync.patch\" --summary \"$GITHUB_STEP_SUMMARY\" --repo \"$GITHUB_REPOSITORY\" --run-id \"$GITHUB_RUN_ID\"";
/// The artifact a stale table's patch is uploaded as, and the patch's file name: the names the
/// command in the job summary downloads (`PATCH_ARTIFACT` and `PATCH_FILE` of
/// crates/xtask/src/farm/table.rs, whose test holds that command to them).
pub const PATCH_ARTIFACT: &str = "buildrules-sync";
pub const PATCH_FILE: &str = "buildrules-sync.patch";
/// The action that uploads the patch, at any commit.
pub const UPLOAD: &str = "actions/upload-artifact@";
/// The exact `if:` of the upload of the patch.
pub const UPLOAD_IF: &str = "${{ failure() }}";

/// The patch file the build writes, below the runner's temporary directory, as the build step's
/// shell names it (`shell`) and as the upload's `with:` does.
pub fn patch_file(shell: bool) -> String {
    let temp = if shell {
        "$RUNNER_TEMP"
    } else {
        "${{ runner.temp }}"
    };
    format!("{temp}/{PATCH_ARTIFACT}/{PATCH_FILE}")
}

/// The `with:` of the upload of the patch, exactly.
pub fn upload_inputs() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("name", PATCH_ARTIFACT.to_owned()),
        ("path", patch_file(false)),
        ("if-no-files-found", "ignore".to_owned()),
        ("retention-days", "30".to_owned()),
    ])
}

/// What a [`FILE`] whose `on` is anything else is refused with.
pub const ON_PROBLEM: &str =
    "farm.yml: on must be pull_request with no filter and push to branches [main] alone";

/// Whether the job `name` of `file` may hold `id-token: write` (under [`JOB_IF`]).
pub fn id_token_job(file: &str, name: &str) -> bool {
    file == FILE && name == JOB
}

/// Whether the `uses:` value `value` in `file` is farm-connect pinned where it may be.
pub fn unversioned_pin_allowed(file: &str, value: &str) -> bool {
    file == FILE
        && value
            .strip_prefix(FARM_CONNECT)
            .is_some_and(|rest| rest.starts_with('@'))
}

/// The scalars of the mapping `node`, by key; `None` when a key or value is not a scalar.
fn scalar_map(node: &Node) -> Option<BTreeMap<&str, &str>> {
    match &node.value {
        Value::Map(entries) => entries
            .iter()
            .map(|(k, v)| Some((k.as_str()?, v.as_str()?)))
            .collect(),
        _ => None,
    }
}

/// Whether `on` is exactly `pull_request` with no filter and `push` with `branches: [main]`.
fn pull_request_and_push_to_main(on: Option<&Node>) -> bool {
    let Some(on) = on.filter(|on| matches!(on.value, Value::Map(_))) else {
        return false;
    };
    let mut names = triggers(on);
    names.sort();
    let unfiltered = entry(on, "pull_request").is_some_and(|v| {
        v.is_empty_map() || (matches!(v.value, Value::Scalar { .. }) && v.as_str().is_none())
    });
    let to_main = entry(on, "push").is_some_and(|push| match entries(push) {
        [(key, branches)] => key.as_str() == Some("branches") && only_main(branches),
        _ => false,
    });
    names == ["pull_request", "push"] && unfiltered && to_main
}

/// Whether `branches` is the list `[main]`.
fn only_main(branches: &Node) -> bool {
    match &branches.value {
        Value::Seq(items) => matches!(&items[..], [only] if only.as_str() == Some("main")),
        _ => false,
    }
}

/// The steps of `job`.
fn steps(job: &Node) -> &[Node] {
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(items)) => items,
        _ => &[],
    }
}

/// What is wrong with the shape of [`FILE`], the document `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !pull_request_and_push_to_main(entry(doc, "on")) {
        problems.push(ON_PROBLEM.to_owned());
    }
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    if !jobs.iter().any(|(name, _)| name.as_str() == Some(JOB)) {
        problems.push(format!("{FILE}: no job {JOB}"));
    }
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        if name != JOB {
            problems.push(format!("job {name} is not a job of {FILE}"));
            continue;
        }
        problems.extend(job_problems(job));
    }
    problems
}

/// What is wrong with the job [`JOB`].
fn job_problems(job: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if entry(job, "runs-on").and_then(Node::as_str) != Some(RUNNER) {
        problems.push(format!("job {JOB}: runs-on must be {RUNNER}"));
    }
    let permissions = entry(job, "permissions").and_then(scalar_map);
    let wanted = BTreeMap::from([("contents", "read"), ("id-token", "write")]);
    if permissions.as_ref() != Some(&wanted) {
        problems.push(format!(
            "job {JOB}: permissions must be exactly contents: read and id-token: write"
        ));
    }
    let steps = steps(job);
    let connects: Vec<&Node> = steps
        .iter()
        .filter(|s| {
            entry(s, "uses")
                .and_then(Node::as_str)
                .is_some_and(|u| unversioned_pin_allowed(FILE, u))
        })
        .collect();
    let inputs = BTreeMap::from(CONNECT_INPUTS);
    match &connects[..] {
        [only] if entry(only, "with").and_then(scalar_map).as_ref() == Some(&inputs) => {}
        [_] => problems.push(format!(
            "job {JOB}: the farm-connect step's `with:` must be exactly {CONNECT_INPUTS:?}"
        )),
        _ => problems.push(format!(
            "job {JOB}: exactly one step must use {FARM_CONNECT}"
        )),
    }
    let connect = steps.iter().position(|s| {
        entry(s, "uses")
            .and_then(Node::as_str)
            .is_some_and(|u| unversioned_pin_allowed(FILE, u))
    });
    let build = steps
        .iter()
        .position(|s| entry(s, "run").and_then(Node::as_str) == Some(BUILD_RUN));
    match (connect, build) {
        (_, None) => problems.push(format!("job {JOB}: no step runs `{BUILD_RUN}`")),
        (Some(c), Some(b)) if c < b => {}
        _ => problems.push(format!(
            "job {JOB}: the build does not come after the farm-connect step"
        )),
    }
    problems.extend(upload_problems(steps, build));
    problems
}

/// Whether `step` uploads the artifact [`PATCH_ARTIFACT`].
fn uploads_patch(step: &Node) -> bool {
    entry(step, "uses")
        .and_then(Node::as_str)
        .is_some_and(|u| u.starts_with(UPLOAD))
        && entry(step, "with")
            .and_then(|with| entry(with, "name"))
            .and_then(Node::as_str)
            == Some(PATCH_ARTIFACT)
}

/// What is wrong with the upload of the patch among the steps `all`, the build at `build`.
fn upload_problems(all: &[Node], build: Option<usize>) -> Vec<String> {
    let uploads: Vec<usize> = (0..all.len()).filter(|&i| uploads_patch(&all[i])).collect();
    let [at] = uploads[..] else {
        return vec![format!(
            "job {JOB}: exactly one step must upload the artifact {PATCH_ARTIFACT} with \
             {UPLOAD}<commit>"
        )];
    };
    let step = &all[at];
    let mut problems = Vec::new();
    if build.is_some_and(|b| b > at) {
        problems.push(format!(
            "job {JOB}: the upload of {PATCH_ARTIFACT} does not come after the build"
        ));
    }
    if entry(step, "if").and_then(Node::as_str) != Some(UPLOAD_IF) {
        problems.push(format!(
            "job {JOB}: the upload of {PATCH_ARTIFACT} must run under exactly `if: {UPLOAD_IF}`"
        ));
    }
    let wanted = upload_inputs();
    let wanted: BTreeMap<&str, &str> = wanted.iter().map(|(k, v)| (*k, v.as_str())).collect();
    if entry(step, "with").and_then(scalar_map).as_ref() != Some(&wanted) {
        problems.push(format!(
            "job {JOB}: the upload of {PATCH_ARTIFACT}'s `with:` must be exactly {wanted:?}"
        ));
    }
    problems
}
