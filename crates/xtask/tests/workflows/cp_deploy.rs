//! The rules of the coordinator's deploy to the control plane in `release.yml`
//! (docs/deploy/control-plane.md): the job [`DIGEST_JOB`], which reads what the deploy is given
//! from the newest release of the run, and the job [`JOB`], which calls docker-swarm-updater's
//! reusable deploy workflow ([`DEPLOY_WORKFLOW`]) with it. [`problems`] refuses `release.yml`
//! when:
//! - either job lacks the exact `if:` [`DEPLOY_IF`] (a release of main, only while the
//!   repository variables `CP_DEPLOY_ENDPOINT` and `CP_DEPLOY_TAILSCALE_TAGS` are both not empty, so that the
//!   deploy is off until they are set), or has a key outside its list ([`DIGEST_KEYS`],
//!   [`JOB_KEYS`]): no `continue-on-error`, no `secrets`, no `strategy`, no `environment` of its
//!   own;
//! - [`DIGEST_JOB`] does not need exactly `plan` and `newest` (the newest release of the run,
//!   after every publish and image of it), does not run on [`RUNNER`] within
//!   [`TIMEOUT_MINUTES`], holds other token rights than exactly `contents: read`, has a step with
//!   a key outside [`STEP_KEYS`] (an `if:` that could skip it, a `continue-on-error`), or its
//!   steps are not exactly [`DIGEST_STEPS`], in that order, with their `with`, `env` and
//!   `working-directory` and `id` ([`step_shape`]): xtask compiled from the workflow's own commit, the
//!   release's files from the run's own `release-<tag>` artifact, checked against SHA256SUMS,
//!   and the digest and the addresses written by `cargo xtask release image-digest` from those
//!   files; or its `outputs` are not exactly [`DIGEST_OUTPUTS`];
//! - [`JOB`] does not need exactly `plan`, `newest` and [`DIGEST_JOB`], holds other token rights
//!   than exactly `id-token: write` (the called workflow asks GitHub for an OIDC token for every
//!   request, and needs nothing else), does not call [`DEPLOY_WORKFLOW`] (the commit the
//!   updater's allow-list names, so another one is refused there), or passes other inputs than
//!   exactly [`WITH`]: the service `hostkeeper-coordinator`, the newest release's version, the
//!   digest and addresses of [`DIGEST_JOB`], the endpoint, audience and tailnet join from the
//!   repository variables, the environment `controlplane` and a wait of 60 minutes. It passes
//!   no signer, so the deploy workflow's default, this repository's artifacts.yml on
//!   refs/heads/main, is the one the updater checks the bundle against.
//!
//! That the `uses:` line carries a version comment is the shared rule of every pin
//! ([`super::lint`]); that [`JOB`] may hold `id-token: write` under [`DEPLOY_IF`], and no other
//! job of the file under another `if:`, is [`super::lint::WRITE_ALLOWED`]; and a job of any
//! other name or file that calls a reusable workflow of another repository is refused for its
//! missing `runs-on` ([`calls_updater`] names the one exception).

use super::desired::{FILE, needs, pairs, step_shape as desired_step_shape, text};
use super::lint::{entries, entry};
use super::yaml::{Node, Value};

/// The job that reads what the deploy is given.
pub const DIGEST_JOB: &str = "cp-deploy-digest";

/// The job that calls [`DEPLOY_WORKFLOW`].
pub const JOB: &str = "cp-deploy";

/// docker-swarm-updater's reusable deploy workflow at the commit the updater's allow-list pins a
/// caller's `job_workflow_ref` to. Moving it is one reviewed change here and in that list.
pub const DEPLOY_WORKFLOW: &str = "komira-ai/docker-swarm-updater/.github/workflows/deploy.yml@c1fcc47610e2886d4d9e561be3c0c76f0452d1f6";

/// The `if:` of both jobs: a release of main ([`super::release::RELEASE_IF`]), while the
/// repository variables `CP_DEPLOY_ENDPOINT` (the deploy front's address) and
/// `CP_DEPLOY_TAILSCALE_TAGS` (the tags the deploy runner's tailnet node takes, without which the
/// join fails) are both not empty.
pub const DEPLOY_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.CP_DEPLOY_ENDPOINT != '' && vars.CP_DEPLOY_TAILSCALE_TAGS != '' }}";

/// The runner of [`DIGEST_JOB`].
pub const RUNNER: &str = "ubuntu-24.04";

/// The `timeout-minutes` of [`DIGEST_JOB`].
pub const TIMEOUT_MINUTES: &str = "20";

/// The keys [`DIGEST_JOB`] may have, sorted.
pub const DIGEST_KEYS: [&str; 7] = [
    "if",
    "needs",
    "outputs",
    "permissions",
    "runs-on",
    "steps",
    "timeout-minutes",
];

/// The keys [`JOB`] may have, sorted.
pub const JOB_KEYS: [&str; 5] = ["if", "needs", "permissions", "uses", "with"];

/// The keys a step of [`DIGEST_JOB`] may have: no `if:` that could skip it, no
/// `continue-on-error` that could let it fail, no `shell` that could change what its command
/// does.
pub const STEP_KEYS: [&str; 7] = [
    "env",
    "id",
    "name",
    "run",
    "uses",
    "with",
    "working-directory",
];

/// The steps of [`DIGEST_JOB`], in order, as [`step_shape`] writes them.
pub const DIGEST_STEPS: [&str; 6] = [
    "uses actions/checkout with persist-credentials=false",
    "run rustup toolchain install",
    "run cargo xtask --help",
    "uses actions/download-artifact with name=release-${{ needs.newest.outputs.tag }} with path=dist",
    "run sha256sum --check --strict SHA256SUMS working-directory dist",
    r#"run cargo xtask release image-digest dist --version "$VERSION" --repo "$GITHUB_REPOSITORY" --output "$GITHUB_OUTPUT" env VERSION=${{ needs.newest.outputs.version }} id image"#,
];

/// The `outputs` of [`DIGEST_JOB`], exactly, sorted by name: the three lines of `release
/// image-digest`, from its step `image`.
pub const DIGEST_OUTPUTS: [(&str, &str); 3] = [
    ("bundle-url", "${{ steps.image.outputs.bundle-url }}"),
    ("digest", "${{ steps.image.outputs.digest }}"),
    ("image-url", "${{ steps.image.outputs.image-url }}"),
];

/// The inputs [`JOB`] passes to [`DEPLOY_WORKFLOW`], exactly, sorted by name.
pub const WITH: [(&str, &str); 12] = [
    ("audience", "${{ vars.CP_DEPLOY_AUDIENCE }}"),
    (
        "bundle-url",
        "${{ needs.cp-deploy-digest.outputs.bundle-url }}",
    ),
    ("endpoint", "${{ vars.CP_DEPLOY_ENDPOINT }}"),
    ("environment", "controlplane"),
    (
        "image-digest",
        "${{ needs.cp-deploy-digest.outputs.digest }}",
    ),
    (
        "image-url",
        "${{ needs.cp-deploy-digest.outputs.image-url }}",
    ),
    ("service", "hostkeeper-coordinator"),
    (
        "tailscale-audience",
        "${{ vars.CP_DEPLOY_TAILSCALE_AUDIENCE }}",
    ),
    (
        "tailscale-oauth-client-id",
        "${{ vars.CP_DEPLOY_TAILSCALE_CLIENT_ID }}",
    ),
    ("tailscale-tags", "${{ vars.CP_DEPLOY_TAILSCALE_TAGS }}"),
    ("version", "${{ needs.newest.outputs.version }}"),
    ("wait-minutes", "60"),
];

/// Whether `job`, the job `name` of `file`, is [`JOB`] of [`FILE`] calling [`DEPLOY_WORKFLOW`]:
/// the one job that calls a reusable workflow of another repository but the CI client's callers,
/// so the shared rules of [`super::lint`] read no `runs-on` of it (the called workflow's own
/// job runs on GitHub's hosted `ubuntu-24.04`).
pub fn calls_updater(file: &str, name: &str, job: &Node) -> bool {
    file == FILE && name == JOB && text(Some(job), "uses") == Some(DEPLOY_WORKFLOW)
}

/// A step as [`DIGEST_STEPS`] writes it: [`super::desired::step_shape`], then
/// ` working-directory <dir>` and ` id <id>` when the step has them (the id is what
/// [`DIGEST_OUTPUTS`] read from).
pub fn step_shape(step: &Node) -> String {
    let mut shape = desired_step_shape(step);
    for key in ["working-directory", "id"] {
        if let Some(value) = text(Some(step), key) {
            shape.push_str(&format!(" {key} {value}"));
        }
    }
    shape
}

/// The keys of `job`, sorted.
fn keys(job: &Node) -> Vec<&str> {
    let mut found: Vec<&str> = entries(job)
        .iter()
        .map(|(k, _)| k.as_str().unwrap_or_default())
        .collect();
    found.sort_unstable();
    found
}

/// One line per rule of this module that the workflow `doc` of `release.yml` breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, allowed) in [(DIGEST_JOB, &DIGEST_KEYS[..]), (JOB, &JOB_KEYS[..])] {
        let Some(j) = super::desired::job(doc, name) else {
            problems.push(format!("{FILE}: job {name} is missing"));
            continue;
        };
        if text(Some(j), "if") != Some(DEPLOY_IF) {
            problems.push(format!(
                "{FILE}: job {name} needs the exact `if: {DEPLOY_IF}`"
            ));
        }
        for key in keys(j).into_iter().filter(|k| !allowed.contains(k)) {
            problems.push(format!(
                "{FILE}: job {name} has `{key}`, which it may not (only {allowed:?})"
            ));
        }
    }
    problems.extend(digest_problems(super::desired::job(doc, DIGEST_JOB)));
    problems.extend(caller_problems(super::desired::job(doc, JOB)));
    problems
}

fn digest_problems(digest: Option<&Node>) -> Vec<String> {
    let Some(digest) = digest else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let got = needs(Some(digest));
    if got != ["newest", "plan"] {
        problems.push(format!(
            "{FILE}: job {DIGEST_JOB} needs {got:?}, not [\"newest\", \"plan\"]"
        ));
    }
    if text(Some(digest), "runs-on") != Some(RUNNER) {
        problems.push(format!("{FILE}: job {DIGEST_JOB} must run on `{RUNNER}`"));
    }
    if text(Some(digest), "timeout-minutes") != Some(TIMEOUT_MINUTES) {
        problems.push(format!(
            "{FILE}: job {DIGEST_JOB} must have `timeout-minutes: {TIMEOUT_MINUTES}`"
        ));
    }
    if pairs(digest, "permissions") != [("contents", "read")] {
        problems.push(format!(
            "{FILE}: job {DIGEST_JOB} must hold exactly `contents: read`"
        ));
    }
    let steps = match entry(digest, "steps").map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    for (at, step) in steps.iter().enumerate() {
        for (key, _) in entries(step) {
            let key = key.as_str().unwrap_or_default();
            if !STEP_KEYS.contains(&key) {
                problems.push(format!(
                    "{FILE}: job {DIGEST_JOB} step {} has `{key}`, which a step of it may not",
                    at + 1
                ));
            }
        }
    }
    let shapes: Vec<String> = steps.iter().map(step_shape).collect();
    if shapes.len() != DIGEST_STEPS.len() {
        problems.push(format!(
            "{FILE}: job {DIGEST_JOB} has {} steps, not the {} of cp_deploy::DIGEST_STEPS",
            shapes.len(),
            DIGEST_STEPS.len()
        ));
    }
    for (at, (got, want)) in shapes.iter().zip(DIGEST_STEPS).enumerate() {
        if got != want {
            problems.push(format!(
                "{FILE}: job {DIGEST_JOB} step {} is `{got}`, not `{want}`",
                at + 1
            ));
        }
    }
    let outputs = pairs(digest, "outputs");
    if outputs != DIGEST_OUTPUTS {
        problems.push(format!(
            "{FILE}: job {DIGEST_JOB} outputs are {outputs:?}, not {DIGEST_OUTPUTS:?}"
        ));
    }
    problems
}

fn caller_problems(caller: Option<&Node>) -> Vec<String> {
    let Some(caller) = caller else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let got = needs(Some(caller));
    if got != [DIGEST_JOB, "newest", "plan"] {
        problems.push(format!(
            "{FILE}: job {JOB} needs {got:?}, not [\"{DIGEST_JOB}\", \"newest\", \"plan\"]"
        ));
    }
    if pairs(caller, "permissions") != [("id-token", "write")] {
        problems.push(format!(
            "{FILE}: job {JOB} must hold exactly `id-token: write`"
        ));
    }
    let uses = text(Some(caller), "uses");
    if uses != Some(DEPLOY_WORKFLOW) {
        problems.push(format!(
            "{FILE}: job {JOB} must call `{DEPLOY_WORKFLOW}`, not `{}`",
            uses.unwrap_or_default()
        ));
    }
    let with = pairs(caller, "with");
    if with != WITH {
        problems.push(format!("{FILE}: job {JOB} passes {with:?}, not {WITH:?}"));
    }
    problems
}
