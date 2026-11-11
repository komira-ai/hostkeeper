//! The job `coordinator` of `release.yml` (docs/deploy/github-actions.md, "Wait for the
//! coordinator"): the release waits until the coordinator runs at least that release, before
//! `set-desired` sends it fields an older coordinator refuses. The control-plane deploy
//! (`cp-deploy`, [`super::cp_deploy`]) or, without it, the coordinator's own machine
//! (docs/deploy/coordinator-update.md) updates it; this job only asks, once that deploy ended,
//! with curl's own retries, `GET /v1/version?at_least=<version>` on the private listener over
//! the tailnet. It takes no token. [`problems`] refuses the job when:
//! - it needs other than exactly [`NEEDS`] (so the wait never starts while the deploy that
//!   brings the coordinator to the release still runs), runs on another runner than
//!   [`RUNNER`], has another timeout than [`TIMEOUT_MINUTES`], or has a key outside [`JOB_KEYS`]
//!   (an `env`, `outputs`, `container`, `services` or `uses` that could run or carry something
//!   else); its `if:` is held to [`super::desired::COORDINATOR_IF`] by
//!   [`super::desired::problems`], and its one write scope to [`super::lint::WRITE_ALLOWED`];
//! - its token rights are other than exactly [`PERMISSIONS`]: `id-token: write` for the tailnet
//!   join, and no repository right, since no step reads the repository;
//! - a step has a key outside [`STEP_KEYS`], or its steps are not exactly [`STEPS`], in that
//!   order, each as [`super::desired::step_shape`] writes it: so the tailnet tag is checked
//!   first, the one action is the pinned tailscale join with the same `with` as the CI client's,
//!   the CA is written from the repository variable, and the one request is the exact command
//!   line of [`STEPS`] (a pinned CA, TLS 1.2 or later, HTTPS only, no header, hence no token;
//!   `--fail`, which writes no body on an HTTP error, so curl can retry a 409 into /dev/null),
//!   which writes the last answer's HTTP status to the step's output `status`, so no checkout,
//!   download, `cargo` or other action can be added; and the two error steps, of which exactly
//!   one runs when the wait failed ([`NO_UPDATER_IF`] after a 409 or a 404, [`NO_ANSWER_IF`]
//!   otherwise), say the exact texts of [`STEPS`];
//! - the steps' ids are not exactly [`STEP_IDS`] (the error steps read `steps.wait`);
//! - a step's `run:` holds an expression (`${{`): a value reaches a command through `env:`.

use super::desired::{pairs, step_shape, text};
use super::lint::{entries, entry};
use super::yaml::{Node, Value};

use super::desired::{FILE, needs};

/// The job's name.
pub const JOB: &str = super::desired::COORDINATOR_JOB;

/// The jobs the job needs, sorted: the control-plane deploy (so the wait starts only once that
/// deploy ended or was skipped), the plan, and the targets job whose version it waits for.
pub const NEEDS: [&str; 3] = ["cp-deploy", "plan", "targets"];

/// The runner of the job.
pub const RUNNER: &str = "ubuntu-24.04";

/// The job's timeout: the wait is 30 minutes, the join and the rest a few more.
pub const TIMEOUT_MINUTES: &str = "40";

/// The keys the job may have; its `if:` is held by [`super::desired::problems`].
pub const JOB_KEYS: [&str; 6] = [
    "if",
    "needs",
    "permissions",
    "runs-on",
    "steps",
    "timeout-minutes",
];

/// The token rights of the job, exactly: the OIDC token of the tailnet join, and nothing else.
pub const PERMISSIONS: [(&str, &str); 1] = [("id-token", "write")];

/// The keys a step may have: an `if:` is allowed, and part of the shape of the two steps that
/// have one; no `continue-on-error`, `shell` or `working-directory`.
pub const STEP_KEYS: [&str; 7] = ["env", "id", "if", "name", "run", "uses", "with"];

/// The ids of the steps, in order, `""` for the step with none.
pub const STEP_IDS: [&str; 6] = ["tailnet-tag", "tailnet", "ca", "wait", "", ""];

/// The condition of the error step that says no coordinator updater reported the release: the
/// wait failed, and the coordinator's last answer was that it runs an older release (409) or is
/// from before the version route (404).
pub const NO_UPDATER_IF: &str = "${{ failure() && steps.wait.outcome == 'failure' && (steps.wait.outputs.status == '409' || steps.wait.outputs.status == '404') }}";

/// The condition of the error step that says the coordinator did not answer: the wait failed,
/// with any other last status (000 for no answer, or none at all).
pub const NO_ANSWER_IF: &str = "${{ failure() && steps.wait.outcome == 'failure' && steps.wait.outputs.status != '409' && steps.wait.outputs.status != '404' }}";

/// The steps, in order, as [`super::desired::step_shape`] writes them.
pub const STEPS: [&str; 6] = [
    r#"run jq --null-input --arg tag "$TAILNET_TAG" 'if $tag | test("^tag:[a-z][a-z0-9-]*\\z") then empty else error("tailnet_tag is not tag: followed by a Tailscale tag name (a lower-case letter, then lower-case letters, digits and -)") end' env TAILNET_TAG=${{ vars.HOSTKEEPER_TAILNET_TAG }}"#,
    "if ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} uses tailscale/github-action with audience=${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }} with oauth-client-id=${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }} with ping=${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }} with sha256sum=c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32 with tags=${{ vars.HOSTKEEPER_TAILNET_TAG }} with use-cache=false with version=1.94.2",
    r#"run printf '%s\n' "$COORDINATOR_CA" > "$RUNNER_TEMP/coordinator-ca.pem" env COORDINATOR_CA=${{ vars.HOSTKEEPER_COORDINATOR_CA }}"#,
    r#"run curl --proto =https --tlsv1.2 --cacert "$RUNNER_TEMP/coordinator-ca.pem" --fail --silent --show-error --max-time 20 --retry 180 --retry-delay 10 --retry-max-time 1800 --retry-all-errors --output /dev/null --write-out 'status=%{http_code}\n' --url "$COORDINATOR_URL/v1/version?at_least=$VERSION" >> "$GITHUB_OUTPUT" env COORDINATOR_URL=${{ vars.HOSTKEEPER_COORDINATOR_URL }} env VERSION=${{ needs.targets.outputs.version }}"#,
    r#"if ${{ failure() && steps.wait.outcome == 'failure' && (steps.wait.outputs.status == '409' || steps.wait.outputs.status == '404') }} run echo "::error::No coordinator updater reported this release ($VERSION) (is the timer installed?). For 30 minutes the coordinator answered that it runs an older release (its last answer was HTTP $STATUS; 404 is a coordinator from before the version route). Install the timer of hostkeeper-coordinator-update on the coordinator's machine, or read its journal there, as docs/deploy/coordinator-update.md says. With the control-plane deploy on (CP_DEPLOY_ENDPOINT), the job cp-deploy reported this release deployed, so check that HOSTKEEPER_COORDINATOR_URL names the coordinator it deploys. The hosts' desired version is not set." env STATUS=${{ steps.wait.outputs.status }} env VERSION=${{ needs.targets.outputs.version }}"#,
    r#"if ${{ failure() && steps.wait.outcome == 'failure' && steps.wait.outputs.status != '409' && steps.wait.outputs.status != '404' }} run echo "::error::The coordinator did not say whether it runs this release ($VERSION) within 30 minutes; its last answer was HTTP $STATUS (000 is no answer). Check the tailnet join and the repository variables HOSTKEEPER_COORDINATOR_URL and HOSTKEEPER_COORDINATOR_CA. The hosts' desired version is not set." env STATUS=${{ steps.wait.outputs.status }} env VERSION=${{ needs.targets.outputs.version }}"#,
];

/// One line per rule of this module that the job `coordinator` of `release.yml` breaks.
pub fn problems(coordinator: Option<&Node>) -> Vec<String> {
    let who = format!("{FILE}: job {JOB}");
    let mut problems = Vec::new();
    let Some(job) = coordinator else {
        return vec![format!("{who} is missing")];
    };
    let got = needs(Some(job));
    if got != NEEDS {
        problems.push(format!("{who} needs {got:?}, not {NEEDS:?}"));
    }
    if text(Some(job), "runs-on") != Some(RUNNER) {
        problems.push(format!("{who} must run on `{RUNNER}`"));
    }
    if text(Some(job), "timeout-minutes") != Some(TIMEOUT_MINUTES) {
        problems.push(format!(
            "{who} must have `timeout-minutes: {TIMEOUT_MINUTES}`"
        ));
    }
    for (key, _) in entries(job) {
        let key = key.as_str().unwrap_or_default();
        if !JOB_KEYS.contains(&key) && key != "continue-on-error" {
            problems.push(format!("{who} has `{key}`, which it may not"));
        }
    }
    if pairs(job, "permissions") != PERMISSIONS.to_vec() {
        problems.push(format!("{who} must hold exactly `id-token: write`"));
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
    if steps.len() != STEPS.len() {
        problems.push(format!(
            "{who} has {} steps, not the {} of coordinator_wait::STEPS",
            steps.len(),
            STEPS.len()
        ));
    }
    for (at, (step, want)) in steps.iter().zip(STEPS).enumerate() {
        let got = step_shape(step);
        if got != want {
            problems.push(format!("{who} step {} is `{got}`, not `{want}`", at + 1));
        }
    }
    let ids: Vec<&str> = steps
        .iter()
        .map(|s| text(Some(s), "id").unwrap_or_default())
        .collect();
    if ids != STEP_IDS {
        problems.push(format!("{who} step ids are {ids:?}, not {STEP_IDS:?}"));
    }
    problems
}
