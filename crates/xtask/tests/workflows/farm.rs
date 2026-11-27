//! The rules of `farm.yml` ([`FILE`]), the farm lane: `buck2 build //:ci` on the build farm,
//! additive and advisory beside ci.yml, which stays the pull-request gate.
//!
//! It is the one workflow that runs for a pull request with a job that holds a write scope: its
//! job [`JOB`] holds `id-token: write`, so that komira's farm-connect action ([`FARM_CONNECT`])
//! can join the farm's private network with the job's OIDC token (workload identity federation:
//! no stored secret). [`super::lint`] allows that scope to this job alone, under its exact `if:`
//! ([`JOB_IF`]: a push to main, or a pull request from a branch of this repository that is not
//! Dependabot's; a fork's pull request gets no token and is skipped), and lets this file, and no
//! other, pin farm-connect and komira's buck2-farm action ([`BUCK2_FARM`]) by commit with no
//! version comment (komira tags no release of them). Every other rule of [`super::lint`] holds for
//! the file as for any pull-request workflow: no secret and no token named, no other write scope,
//! no root, no macOS, a Linux x86_64 runner; and [`super::step_bounds`] gives every step a
//! `timeout-minutes` under the job's.
//!
//! [`problems`] holds the file to its shape: the triggers, the one job, its rights, farm-connect's
//! inputs, the farm's builds and the checks between them. It does not fix the other steps of the
//! job, each of which could ask for the token; a change to them is reviewed like any change to a
//! workflow:
//! - its `on` is exactly `pull_request` with no filter and `push` to `branches: [main]`;
//! - its jobs are exactly [`JOB`], on [`RUNNER`], with `permissions` exactly `contents: read`
//!   and `id-token: write`;
//! - farm-connect is used by exactly one step, whose `with:` is exactly [`CONNECT_INPUTS`]: the
//!   tailnet identity (`TS_CLIENT_ID`, `TS_AUDIENCE`) and the farm's address, instance, worker
//!   property set and TLS switch, each from the organisation's variable of its name, and nothing
//!   else (no secret, no token, no input a pull request could set);
//! - every buck2 command that builds on the farm is a step of buck2-farm, whose watchdog ends a
//!   run hung on a dead connection to the farm and runs it again, as it does a run cut by one,
//!   and which fails a run whose summary does not say `local: 0`: exactly two steps use it, one
//!   with `args:` [`TOOL_ARGS`] and one [`CI_ARGS`], each with the pinned buck2 and the built tool
//!   ([`farm_inputs`]); and no `run:` of the job names buck2 ([`names_buck2`]) but
//!   [`WORKERS_RUN`] (`buck2 audit config`, which builds nothing), [`TABLE_RUN`] (`buildrules
//!   check`, and `buildrules sync` for a stale table) and [`KILL_RUN`] (the daemon's stop);
//! - after farm-connect, in this order: [`WORKERS_RUN`], the build of [`TOOL_ARGS`],
//!   [`TABLE_RUN`] (which writes a stale table's fix as a patch to the file [`patch_file`] names),
//!   the build of [`CI_ARGS`];
//! - the tool is built by exactly one step, [`COMPILE_RUN`], from exactly one checkout of komira's
//!   sources ([`tool_checkout`]), both before the first build; farm-connect, both buck2-farm steps
//!   and that checkout name one komira commit, so the action runs the tool its own commit holds;
//! - one step after the build of [`CI_ARGS`] runs [`KILL_RUN`] under exactly [`KILL_IF`];
//! - exactly one step after the build uploads the artifact [`PATCH_ARTIFACT`], under exactly
//!   [`UPLOAD_IF`] (the job failed: the patch exists only when the table check wrote it), with
//!   `actions/upload-artifact` and a `with:` of exactly [`upload_inputs`]: that artifact name and
//!   the patch file, so the command the job summary prints downloads what was uploaded.

use std::collections::BTreeMap;

use super::lint::{entries, entry, triggers};
use super::xtask_calls::commands;
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
/// komira's action that runs one buck2 command on the farm with the watchdog.
pub const BUCK2_FARM: &str = "komira-ai/komira/.github/actions/buck2-farm";
/// The `args:` of the first buck2-farm step: the `buildrules` command line, to a file.
pub const TOOL_ARGS: &str =
    "build buildrules//cli:buildrules --out ${{ runner.temp }}/buildrules-cli";
/// The `args:` of the second: the farm lane's build.
pub const CI_ARGS: &str = "build //:ci";
/// The worker check's `run:`.
pub const WORKERS_RUN: &str = "cargo xtask farm-workers --buck2 \"$RUNNER_TEMP/bin/buck2\"";
/// The table check's `run:`.
pub const TABLE_RUN: &str = "cargo xtask farm-table --buck2 \"$RUNNER_TEMP/bin/buck2\" --buildrules \"$RUNNER_TEMP/buildrules-cli\" --patch \"$RUNNER_TEMP/buildrules-sync/buildrules-sync.patch\" --summary \"$GITHUB_STEP_SUMMARY\" --repo \"$GITHUB_REPOSITORY\" --run-id \"$GITHUB_RUN_ID\"";
/// The daemon's stop.
pub const KILL_RUN: &str = "\"$RUNNER_TEMP/bin/buck2\" kill";
/// The exact `if:` of [`KILL_RUN`]: however the job went, once buck2 is installed.
pub const KILL_IF: &str = "${{ always() && steps.buck2-check.outcome == 'success' }}";
/// The build of the tool from the checkout of [`tool_checkout`].
pub const COMPILE_RUN: &str = "\"$RUNNER_TEMP/zig-checked/zig-x86_64-linux-$ZIG_VERSION/zig\" build-exe -OReleaseSafe -target x86_64-linux-musl -fstrip --cache-dir \"$RUNNER_TEMP/zig-local-cache\" --global-cache-dir \"$RUNNER_TEMP/zig-global-cache\" -femit-bin=\"$RUNNER_TEMP/bin/assert_remote\" buck-out/komira/tools/build/ci/assert_remote/assert_remote.zig";
/// The repository of the two actions and of the tool's sources.
pub const KOMIRA: &str = "komira-ai/komira";
/// The action that checks out the tool's sources, at any commit.
pub const CHECKOUT: &str = "actions/checkout@";

/// The `with:` of a buck2-farm step whose `args:` is `args`, exactly.
pub fn farm_inputs(args: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("buck2", "${{ runner.temp }}/bin/buck2".to_owned()),
        (
            "assert-remote",
            "${{ runner.temp }}/bin/assert_remote".to_owned(),
        ),
        ("args", args.to_owned()),
    ])
}

/// The `with:` of the checkout of the tool's sources at the komira commit `commit`, exactly: the
/// `.zig` files of its directory alone, into `buck-out/` (which git and buck2 pass over), with
/// no credentials kept.
pub fn tool_checkout(commit: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("repository", KOMIRA.to_owned()),
        ("ref", commit.to_owned()),
        ("path", "buck-out/komira".to_owned()),
        (
            "sparse-checkout",
            "/tools/build/ci/assert_remote/*.zig".to_owned(),
        ),
        ("sparse-checkout-cone-mode", "false".to_owned()),
        ("filter", "blob:none".to_owned()),
        ("persist-credentials", "false".to_owned()),
    ])
}

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

/// The commit `value`, a `uses:` value, pins the action `action` to, when it names that action.
fn pin<'a>(value: &'a str, action: &str) -> Option<&'a str> {
    value.strip_prefix(action)?.strip_prefix('@')
}

/// Whether the `uses:` value `value` in `file` is farm-connect or buck2-farm pinned where it may
/// be.
pub fn unversioned_pin_allowed(file: &str, value: &str) -> bool {
    file == FILE && (pin(value, FARM_CONNECT).is_some() || pin(value, BUCK2_FARM).is_some())
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

/// What a job whose steps after farm-connect are not in their order is refused with.
pub const ORDER_PROBLEM: &str = "job build: the steps after farm-connect must be, in this order, the worker check, the buck2-farm build of the buildrules command line, the table check and the buck2-farm build of //:ci";
/// What a job whose komira pins differ is refused with.
pub const PINS_PROBLEM: &str = "job build: farm-connect, both buck2-farm steps and the checkout of assert_remote's sources must name one komira commit";

/// What a job whose buck2-farm steps are not the two builds is refused with.
pub fn farm_steps_problem() -> String {
    format!(
        "job {JOB}: exactly two steps must use {BUCK2_FARM}, one with `with:` exactly {:?} and one with {:?}",
        farm_inputs(TOOL_ARGS),
        farm_inputs(CI_ARGS)
    )
}

/// What a job whose tool is not built from komira's sources before the builds is refused with.
pub fn tool_problem() -> String {
    format!(
        "job {JOB}: exactly one step must check out assert_remote's sources from {KOMIRA} (`with:` exactly {:?} but its `ref:`), and exactly one after it run `{COMPILE_RUN}`, both before the first {BUCK2_FARM} step",
        tool_checkout("<commit>")
    )
}

/// What a job without the daemon's stop after the build is refused with.
pub fn kill_problem() -> String {
    format!(
        "job {JOB}: exactly one step, after the build of {CI_ARGS}, must run `{KILL_RUN}` under exactly `if: {KILL_IF}`"
    )
}

/// What a `run:` that names buck2 and is none of the three allowed is refused with.
pub fn outside_problem(run: &str) -> String {
    format!("job {JOB}: `{run}` runs buck2 outside {BUCK2_FARM}")
}

/// Whether the script `run` names buck2 as a program it runs or hands on: a word `buck2`, or one
/// that ends `/buck2`, in any of its commands, but as the value of `--out` (the install of the
/// pinned buck2 writes it there).
pub fn names_buck2(run: &str) -> bool {
    commands(run).iter().any(|words| {
        words.iter().enumerate().any(|(i, w)| {
            (w == "buck2" || w.ends_with("/buck2")) && (i == 0 || words[i - 1] != "--out")
        })
    })
}

/// The `uses:` of `step`.
fn uses_of(step: &Node) -> Option<&str> {
    entry(step, "uses").and_then(Node::as_str)
}

/// The `run:` of `step`.
fn run_of(step: &Node) -> Option<&str> {
    entry(step, "run").and_then(Node::as_str)
}

/// The scalar `with:` of `step`, by key.
fn with_of(step: &Node) -> Option<BTreeMap<&str, &str>> {
    entry(step, "with").and_then(scalar_map)
}

/// `map` with its values borrowed, to compare with a [`with_of`].
fn borrowed<'a>(map: &'a BTreeMap<&'static str, String>) -> BTreeMap<&'a str, &'a str> {
    map.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The positions of the steps of `steps` that `keep` keeps.
fn positions(steps: &[Node], keep: impl Fn(&Node) -> bool) -> Vec<usize> {
    (0..steps.len()).filter(|&i| keep(&steps[i])).collect()
}

/// Whether `step` checks out komira's sources.
fn checks_out_komira(step: &Node) -> bool {
    uses_of(step).is_some_and(|u| u.starts_with(CHECKOUT))
        && with_of(step).is_some_and(|w| w.get("repository") == Some(&KOMIRA))
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
    let connects = positions(steps, |s| {
        uses_of(s).is_some_and(|u| pin(u, FARM_CONNECT).is_some())
    });
    let inputs = BTreeMap::from(CONNECT_INPUTS);
    match &connects[..] {
        [only] if with_of(&steps[*only]).as_ref() == Some(&inputs) => {}
        [_] => problems.push(format!(
            "job {JOB}: the farm-connect step's `with:` must be exactly {CONNECT_INPUTS:?}"
        )),
        _ => problems.push(format!(
            "job {JOB}: exactly one step must use {FARM_CONNECT}"
        )),
    }
    let farms = positions(steps, |s| {
        uses_of(s).is_some_and(|u| pin(u, BUCK2_FARM).is_some())
    });
    let build_of = |args: &str| {
        let inputs = farm_inputs(args);
        farms
            .iter()
            .copied()
            .find(|&i| with_of(&steps[i]) == Some(borrowed(&inputs)))
    };
    let builds = match (farms.len(), build_of(TOOL_ARGS), build_of(CI_ARGS)) {
        (2, Some(tool), Some(ci)) => Some((tool, ci)),
        _ => {
            problems.push(farm_steps_problem());
            None
        }
    };
    let first_run = |want: &str| steps.iter().position(|s| run_of(s) == Some(want));
    if let Some((tool, ci)) = builds {
        let order = [
            connects.first().copied(),
            first_run(WORKERS_RUN),
            Some(tool),
            first_run(TABLE_RUN),
            Some(ci),
        ];
        let in_order =
            order.iter().all(Option::is_some) && order.windows(2).all(|pair| pair[0] < pair[1]);
        if !in_order {
            problems.push(ORDER_PROBLEM.to_owned());
        }
    }
    let checkouts = positions(steps, checks_out_komira);
    let compiles = positions(steps, |s| run_of(s) == Some(COMPILE_RUN));
    let tool_built = match (&checkouts[..], &compiles[..]) {
        ([checkout], [compile]) => {
            let with = with_of(&steps[*checkout]);
            let commit = with
                .as_ref()
                .and_then(|w| w.get("ref").copied())
                .unwrap_or_default();
            with == Some(borrowed(&tool_checkout(commit)))
                && checkout < compile
                && farms.first().is_none_or(|first| compile < first)
        }
        _ => false,
    };
    if !tool_built {
        problems.push(tool_problem());
    }
    let mut commits: Vec<&str> = connects
        .iter()
        .chain(&farms)
        .filter_map(|&i| {
            uses_of(&steps[i])
                .and_then(|u| u.split_once('@'))
                .map(|(_, c)| c)
        })
        .chain(
            checkouts
                .iter()
                .filter_map(|&i| with_of(&steps[i]).and_then(|w| w.get("ref").copied())),
        )
        .collect();
    commits.sort_unstable();
    commits.dedup();
    if commits.len() > 1 {
        problems.push(PINS_PROBLEM.to_owned());
    }
    for run in steps.iter().filter_map(run_of) {
        if names_buck2(run) && ![WORKERS_RUN, TABLE_RUN, KILL_RUN].contains(&run) {
            problems.push(outside_problem(run));
        }
    }
    let ci = builds.map(|(_, ci)| ci);
    let kills = positions(steps, |s| run_of(s) == Some(KILL_RUN));
    let stopped = match &kills[..] {
        [kill] => {
            entry(&steps[*kill], "if").and_then(Node::as_str) == Some(KILL_IF)
                && ci.is_none_or(|ci| ci < *kill)
        }
        _ => false,
    };
    if !stopped {
        problems.push(kill_problem());
    }
    problems.extend(upload_problems(steps, ci));
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
