//! The deploy of the coordinator to its Docker Swarm service in `release.yml`
//! (docs/deploy/coordinator-update.md, "On Docker Swarm"): the job [`DIGEST_JOB`] reads the
//! digest of the coordinator's linux/amd64 image from the release's image index, and the job
//! [`JOB`] calls the reusable workflow [`WORKFLOW`] of `komira-ai/docker-swarm-updater`, which asks
//! the updater of the Swarm service, with an OIDC token, to deploy that release. The deploy is
//! off until the repository variable [`GATE`] is not empty. [`problems`] refuses `release.yml`
//! when:
//! - either job lacks the exact `if:` [`DEPLOY_IF`] (a release of main that [`super::desired`]'s
//!   `targets` found the newest published, only while [`GATE`] is not empty, so that the deploy
//!   can be turned off and an older run never replaces a newer release), or has
//!   `continue-on-error`;
//! - [`JOB`] does not need exactly `plan`, `targets` and [`DIGEST_JOB`], holds other token rights
//!   than exactly [`PERMISSIONS`] (`id-token: write` and nothing else: no repository right, no
//!   other scope), has a key outside [`JOB_KEYS`] (a `runs-on`, `steps`, `secrets`, `env` or
//!   `container` that could run or carry something else), or has a `with` other than exactly
//!   [`WITH`]: the service, the version of the release and the digest from [`DIGEST_JOB`], the
//!   URLs of the release's linux-x86_64 image tarball and its bundle, the endpoint, the
//!   audience, the tailnet join and the environment `controlplane` from the repository
//!   variables ([`GATE`] and its three siblings), and no input the lint does not name (a
//!   signer other than this repository's `artifacts.yml` on main would be one);
//! - [`JOB`] calls another reusable workflow than [`WORKFLOW`], or at another revision than
//!   [`COMMIT`], or without the version comment `v`[`RELEASE`] on its line (the shared rule of
//!   [`super::lint`] holds every `uses:` to a full commit and a version comment; this one holds
//!   this call to the one revision that was read). Moving it is one reviewed change: these two
//!   values and the `uses:` line;
//! - [`DIGEST_JOB`] needs other than exactly `plan` and `targets`, holds other token rights than
//!   exactly `contents: read`, has a key outside [`DIGEST_JOB_KEYS`], has a step with a key
//!   outside [`STEP_KEYS`], steps other than exactly [`DIGEST_STEPS`], in that order (the run's
//!   own `release-v<version>` artifact, checked against SHA256SUMS, then the one jq command),
//!   steps whose ids are not [`DIGEST_IDS`], a `run:` that holds an expression (`${{`: a value
//!   reaches a command through `env:`), or `outputs` other than exactly [`DIGEST_OUTPUTS`].
//!
//! `id-token: write` is held by [`JOB`] alone, under [`DEPLOY_IF`] ([`super::lint`] refuses it in
//! every other job of `release.yml` but those of its tables), and [`JOB`] is the one job of any
//! workflow that calls a reusable workflow other than the CI client ([`is_deploy_job`]).

use super::desired::{FILE, needs, pairs, step_shape, text};
use super::lint::{entries, entry, version_comment};
use super::yaml::{Node, Value};

/// The job that reads the digest of the coordinator's image.
pub const DIGEST_JOB: &str = "deploy-digest";

/// The job that calls the deploy workflow.
pub const JOB: &str = "deploy";

/// The repository variable that turns the deploy on: not empty runs both jobs, empty or unset
/// skips them. It holds the address of the deploy front, which is not a secret.
pub const GATE: &str = "CP_DEPLOY_ENDPOINT";

/// The reusable workflow [`JOB`] calls, as its `uses:` names it before the `@`.
pub const WORKFLOW: &str = "komira-ai/docker-swarm-updater/.github/workflows/deploy.yml";

/// The revision of [`WORKFLOW`] that [`JOB`] calls: a full commit, the one the release
/// [`RELEASE`] was tagged at.
pub const COMMIT: &str = "c1fcc47610e2886d4d9e561be3c0c76f0452d1f6";

/// The release of `komira-ai/docker-swarm-updater` at [`COMMIT`], X.Y.Z, which the version
/// comment on the `uses:` line names.
pub const RELEASE: &str = "0.1.3";

/// The `if:` of both jobs: a release was planned on main, `targets` found it the newest published
/// (`targets` runs only while `RELEASE_SETS_DESIRED` is `true`), and [`GATE`] is not empty.
pub const DEPLOY_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && needs.targets.outputs.newest == 'true' && vars.CP_DEPLOY_ENDPOINT != '' }}";

/// The token rights of [`JOB`], exactly: the OIDC token the called workflow asks GitHub for.
pub const PERMISSIONS: [(&str, &str); 1] = [("id-token", "write")];

/// The keys [`JOB`] may have: it is a caller, so no `runs-on`, `steps`, `secrets` or `env`.
pub const JOB_KEYS: [&str; 5] = ["if", "needs", "permissions", "uses", "with"];

/// The keys [`DIGEST_JOB`] may have.
pub const DIGEST_JOB_KEYS: [&str; 7] = [
    "if",
    "needs",
    "outputs",
    "permissions",
    "runs-on",
    "steps",
    "timeout-minutes",
];

/// The keys a step of [`DIGEST_JOB`] may have: no `if:` that could skip it, no
/// `continue-on-error` that could let it fail, no `shell`.
pub const STEP_KEYS: [&str; 7] = [
    "env",
    "id",
    "name",
    "run",
    "uses",
    "with",
    "working-directory",
];

/// The inputs [`JOB`] passes to [`WORKFLOW`], exactly, sorted by name. The signer inputs are not
/// passed: the called workflow takes this repository's `artifacts.yml` on `refs/heads/main`,
/// which is what signed the release.
pub const WITH: [(&str, &str); 11] = [
    ("audience", "${{ vars.CP_DEPLOY_AUDIENCE }}"),
    (
        "bundle-url",
        "https://github.com/komira-ai/hostkeeper/releases/download/v${{ needs.targets.outputs.version }}/hostkeeper-coordinator-${{ needs.targets.outputs.version }}-linux-x86_64.image.tar.sigstore.json",
    ),
    ("endpoint", "${{ vars.CP_DEPLOY_ENDPOINT }}"),
    ("environment", "controlplane"),
    ("image-digest", "${{ needs.deploy-digest.outputs.digest }}"),
    (
        "image-url",
        "https://github.com/komira-ai/hostkeeper/releases/download/v${{ needs.targets.outputs.version }}/hostkeeper-coordinator-${{ needs.targets.outputs.version }}-linux-x86_64.image.tar",
    ),
    ("service", "hostkeeper-coordinator"),
    ("tailscale-audience", "${{ vars.CP_DEPLOY_TAILSCALE_AUDIENCE }}"),
    ("tailscale-oauth-client-id", "${{ vars.CP_DEPLOY_TAILSCALE_CLIENT_ID }}"),
    ("tailscale-tags", "tag:cp-deploy"),
    ("version", "${{ needs.targets.outputs.version }}"),
];

/// The steps of [`DIGEST_JOB`], in order, as [`step_shape`] writes them, each followed by ` in
/// <directory>` when it has a `working-directory`.
pub const DIGEST_STEPS: [&str; 3] = [
    "uses actions/download-artifact with name=release-v${{ needs.targets.outputs.version }} with path=dist",
    "run sha256sum --check --strict SHA256SUMS in dist",
    r##"run jq --raw-output '[.manifests[] | select(.platform.os == "linux" and .platform.architecture == "amd64") | .digest] | if length == 1 and (.[0] | test("^sha256:[0-9a-f]{64}$")) then "digest=\(.[0])" else error("the image index does not list exactly one linux/amd64 manifest by a sha256 digest") end' "hostkeeper-coordinator-$VERSION.image.json" >> "$GITHUB_OUTPUT" env VERSION=${{ needs.targets.outputs.version }} in dist"##,
];

/// The ids of the steps of [`DIGEST_JOB`], in order, `""` for the step with none.
pub const DIGEST_IDS: [&str; 3] = ["", "", "digest"];

/// The `outputs` of [`DIGEST_JOB`], exactly.
pub const DIGEST_OUTPUTS: [(&str, &str); 1] = [("digest", "${{ steps.digest.outputs.digest }}")];

/// Whether the job `name` of the workflow `file` is [`JOB`]: the one job that may call a
/// reusable workflow other than the CI client and hold `id-token: write` for it. What it calls
/// and how is held by [`problems`].
pub fn is_deploy_job(file: &str, name: &str) -> bool {
    file == FILE && name == JOB
}

/// The step as [`DIGEST_STEPS`] writes it.
fn shape(step: &Node) -> String {
    let mut shape = step_shape(step);
    if let Some(dir) = text(Some(step), "working-directory") {
        shape.push_str(&format!(" in {dir}"));
    }
    shape
}

/// One line per rule of this module that the workflow `doc` of `release.yml`, whose text is
/// `source`, breaks; none for any other file.
pub fn problems(file: &str, source: &str, doc: &Node) -> Vec<String> {
    if file != FILE {
        return Vec::new();
    }
    let mut problems = Vec::new();
    for name in [DIGEST_JOB, JOB] {
        let job = super::desired::job(doc, name);
        if text(job, "if") != Some(DEPLOY_IF) {
            problems.push(format!(
                "{FILE}: job {name} needs the exact `if: {DEPLOY_IF}`"
            ));
        }
        if job.and_then(|j| entry(j, "continue-on-error")).is_some() {
            problems.push(format!(
                "{FILE}: job {name} has `continue-on-error`: a failure must stop the deploy"
            ));
        }
    }
    problems.extend(digest_problems(super::desired::job(doc, DIGEST_JOB)));
    problems.extend(call_problems(source, super::desired::job(doc, JOB)));
    problems
}

/// What is wrong with the job [`JOB`], which calls the deploy workflow.
fn call_problems(source: &str, job: Option<&Node>) -> Vec<String> {
    let who = format!("{FILE}: job {JOB}");
    let Some(job) = job else {
        return vec![format!("{who} is missing")];
    };
    let mut problems = Vec::new();
    let got = needs(Some(job));
    if got != ["deploy-digest", "plan", "targets"] {
        problems.push(format!(
            "{who} needs {got:?}, not [\"deploy-digest\", \"plan\", \"targets\"]"
        ));
    }
    if pairs(job, "permissions") != PERMISSIONS.to_vec() {
        problems.push(format!("{who} must hold exactly `id-token: write`"));
    }
    for (key, _) in entries(job) {
        let key = key.as_str().unwrap_or_default();
        if !JOB_KEYS.contains(&key) && key != "continue-on-error" {
            problems.push(format!("{who} has `{key}`, which it may not"));
        }
    }
    match entry(job, "uses") {
        None => problems.push(format!("{who} does not call {WORKFLOW}")),
        Some(node) => {
            let uses = node.as_str().unwrap_or_default();
            let (path, pin) = uses.split_once('@').unwrap_or((uses, ""));
            if path != WORKFLOW {
                problems.push(format!("{who} calls {path}, not {WORKFLOW}"));
            }
            if pin != COMMIT {
                problems.push(format!(
                    "{who} must pin {WORKFLOW} at the revision {COMMIT}, not `{uses}`"
                ));
            }
            let want = format!("v{RELEASE}");
            let comment = version_comment(source, node.line(), uses);
            if comment.is_none_or(|c| c.split_whitespace().next() != Some(want.as_str())) {
                problems.push(format!(
                    "{who}: the version comment on the `uses:` line must start with `v{RELEASE}`"
                ));
            }
        }
    }
    let with = pairs(job, "with");
    if with != WITH {
        problems.push(format!("{who} passes {with:?}, not {WITH:?}"));
    }
    problems
}

/// What is wrong with the job [`DIGEST_JOB`].
fn digest_problems(job: Option<&Node>) -> Vec<String> {
    let who = format!("{FILE}: job {DIGEST_JOB}");
    let Some(job) = job else {
        return vec![format!("{who} is missing")];
    };
    let mut problems = Vec::new();
    let got = needs(Some(job));
    if got != ["plan", "targets"] {
        problems.push(format!("{who} needs {got:?}, not [\"plan\", \"targets\"]"));
    }
    if pairs(job, "permissions") != [("contents", "read")] {
        problems.push(format!("{who} must hold exactly `contents: read`"));
    }
    for (key, _) in entries(job) {
        let key = key.as_str().unwrap_or_default();
        if !DIGEST_JOB_KEYS.contains(&key) && key != "continue-on-error" {
            problems.push(format!("{who} has `{key}`, which it may not"));
        }
    }
    let steps = match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    for (at, step) in steps.iter().enumerate() {
        for (key, _) in entries(step) {
            let key = key.as_str().unwrap_or_default();
            if !STEP_KEYS.contains(&key) {
                problems.push(format!(
                    "{who} step {} has `{key}`, which a step of it may not",
                    at + 1
                ));
            }
        }
        if text(Some(step), "run").is_some_and(|run| run.contains("${{")) {
            problems.push(format!(
                "{who} step {} writes an expression into its `run:`; pass the value through `env:`",
                at + 1
            ));
        }
    }
    if steps.len() != DIGEST_STEPS.len() {
        problems.push(format!(
            "{who} has {} steps, not the {} of deploy::DIGEST_STEPS",
            steps.len(),
            DIGEST_STEPS.len()
        ));
    }
    for (at, (step, want)) in steps.iter().zip(DIGEST_STEPS).enumerate() {
        let got = shape(step);
        if got != want {
            problems.push(format!("{who} step {} is `{got}`, not `{want}`", at + 1));
        }
    }
    let ids: Vec<&str> = steps
        .iter()
        .map(|s| text(Some(s), "id").unwrap_or_default())
        .collect();
    if ids != DIGEST_IDS {
        problems.push(format!("{who} step ids are {ids:?}, not {DIGEST_IDS:?}"));
    }
    let outputs = pairs(job, "outputs");
    if outputs != DIGEST_OUTPUTS {
        problems.push(format!("{who} outputs are {outputs:?}, not {DIGEST_OUTPUTS:?}"));
    }
    problems
}
