//! The rules of hostkeeper's own deploy in `release.yml` (docs/deploy/github-actions.md): the
//! job [`TARGETS_JOB`], which writes the targets of the newest release of the run, and the job
//! [`JOB`], which sets it as hostkeeper's desired version through the CI client. A release is set
//! with native entries alone (docs/design/packages-native.md section 10.5, A24): no host fetches
//! it from the release channel, so nothing waits for the channel, and no index is given.
//! [`problems`] refuses `release.yml` when:
//! - [`TARGETS_JOB`] lacks the exact `if:` [`DESIRED_IF`] (a release of main, only while the
//!   repository variable [`GATE`] is `true`, so that the deploy can be turned off), or [`JOB`]
//!   the exact `if:` [`SET_IF`] (the same, and only when this run's release is the newest
//!   published); or either job has `continue-on-error`;
//! - [`TARGETS_JOB`] does not need exactly `plan` and `publish` (it runs once every release of
//!   the run is published, and not before), holds other token rights than exactly
//!   [`TARGETS_PERMISSIONS`] (reading the repository: it joins no tailnet), has a step with a key
//!   outside [`STEP_KEYS`] (an `if:` that could skip it, a `continue-on-error`), or its steps are
//!   not exactly [`TARGETS_STEPS`], in that order, with their `with` and `env`: the newest
//!   version of the run and its commit, xtask compiled from a checkout of the release's commit,
//!   the release's files from the run's own `release-<tag>` artifact, the release's targets
//!   written by `cargo xtask release targets` from those files, and whether no newer release is
//!   published; so no step waits for the release channel, compares an index or writes one out;
//!   or its `outputs` are not exactly [`TARGETS_OUTPUTS`]; or a step's `run:` holds an
//!   expression (`${{`): a value reaches a command through the step's `env:` and, for a command
//!   of ours, a flag, never as text written into the script, which a value holding shell syntax
//!   would run;
//! - [`COORDINATOR_JOB`] lacks the exact `if:` [`SET_IF`] or has `continue-on-error`, or breaks
//!   the rules of [`super::coordinator_wait`] (it waits until the coordinator runs the release,
//!   and holds `id-token: write` for the tailnet join alone);
//! - [`JOB`] does not need exactly `plan`, [`TARGETS_JOB`] and [`COORDINATOR_JOB`] (it calls the
//!   coordinator only with the release's targets, and only once the coordinator runs the
//!   release), or its `with` does not set `service: hostkeeper`, the version
//!   and the targets from [`TARGETS_JOB`] ([`WITH_FROM_TARGETS`]), and `wait: false`; or passes
//!   one of [`NOT_PASSED`]: a `package_digest` (one package for every host, whatever its
//!   platform), a `release_index` or a `release_index_digest` (a release is set with no index).
//!
//! That [`JOB`] calls the CI client of this repository at the CI client's revision
//! ([`super::callers::CLIENT_COMMIT`], with its release's `hostkeeper_version` and
//! `hostkeeper_commit`), with exactly `contents: read` and `id-token: write` and no secrets, is
//! the rule of every caller ([`super::callers`]).

use super::lint::{entries, entry};
use super::yaml::{Node, Value};

/// The workflow the two jobs are in.
pub const FILE: &str = "release.yml";

/// The job that writes the targets of the release.
pub const TARGETS_JOB: &str = "targets";

/// The job that waits until the coordinator runs the release ([`super::coordinator_wait`]).
pub const COORDINATOR_JOB: &str = "coordinator";

/// The job that calls the CI client.
pub const JOB: &str = "set-desired";

/// The repository variable that turns the deploy on: `true` runs both jobs; unset, or any other
/// value, skips them.
pub const GATE: &str = "RELEASE_SETS_DESIRED";

/// The `if:` of [`TARGETS_JOB`]: a release of main was planned, and [`GATE`] is `true`.
pub const DESIRED_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' }}";

/// The `if:` of [`JOB`]: [`DESIRED_IF`], and [`TARGETS_JOB`] found this run's release the newest
/// published, so that a late run of an older commit never moves the fleet back.
pub const SET_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' && needs.targets.outputs.newest == 'true' }}";

/// The keys a step of [`TARGETS_JOB`] may have: no `if:` that could skip it, no
/// `continue-on-error` that could let it fail, no `shell` or `working-directory` that could
/// change what its command does.
pub const STEP_KEYS: [&str; 6] = ["env", "id", "name", "run", "uses", "with"];

/// The token rights of [`TARGETS_JOB`], exactly: it reads the repository, and asks GitHub for no
/// OIDC token, since it joins no tailnet.
pub const TARGETS_PERMISSIONS: [(&str, &str); 1] = [("contents", "read")];

/// The value every caller of the CI client passes as `join_tailnet`: the repository variable of
/// the federated identity's client ID is set. Empty, the CI client's runner does not join.
pub const JOIN_IF: &str = "${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}";

/// The steps of [`TARGETS_JOB`], in order, as [`step_shape`] writes them: `if <condition> ` first
/// when the step has an `if:` (which none may), then a `run:` step as its command, an action as
/// `uses <action>`, each followed by its `with` and its `env`, every entry as ` with <key>=<value>`
/// or ` env <key>=<value>`, sorted by key.
pub const TARGETS_STEPS: [&str; 7] = [
    r#"run jq --raw-output '.include[-1] | "version=\(.version)", "tag=\(.tag)", "commit=\(.commit)"' <<< "$MATRIX" >> "$GITHUB_OUTPUT" env MATRIX=${{ needs.plan.outputs.matrix }}"#,
    "uses actions/checkout with persist-credentials=false with ref=${{ steps.newest.outputs.commit }}",
    "run rustup toolchain install",
    "run cargo xtask --help",
    "uses actions/download-artifact with name=release-${{ steps.newest.outputs.tag }} with path=dist",
    r#"run cargo xtask release targets --version "$VERSION" --dir dist --repo "$GITHUB_REPOSITORY" --output "$GITHUB_OUTPUT" env VERSION=${{ steps.newest.outputs.version }}"#,
    r#"run gh release list --repo "$GITHUB_REPOSITORY" --exclude-drafts --limit 100 --json tagName --jq '[.[].tagName | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) | ltrimstr("v") | split(".") | map(tonumber)] | max | "newest=\(map(tostring) | join(".") == env.VERSION)"' >> "$GITHUB_OUTPUT" env GH_TOKEN=${{ github.token }} env VERSION=${{ steps.newest.outputs.version }}"#,
];

/// The `outputs` of [`TARGETS_JOB`], exactly, sorted by name.
pub const TARGETS_OUTPUTS: [(&str, &str); 3] = [
    ("newest", "${{ steps.latest.outputs.newest }}"),
    ("targets", "${{ steps.targets.outputs.targets }}"),
    ("version", "${{ steps.newest.outputs.version }}"),
];

/// The inputs [`JOB`] passes from the outputs of [`TARGETS_JOB`], each from the output of its
/// own name.
pub const WITH_FROM_TARGETS: [&str; 2] = ["version", "targets"];

/// The inputs of the CI client [`JOB`] may not pass: one package for every host, or a release
/// index, of which a set of native entries alone has none.
pub const NOT_PASSED: [&str; 3] = ["package_digest", "release_index", "release_index_digest"];

pub fn job<'a>(doc: &'a Node, name: &str) -> Option<&'a Node> {
    entry(doc, "jobs").and_then(|jobs| entry(jobs, name))
}

pub fn text<'a>(map: Option<&'a Node>, key: &str) -> Option<&'a str> {
    map.and_then(|m| entry(m, key)).and_then(Node::as_str)
}

/// The names `needs` lists (one name or a list), sorted.
pub fn needs(job: Option<&Node>) -> Vec<&str> {
    let mut found: Vec<&str> = match job.and_then(|j| entry(j, "needs")) {
        Some(Node {
            value: Value::Seq(items),
            ..
        }) => items.iter().filter_map(Node::as_str).collect(),
        Some(one) => one.as_str().into_iter().collect(),
        None => Vec::new(),
    };
    found.sort_unstable();
    found
}

/// The `(key, value)` pairs of the mapping `key` of `node`, sorted by key.
pub fn pairs<'a>(node: &'a Node, key: &str) -> Vec<(&'a str, &'a str)> {
    let mut found: Vec<(&str, &str)> = entry(node, key)
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default(),
                v.as_str().unwrap_or_default(),
            )
        })
        .collect();
    found.sort_unstable();
    found
}

/// The step as [`TARGETS_STEPS`] writes it.
pub fn step_shape(step: &Node) -> String {
    let mut shape = text(Some(step), "if").map_or_else(String::new, |c| format!("if {c} "));
    shape.push_str(&match text(Some(step), "run") {
        Some(run) => format!("run {}", run.trim_end()),
        None => {
            let action = text(Some(step), "uses")
                .map(|u| u.split_once('@').map_or(u, |(path, _)| path))
                .unwrap_or_default();
            format!("uses {action}")
        }
    });
    for key in ["with", "env"] {
        for (k, v) in pairs(step, key) {
            shape.push_str(&format!(" {key} {k}={v}"));
        }
    }
    shape
}

/// One line per rule of this module that the workflow `doc` of `release.yml` breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, condition) in [
        (TARGETS_JOB, DESIRED_IF),
        (COORDINATOR_JOB, SET_IF),
        (JOB, SET_IF),
    ] {
        let j = job(doc, name);
        if text(j, "if") != Some(condition) {
            problems.push(format!(
                "{FILE}: job {name} needs the exact `if: {condition}`"
            ));
        }
        if j.and_then(|j| entry(j, "continue-on-error")).is_some() {
            problems.push(format!(
                "{FILE}: job {name} has `continue-on-error`: a failure must stop the deploy"
            ));
        }
    }
    problems.extend(targets_problems(job(doc, TARGETS_JOB)));
    problems.extend(super::coordinator_wait::problems(job(doc, COORDINATOR_JOB)));
    problems.extend(caller_problems(job(doc, JOB)));
    problems
}

fn targets_problems(targets: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    let got = needs(targets);
    if got != ["plan", "publish"] {
        problems.push(format!(
            "{FILE}: job {TARGETS_JOB} needs {got:?}, not [\"plan\", \"publish\"]"
        ));
    }
    if targets.map(|j| pairs(j, "permissions")) != Some(TARGETS_PERMISSIONS.to_vec()) {
        problems.push(format!(
            "{FILE}: job {TARGETS_JOB} must hold exactly `contents: read`"
        ));
    }
    let steps = match targets.and_then(|j| entry(j, "steps")).map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    for (at, step) in steps.iter().enumerate() {
        for (key, _) in entries(step) {
            let key = key.as_str().unwrap_or_default();
            if !STEP_KEYS.contains(&key) {
                problems.push(format!(
                    "{FILE}: job {TARGETS_JOB} step {} has `{key}`, which a step of it may not",
                    at + 1
                ));
            }
        }
    }
    let shapes: Vec<String> = steps.iter().map(step_shape).collect();
    if shapes.len() != TARGETS_STEPS.len() {
        problems.push(format!(
            "{FILE}: job {TARGETS_JOB} has {} steps, not the {} of desired::TARGETS_STEPS",
            shapes.len(),
            TARGETS_STEPS.len()
        ));
    }
    for (at, (got, want)) in shapes.iter().zip(TARGETS_STEPS).enumerate() {
        if got != want {
            problems.push(format!(
                "{FILE}: job {TARGETS_JOB} step {} is `{got}`, not `{want}`",
                at + 1
            ));
        }
    }
    for (at, step) in steps.iter().enumerate() {
        if text(Some(step), "run").is_some_and(|run| run.contains("${{")) {
            problems.push(format!(
                "{FILE}: job {TARGETS_JOB} step {} writes an expression into its `run:`; pass the value through `env:`",
                at + 1
            ));
        }
    }
    let outputs = targets.map(|j| pairs(j, "outputs")).unwrap_or_default();
    if outputs != TARGETS_OUTPUTS {
        problems.push(format!(
            "{FILE}: job {TARGETS_JOB} outputs are {outputs:?}, not {TARGETS_OUTPUTS:?}"
        ));
    }
    problems
}

fn caller_problems(caller: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    let got = needs(caller);
    if got != ["coordinator", "plan", TARGETS_JOB] {
        problems.push(format!(
            "{FILE}: job {JOB} needs {got:?}, not [\"coordinator\", \"plan\", \"{TARGETS_JOB}\"]"
        ));
    }
    let with = caller.and_then(|j| entry(j, "with"));
    if text(with, "service") != Some("hostkeeper") {
        problems.push(format!("{FILE}: job {JOB} must pass `service: hostkeeper`"));
    }
    for input in WITH_FROM_TARGETS {
        let want = format!("${{{{ needs.{TARGETS_JOB}.outputs.{input} }}}}");
        if text(with, input) != Some(want.as_str()) {
            problems.push(format!("{FILE}: job {JOB} must pass `{input}: {want}`"));
        }
    }
    for input in NOT_PASSED {
        if with.and_then(|w| entry(w, input)).is_some() {
            problems.push(format!(
                "{FILE}: job {JOB} passes {input}; a release is set with its native entries alone, each platform's its own"
            ));
        }
    }
    if text(with, "wait") != Some("false") {
        problems.push(format!("{FILE}: job {JOB} must pass `wait: false`"));
    }
    problems
}
