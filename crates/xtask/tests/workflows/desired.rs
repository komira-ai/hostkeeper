//! The rules of hostkeeper's own deploy in `release.yml` (docs/deploy/github-actions.md): the
//! job [`CHANNEL_JOB`], which waits until the release channel serves the newest release of the
//! run, and the job [`JOB`], which sets it as hostkeeper's desired version through the CI client.
//! [`problems`] refuses `release.yml` when:
//! - [`CHANNEL_JOB`] lacks the exact `if:` [`DESIRED_IF`] (a release of main, only while the
//!   repository variable [`GATE`] is `true`, so that the deploy can be turned off), or [`JOB`]
//!   the exact `if:` [`SET_IF`] (the same, and only when this run's release is the newest
//!   published); or either job has `continue-on-error`;
//! - [`CHANNEL_JOB`] does not need exactly `plan` and `publish` (it runs once every release of the
//!   run is published, and not before), holds other token rights than exactly
//!   [`CHANNEL_PERMISSIONS`], has a step with a key outside [`STEP_KEYS`] (a `continue-on-error`,
//!   or an `if:` on any step but the two of the tailnet join, whose `if:` is in their
//!   [`CHANNEL_STEPS`] entry), or its steps are not exactly [`CHANNEL_STEPS`], in that order,
//!   with their `if`, `with` and `env`: the newest version of the run and its commit, xtask
//!   compiled from a checkout of that commit, the published index from the run's own
//!   `release-<tag>` artifact, only when [`JOIN_IF`] holds the channel's host (the step
//!   [`HOST_STEP`]) and the tailnet join that pings it, the wait for the channel, the
//!   byte comparison of the served index with the published one, the index's text and SHA-256 as
//!   outputs, the release's targets written by `cargo xtask release targets` from the same files,
//!   and whether no newer release is published; or its `outputs` are not exactly
//!   [`CHANNEL_OUTPUTS`]; or its join does not use [`JOIN_USES`], the action and commit of the
//!   CI client's join (a test keeps the two the same);
//! - [`JOB`] does not need exactly `plan` and [`CHANNEL_JOB`] (it calls the coordinator only once
//!   the channel serves the release), or its `with` does not set `service: hostkeeper`, the
//!   version, the release index, its digest and the targets from [`CHANNEL_JOB`]
//!   ([`WITH_FROM_CHANNEL`]), and `wait: false`; or passes a `package_digest` (one package for
//!   every host, whatever its platform).
//!
//! That [`JOB`] calls the CI client of this repository at the CI client's revision
//! ([`super::callers::CLIENT_COMMIT`], with its release's `hostkeeper_version` and
//! `hostkeeper_commit`), with exactly `contents: read` and `id-token: write` and no secrets, is
//! the rule of every caller ([`super::callers`]).

use super::lint::{entries, entry};
use super::yaml::{Node, Value};

/// The workflow the two jobs are in.
pub const FILE: &str = "release.yml";

/// The job that waits for the release channel.
pub const CHANNEL_JOB: &str = "channel";

/// The job that calls the CI client.
pub const JOB: &str = "set-desired";

/// The repository variable that turns the deploy on: `true` runs both jobs; unset, or any other
/// value, skips them.
pub const GATE: &str = "RELEASE_SETS_DESIRED";

/// The `if:` of [`CHANNEL_JOB`]: a release of main was planned, and [`GATE`] is `true`.
pub const DESIRED_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' }}";

/// The `if:` of [`JOB`]: [`DESIRED_IF`], and [`CHANNEL_JOB`] found this run's release the newest
/// published, so that a late run of an older commit never moves the fleet back.
pub const SET_IF: &str = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' && needs.channel.outputs.newest == 'true' }}";

/// The keys a step of [`CHANNEL_JOB`] may have: no `if:` that could skip it (but the `if:`
/// [`JOIN_IF`] of the two steps of the join, which [`CHANNEL_STEPS`] writes), no
/// `continue-on-error` that could let it fail, no `shell` or `working-directory` that could
/// change what its command does.
pub const STEP_KEYS: [&str; 6] = ["env", "id", "name", "run", "uses", "with"];

/// The token rights of [`CHANNEL_JOB`], exactly, sorted: it reads the repository, and the join
/// asks GitHub for an OIDC token, as the CI client's join does.
pub const CHANNEL_PERMISSIONS: [(&str, &str); 2] = [("contents", "read"), ("id-token", "write")];

/// The `if:` of the two steps of the tailnet join in [`CHANNEL_JOB`], and the value every caller
/// of the CI client passes as `join_tailnet`: the repository variable of the federated identity's
/// client ID is set. Empty, the runner does not join, and reaches the channel as it is.
pub const JOIN_IF: &str = "${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}";

/// The `id` of the step of [`CHANNEL_JOB`] that writes the channel's host, which the join pings.
pub const HOST_STEP: &str = "channel-host";

/// The `uses:` of the join step of [`CHANNEL_JOB`], before its version comment: the CI client's
/// join action at the same commit (`set-desired.yml`, step `tailnet`).
pub const JOIN_USES: &str = "tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd";

/// The index in [`CHANNEL_STEPS`] of the step [`HOST_STEP`], of the join right after it, and of
/// the wait right after the join.
pub const HOST_AT: usize = 5;
/// See [`HOST_AT`].
pub const JOIN_AT: usize = 6;
/// See [`HOST_AT`].
pub const WAIT_AT: usize = 7;

/// The steps of [`CHANNEL_JOB`], in order, as [`step_shape`] writes them: `if <condition> ` first
/// when the step has an `if:`, then a `run:` step as its command, an action as `uses <action>`,
/// each followed by its `with` and its `env`, every entry as ` with <key>=<value>` or
/// ` env <key>=<value>`, sorted by key.
pub const CHANNEL_STEPS: [&str; 12] = [
    r#"run jq --raw-output '.include[-1] | "version=\(.version)", "tag=\(.tag)", "commit=\(.commit)"' <<< "$MATRIX" >> "$GITHUB_OUTPUT" env MATRIX=${{ needs.plan.outputs.matrix }}"#,
    "uses actions/checkout with persist-credentials=false with ref=${{ steps.newest.outputs.commit }}",
    "run rustup toolchain install",
    "run cargo xtask --help",
    "uses actions/download-artifact with name=release-${{ steps.newest.outputs.tag }} with path=dist",
    r#"if ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} run [[ "$RELEASE_CHANNEL" =~ ^https://([A-Za-z0-9.-]+)(:[0-9]+)?(/.*)?$ ]] || { echo "::error::HOSTKEEPER_RELEASE_CHANNEL is not https://<host>[:<port>][/<path>]"; exit 1; }; echo "hostname=${BASH_REMATCH[1]}" >> "$GITHUB_OUTPUT" env RELEASE_CHANNEL=${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}"#,
    "if ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} uses tailscale/github-action with audience=${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }} with oauth-client-id=${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }} with ping=${{ steps.channel-host.outputs.hostname }} with sha256sum=c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32 with tags=${{ vars.HOSTKEEPER_TAILNET_TAG }} with use-cache=false with version=1.94.2",
    r#"run curl --proto =https --proto-redir =https --tlsv1.2 --fail --silent --show-error --max-time 30 --retry 45 --retry-delay 40 --retry-max-time 1800 --retry-all-errors --output "$RUNNER_TEMP/served-index.json" "$RELEASE_CHANNEL/hostkeeper/$VERSION/index.json" env RELEASE_CHANNEL=${{ vars.HOSTKEEPER_RELEASE_CHANNEL }} env VERSION=${{ steps.newest.outputs.version }}"#,
    r#"run cmp dist/index.json "$RUNNER_TEMP/served-index.json""#,
    r#"run printf 'release_index=%s\nrelease_index_digest=%s\n' "$(< dist/index.json)" "$(sha256sum dist/index.json | cut -d ' ' -f 1)" >> "$GITHUB_OUTPUT""#,
    r#"run cargo xtask release targets --version "$VERSION" --dir dist --repo "$GITHUB_REPOSITORY" --output "$GITHUB_OUTPUT" env VERSION=${{ steps.newest.outputs.version }}"#,
    r#"run gh release list --repo "$GITHUB_REPOSITORY" --exclude-drafts --limit 100 --json tagName --jq '[.[].tagName | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) | ltrimstr("v") | split(".") | map(tonumber)] | max | "newest=\(map(tostring) | join(".") == env.VERSION)"' >> "$GITHUB_OUTPUT" env GH_TOKEN=${{ github.token }} env VERSION=${{ steps.newest.outputs.version }}"#,
];

/// The `outputs` of [`CHANNEL_JOB`], exactly, sorted by name.
pub const CHANNEL_OUTPUTS: [(&str, &str); 5] = [
    ("newest", "${{ steps.latest.outputs.newest }}"),
    ("release_index", "${{ steps.index.outputs.release_index }}"),
    (
        "release_index_digest",
        "${{ steps.index.outputs.release_index_digest }}",
    ),
    ("targets", "${{ steps.targets.outputs.targets }}"),
    ("version", "${{ steps.newest.outputs.version }}"),
];

/// The inputs [`JOB`] passes from the outputs of [`CHANNEL_JOB`], each from the output of its
/// own name.
pub const WITH_FROM_CHANNEL: [&str; 4] = [
    "version",
    "release_index",
    "release_index_digest",
    "targets",
];

fn job<'a>(doc: &'a Node, name: &str) -> Option<&'a Node> {
    entry(doc, "jobs").and_then(|jobs| entry(jobs, name))
}

fn text<'a>(map: Option<&'a Node>, key: &str) -> Option<&'a str> {
    map.and_then(|m| entry(m, key)).and_then(Node::as_str)
}

/// The names `needs` lists (one name or a list), sorted.
fn needs(job: Option<&Node>) -> Vec<&str> {
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
fn pairs<'a>(node: &'a Node, key: &str) -> Vec<(&'a str, &'a str)> {
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

/// The step as [`CHANNEL_STEPS`] writes it.
fn step_shape(step: &Node) -> String {
    let mut shape = text(Some(step), "if").map_or_else(String::new, |c| format!("if {c} "));
    shape.push_str(&match text(Some(step), "run") {
        Some(run) => format!("run {run}"),
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
    for (name, condition) in [(CHANNEL_JOB, DESIRED_IF), (JOB, SET_IF)] {
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
    problems.extend(channel_problems(job(doc, CHANNEL_JOB)));
    problems.extend(caller_problems(job(doc, JOB)));
    problems
}

fn channel_problems(channel: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    let got = needs(channel);
    if got != ["plan", "publish"] {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} needs {got:?}, not [\"plan\", \"publish\"]"
        ));
    }
    if channel.map(|j| pairs(j, "permissions")) != Some(CHANNEL_PERMISSIONS.to_vec()) {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} must hold exactly `contents: read` and `id-token: write`"
        ));
    }
    let steps = match channel.and_then(|j| entry(j, "steps")).map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    for (at, step) in steps.iter().enumerate() {
        for (key, _) in entries(step) {
            let key = key.as_str().unwrap_or_default();
            let joins = key == "if" && CHANNEL_STEPS.get(at).is_some_and(|s| s.starts_with("if "));
            if !STEP_KEYS.contains(&key) && !joins {
                problems.push(format!(
                    "{FILE}: job {CHANNEL_JOB} step {} has `{key}`, which a step of it may not",
                    at + 1
                ));
            }
        }
    }
    let shapes: Vec<String> = steps.iter().map(step_shape).collect();
    if shapes.len() != CHANNEL_STEPS.len() {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} has {} steps, not the {} of desired::CHANNEL_STEPS",
            shapes.len(),
            CHANNEL_STEPS.len()
        ));
    }
    for (at, (got, want)) in shapes.iter().zip(CHANNEL_STEPS).enumerate() {
        if got != want {
            problems.push(format!(
                "{FILE}: job {CHANNEL_JOB} step {} is `{got}`, not `{want}`",
                at + 1
            ));
        }
    }
    if steps.get(HOST_AT).and_then(|s| text(Some(s), "id")) != Some(HOST_STEP) {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} step {} does not have `id: {HOST_STEP}`, the host the join pings",
            HOST_AT + 1
        ));
    }
    let uses = steps.get(JOIN_AT).and_then(|s| text(Some(s), "uses"));
    if uses != Some(JOIN_USES) {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} step {} does not use `{JOIN_USES}`, the CI client's join",
            JOIN_AT + 1
        ));
    }
    let outputs = channel.map(|j| pairs(j, "outputs")).unwrap_or_default();
    if outputs != CHANNEL_OUTPUTS {
        problems.push(format!(
            "{FILE}: job {CHANNEL_JOB} outputs are {outputs:?}, not {CHANNEL_OUTPUTS:?}"
        ));
    }
    problems
}

fn caller_problems(caller: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    let got = needs(caller);
    if got != [CHANNEL_JOB, "plan"] {
        problems.push(format!(
            "{FILE}: job {JOB} needs {got:?}, not [\"{CHANNEL_JOB}\", \"plan\"]"
        ));
    }
    let with = caller.and_then(|j| entry(j, "with"));
    if text(with, "service") != Some("hostkeeper") {
        problems.push(format!("{FILE}: job {JOB} must pass `service: hostkeeper`"));
    }
    for input in WITH_FROM_CHANNEL {
        let want = format!("${{{{ needs.{CHANNEL_JOB}.outputs.{input} }}}}");
        if text(with, input) != Some(want.as_str()) {
            problems.push(format!("{FILE}: job {JOB} must pass `{input}: {want}`"));
        }
    }
    if with.and_then(|w| entry(w, "package_digest")).is_some() {
        problems.push(format!(
            "{FILE}: job {JOB} passes package_digest; the release index names each platform's package"
        ));
    }
    if text(with, "wait") != Some("false") {
        problems.push(format!("{FILE}: job {JOB} must pass `wait: false`"));
    }
    problems
}
