//! The jobs of this repository that call the CI client, `set-desired.yml`
//! (docs/deploy/ci-client.md), and the rules of `hello-desired.yml`, the hello proof run. The
//! rules of the other caller, hostkeeper's own deploy in `release.yml`, are [`super::desired`].
//!
//! A job that calls a reusable workflow has no `runs-on` and no steps, so the shared rules of
//! [`super::lint`] refuse it, and grant `id-token: write` to no job of a caller. The one exception
//! is a job of [`CALLERS`] that calls the CI client. [`problems`] refuses the workflow when:
//! - a job calls `set-desired.yml` (its job-level `uses:` names the file) and is not in
//!   [`CALLERS`]: every way CI sets a desired version is listed here;
//! - a job of [`CALLERS`] calls another workflow than [`SET_DESIRED`] of this repository (the
//!   pin, a full commit with a version comment, is the shared rule's), has another `if:` than its
//!   entry gives, a `permissions` that is not exactly `contents: read` and `id-token: write`, or
//!   passes `secrets` (the CI client takes none: the OIDC token is its one credential);
//! - a job of [`CALLERS`] does not run the CI client's revision: its `uses:` is not pinned to
//!   [`CLIENT_COMMIT`], the version comment on that line does not start with
//!   `v`[`CLIENT_RELEASE`], or its `with` does not pass `hostkeeper_version: `[`CLIENT_RELEASE`]
//!   and `hostkeeper_commit: `[`CLIENT_COMMIT`] (the release whose hkctl the job downloads, and
//!   the commit its attestation must name);
//! - a job of [`CALLERS`] does not pass `tailnet_tag: `[`TAILNET_TAG`], whether or not it joins
//!   the tailnet: the tag comes from one repository variable, never from the CI client's default
//!   or a tag written into the caller; or does not pass each of [`JOIN_FROM_VARS`] exactly: it
//!   joins the tailnet whenever the repository variable of the federated identity's client ID
//!   is set, so a coordinator served only on the tailnet is reachable from every caller;
//! - a job of [`CALLERS`] for the file is missing, or does not call the CI client;
//! - it is the file of a proof run, [`HELLO_RUN`] or [`TEMPLATE_RUN`], and runs on anything but
//!   `workflow_dispatch`, lacks the input `version` (a required string) or, for hello, `variant`
//!   (a required choice of exactly [`HELLO_VARIANTS`]), or its caller does not pass the run's
//!   `service`, or a job of it lacks the exact `if:` [`HELLO_IF`] (a dispatch from another branch
//!   runs nothing);
//! - it is the file of a proof run and its caller does not pass each of the run's inputs from
//!   its release job ([`HELLO_FROM_RELEASE`] for hello: the version and the targets alone;
//!   [`TEMPLATE_FROM_RELEASE`] for the template) from the output of that name, passes an input
//!   of [`INDEX_INPUTS`] the run does not list (hello sets native entries alone, with no index),
//!   or passes a `package_digest` (one platform's package for every host); or the `outputs` of
//!   the release job are not exactly the run's ([`HELLO_RELEASE_OUTPUTS`],
//!   [`TEMPLATE_RELEASE_OUTPUTS`]); or its step `targets` does not run the run's targets command.
//!
//! The caller template a service of another repository copies, the one `yaml` block of
//! [`template_doc::DOC`], is linted under that file name ([`super::template_doc`]): [`TEMPLATE`]
//! is its caller entry, which [`caller`] finds beside [`CALLERS`], and [`TEMPLATE_RUN`] holds it
//! to the rules of `hello-desired.yml` for a placeholder service.

use super::desired;
use super::lint::{entries, entry, triggers, version_comment};
use super::template_doc;
use super::yaml::{Node, Value};

/// The CI client, as a caller's `uses:` names it before the `@`.
pub const SET_DESIRED: &str = "komira-ai/hostkeeper/.github/workflows/set-desired.yml";

/// The revision of the CI client every job of [`CALLERS`] runs: the commit of this repository
/// whose `set-desired.yml` it calls, which is also the commit the release [`CLIENT_RELEASE`] was
/// built from. It is the first release whose `set-desired.yml` takes the tailnet tag,
/// `tailnet_tag` ([`TAILNET_TAG`]), beside a release's targets (docs/design/packages.md, section
/// 3.7), on the client of hkctl, one set and one held wait (docs/design/two-programs.md,
/// section 5). Moving it is one reviewed change: these two
/// values, the `uses:` line, `hostkeeper_version` and `hostkeeper_commit` of both callers, and the
/// coordinator's allow-list entry (docs/deploy/github-actions.md, "The allow-list entry").
pub const CLIENT_COMMIT: &str = "2c767d1842a88edb7120073d2dff3b4bfccc834c";

/// The release built from [`CLIENT_COMMIT`], X.Y.Z: the one whose `hkctl-<version>` the CI client
/// downloads and verifies.
pub const CLIENT_RELEASE: &str = "0.1.184";

/// What every job of [`CALLERS`] passes as `tailnet_tag`, exactly: the repository variable that
/// holds the tag the tailnet's trust credential issues (docs/deploy/github-actions.md, "The
/// repository variables"). The CI client checks the tag whether or not the job joins, so an empty
/// variable ends the job at its first step, before anything is downloaded.
pub const TAILNET_TAG: &str = "${{ vars.HOSTKEEPER_TAILNET_TAG }}";

/// What every job of [`CALLERS`] passes for the tailnet join, exactly: it joins when the
/// repository variable `HOSTKEEPER_TAILNET_CLIENT_ID` is set ([`desired::JOIN_IF`]), with the
/// federated identity and the coordinator's tailnet name from the repository variables.
pub const JOIN_FROM_VARS: [(&str, &str); 4] = [
    ("join_tailnet", desired::JOIN_IF),
    (
        "tailnet_client_id",
        "${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}",
    ),
    (
        "tailnet_audience",
        "${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}",
    ),
    (
        "tailnet_coordinator",
        "${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}",
    ),
];

/// A job that calls [`SET_DESIRED`]: the workflow file, the job, and its `if:`, exactly.
pub struct Caller {
    pub file: &'static str,
    pub job: &'static str,
    pub condition: &'static str,
}

/// The hello proof run.
pub const HELLO_FILE: &str = "hello-desired.yml";
/// Its job that calls the CI client.
pub const HELLO_JOB: &str = "set-desired";
/// Its `if:`: a dispatch from main only.
pub const HELLO_IF: &str = "${{ github.ref == 'refs/heads/main' }}";
/// The values of its input `variant`, in order: the build that passes, and the one whose
/// `/readyz` answers 503 (docs/hello.md, step 4).
pub const HELLO_VARIANTS: [&str; 2] = ["hello", "fail-ready"];
/// Its job that reads the release and writes the targets.
pub const HELLO_RELEASE_JOB: &str = "release";
/// The inputs [`HELLO_JOB`] passes from the outputs of [`HELLO_RELEASE_JOB`], each from the
/// output of its own name: the version and its targets alone, native entries every host is
/// ordered its own platform's of (docs/design/packages-native.md section 10.5, A24).
pub const HELLO_FROM_RELEASE: [&str; 2] = ["version", "targets"];
/// The `outputs` of [`HELLO_RELEASE_JOB`], exactly, sorted by name: the targets from the step
/// that runs [`HELLO_TARGETS_RUN`], the version from the step `version`.
pub const HELLO_RELEASE_OUTPUTS: [(&str, &str); 2] = [
    ("targets", "${{ steps.targets.outputs.targets }}"),
    ("version", "${{ steps.version.outputs.version }}"),
];
/// The inputs of the CI client that carry a release index. hello's caller passes none of them:
/// a release is set with native entries alone, and no index.
pub const INDEX_INPUTS: [&str; 2] = ["release_index", "release_index_digest"];
/// The inputs the caller template passes from the outputs of its release job: a service of
/// another repository still ships archives, and gives the index with its targets so that a
/// daemon before targets is ordered its archive (docs/deploy/service-caller.md).
pub const TEMPLATE_FROM_RELEASE: [&str; 4] = [
    "version",
    "targets",
    "release_index",
    "release_index_digest",
];
/// The `outputs` of the template's release job, exactly, sorted by name.
pub const TEMPLATE_RELEASE_OUTPUTS: [(&str, &str); 4] = [
    ("release_index", "${{ steps.index.outputs.release_index }}"),
    (
        "release_index_digest",
        "${{ steps.index.outputs.release_index_digest }}",
    ),
    ("targets", "${{ steps.targets.outputs.targets }}"),
    ("version", "${{ steps.index.outputs.version }}"),
];
/// The `run:` of the step `targets` of [`HELLO_RELEASE_JOB`], before its arguments: the targets
/// are written by the verb that checks the release's files, for hello.
pub const HELLO_TARGETS_RUN: &str = "cargo xtask release targets --service hello ";

/// A workflow that reads a release, writes its targets and calls the CI client with them, run by
/// hand from main: `hello-desired.yml`, and the caller template of [`template_doc::DOC`].
pub struct Proof {
    /// The workflow file, or the page the template is read from.
    pub file: &'static str,
    /// Its job that calls the CI client.
    pub job: &'static str,
    /// Its job that reads the release and writes the targets.
    pub release_job: &'static str,
    /// The `service` its caller passes.
    pub service: &'static str,
    /// The values of its input `variant`, in order; none: it has no such input to check.
    pub variants: &'static [&'static str],
    /// The `run:` of the step `targets` of the release job, before its arguments.
    pub targets_run: &'static str,
    /// The inputs its caller passes from the outputs of the release job, each from the output
    /// of its own name; an input of [`INDEX_INPUTS`] not among them may not be passed at all.
    pub from_release: &'static [&'static str],
    /// The `outputs` of its release job, exactly, sorted by name.
    pub release_outputs: &'static [(&'static str, &'static str)],
}

/// The hello proof run.
pub const HELLO_RUN: Proof = Proof {
    file: HELLO_FILE,
    job: HELLO_JOB,
    release_job: HELLO_RELEASE_JOB,
    service: "hello",
    variants: &HELLO_VARIANTS,
    targets_run: HELLO_TARGETS_RUN,
    from_release: &HELLO_FROM_RELEASE,
    release_outputs: &HELLO_RELEASE_OUTPUTS,
};

/// The caller template: the same shape for the placeholder service `my-service`, whose targets
/// come from the service's own command, shown as a verb of its own xtask.
pub const TEMPLATE_RUN: Proof = Proof {
    file: template_doc::DOC,
    job: "set-desired",
    release_job: "release",
    service: "my-service",
    variants: &[],
    targets_run: "cargo xtask release targets ",
    from_release: &TEMPLATE_FROM_RELEASE,
    release_outputs: &TEMPLATE_RELEASE_OUTPUTS,
};

/// The caller entry of the template: not a job of this repository, so not in [`CALLERS`], but
/// held to every rule of one.
pub static TEMPLATE: Caller = Caller {
    file: TEMPLATE_RUN.file,
    job: TEMPLATE_RUN.job,
    condition: HELLO_IF,
};

/// Every job of this repository that calls the CI client: the hello proof run, and
/// hostkeeper's own deploy in release.yml ([`desired`]).
pub const CALLERS: &[Caller] = &[
    Caller {
        file: HELLO_FILE,
        job: HELLO_JOB,
        condition: HELLO_IF,
    },
    Caller {
        file: desired::FILE,
        job: desired::JOB,
        condition: desired::SET_IF,
    },
];

/// Every caller entry: [`CALLERS`], then [`TEMPLATE`].
fn every_caller() -> impl Iterator<Item = &'static Caller> {
    CALLERS.iter().chain(std::iter::once(&TEMPLATE))
}

/// The caller entry of the job `job` of `file` ([`CALLERS`] or [`TEMPLATE`]), if it is one.
pub fn caller(file: &str, job: &str) -> Option<&'static Caller> {
    every_caller().find(|c| c.file == file && c.job == job)
}

/// The job-level `uses:` of `job`: the reusable workflow it calls, if any.
pub fn job_uses(job: &Node) -> Option<&str> {
    entry(job, "uses").and_then(Node::as_str)
}

/// Whether the `uses:` value names the CI client, by any owner, repository or ref.
pub fn names_set_desired(uses: &str) -> bool {
    let path = uses.split_once('@').map_or(uses, |(path, _)| path);
    path.rsplit('/').next() == Some("set-desired.yml")
}

/// The jobs of `doc`, by name.
fn jobs(doc: &Node) -> impl Iterator<Item = (&str, &Node)> {
    entry(doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| (k.as_str().unwrap_or_default(), v))
}

/// The `(scope, level)` pairs of a job's `permissions`, sorted; empty when it has none.
fn permissions(job: &Node) -> Vec<(&str, &str)> {
    let mut found: Vec<(&str, &str)> = entry(job, "permissions")
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

/// One line per rule of this module the workflow `doc` of the file `file` breaks.
pub fn problems(file: &str, text: &str, doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, job) in jobs(doc) {
        let Some(uses_node) = entry(job, "uses") else {
            continue;
        };
        let Some(uses) = uses_node.as_str().filter(|u| names_set_desired(u)) else {
            continue;
        };
        let Some(c) = caller(file, name) else {
            let listed: Vec<String> = CALLERS
                .iter()
                .map(|c| format!("{}/{}", c.file, c.job))
                .collect();
            problems.push(format!(
                "job {name} calls set-desired.yml, and only {listed:?} may"
            ));
            continue;
        };
        let path = uses.split_once('@').map_or(uses, |(path, _)| path);
        if path != SET_DESIRED {
            problems.push(format!("job {name} calls {path}, not {SET_DESIRED}"));
        }
        if entry(job, "if").and_then(Node::as_str) != Some(c.condition) {
            problems.push(format!("job {name} needs the exact `if: {}`", c.condition));
        }
        if permissions(job) != [("contents", "read"), ("id-token", "write")] {
            problems.push(format!(
                "job {name}: permissions must be exactly contents: read and id-token: write"
            ));
        }
        if entry(job, "secrets").is_some() {
            problems.push(format!("job {name} passes secrets to set-desired.yml"));
        }
        problems.extend(revision_problems(name, text, job, uses, uses_node.line()));
        if text_of(entry(job, "with"), "tailnet_tag") != Some(TAILNET_TAG) {
            problems.push(format!(
                "job {name} must pass `tailnet_tag: {TAILNET_TAG}`, the tag the tailnet's trust credential issues"
            ));
        }
        for (input, want) in JOIN_FROM_VARS {
            if text_of(entry(job, "with"), input) != Some(want) {
                problems.push(format!(
                    "job {name} must pass `{input}: {want}`, so it joins the tailnet whenever the client ID is set"
                ));
            }
        }
    }
    for c in every_caller().filter(|c| c.file == file) {
        let calls = jobs(doc)
            .find(|(name, _)| *name == c.job)
            .and_then(|(_, job)| job_uses(job))
            .is_some_and(names_set_desired);
        if !calls {
            problems.push(format!(
                "{file}: job {} does not call set-desired.yml",
                c.job
            ));
        }
    }
    for run in [&HELLO_RUN, &TEMPLATE_RUN]
        .into_iter()
        .filter(|r| r.file == file)
    {
        problems.extend(proof_problems(run, doc));
    }
    problems
}

/// What keeps the caller job `name` from running the CI client's revision, [`CLIENT_COMMIT`] and
/// [`CLIENT_RELEASE`]: its `uses:` value is `uses`, written on the line `line` of `text`.
fn revision_problems(name: &str, text: &str, job: &Node, uses: &str, line: usize) -> Vec<String> {
    let mut problems = Vec::new();
    let pin = uses.split_once('@').map(|(_, at)| at);
    if pin != Some(CLIENT_COMMIT) {
        problems.push(format!(
            "job {name} must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `{uses}`"
        ));
    }
    let release = format!("v{CLIENT_RELEASE}");
    let comment = version_comment(text, line, uses).unwrap_or_default();
    if comment.split_whitespace().next() != Some(release.as_str()) {
        problems.push(format!(
            "job {name} must name the CI client's release `# {release}` after its `uses:`, not `# {comment}`"
        ));
    }
    let with = entry(job, "with");
    for (input, want) in [
        ("hostkeeper_version", CLIENT_RELEASE),
        ("hostkeeper_commit", CLIENT_COMMIT),
    ] {
        if text_of(with, input) != Some(want) {
            problems.push(format!(
                "job {name} must pass `{input}: {want}`, the CI client's revision"
            ));
        }
    }
    problems
}

/// The text of a scalar entry of `map`, if there is one.
fn text_of<'a>(map: Option<&'a Node>, key: &str) -> Option<&'a str> {
    map.and_then(|m| entry(m, key)).and_then(Node::as_str)
}

/// What the proof run `run` (`hello-desired.yml`, or the caller template) must be beyond the
/// rules of every caller.
fn proof_problems(run: &Proof, doc: &Node) -> Vec<String> {
    let file = run.file;
    let mut problems = Vec::new();
    let on = entry(doc, "on");
    if on.map(triggers).unwrap_or_default() != ["workflow_dispatch"] {
        problems.push(format!("{file}: on must be workflow_dispatch only"));
    }
    for (name, job) in jobs(doc) {
        if entry(job, "if").and_then(Node::as_str) != Some(HELLO_IF) {
            problems.push(format!(
                "{file}: job {name} needs the exact `if: {HELLO_IF}`"
            ));
        }
    }
    let inputs = on
        .and_then(|o| entry(o, "workflow_dispatch"))
        .and_then(|d| entry(d, "inputs"));
    let input = |name: &str| inputs.and_then(|i| entry(i, name));
    let version = input("version");
    if text_of(version, "type") != Some("string") || text_of(version, "required") != Some("true") {
        problems.push(format!("{file}: input version must be a required string"));
    }
    if !run.variants.is_empty() {
        let variant = input("variant");
        let options: Vec<&str> = match variant.and_then(|v| entry(v, "options")).map(|o| &o.value) {
            Some(Value::Seq(items)) => items.iter().filter_map(Node::as_str).collect(),
            _ => Vec::new(),
        };
        let choice = text_of(variant, "type") == Some("choice")
            && text_of(variant, "required") == Some("true");
        if !choice || options != run.variants {
            problems.push(format!(
                "{file}: input variant must be a required choice of exactly {:?}",
                run.variants
            ));
        }
    }
    let job = jobs(doc).find(|(name, _)| *name == run.job).map(|(_, j)| j);
    let with = job.and_then(|j| entry(j, "with"));
    if text_of(with, "service") != Some(run.service) {
        problems.push(format!(
            "{file}: job {} must pass `service: {}`",
            run.job, run.service
        ));
    }
    for input in run.from_release {
        let want = format!("${{{{ needs.{}.outputs.{input} }}}}", run.release_job);
        if text_of(with, input) != Some(want.as_str()) {
            problems.push(format!(
                "{file}: job {} must pass `{input}: {want}`",
                run.job
            ));
        }
    }
    for input in INDEX_INPUTS
        .into_iter()
        .filter(|i| !run.from_release.contains(i))
    {
        if with.and_then(|w| entry(w, input)).is_some() {
            problems.push(format!(
                "{file}: job {} passes {input}; a release is set with native entries alone, and no index",
                run.job
            ));
        }
    }
    if with.and_then(|w| entry(w, "package_digest")).is_some() {
        problems.push(format!(
            "{file}: job {} passes package_digest; the targets name each platform's package",
            run.job
        ));
    }
    problems.extend(release_job_problems(run, doc));
    problems
}

/// What the release job of the proof run `run` must be: its outputs exactly the run's
/// `release_outputs`, and its step `targets` running the run's targets command.
fn release_job_problems(run: &Proof, doc: &Node) -> Vec<String> {
    let (file, release_job) = (run.file, run.release_job);
    let mut problems = Vec::new();
    let job = jobs(doc)
        .find(|(name, _)| *name == release_job)
        .map(|(_, j)| j);
    let mut outputs: Vec<(&str, &str)> = job
        .and_then(|j| entry(j, "outputs"))
        .map_or(&[][..], entries)
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default(),
                v.as_str().unwrap_or_default(),
            )
        })
        .collect();
    outputs.sort_unstable();
    if outputs != run.release_outputs {
        problems.push(format!(
            "{file}: job {release_job} outputs are {outputs:?}, not {:?}",
            run.release_outputs
        ));
    }
    let steps = match job.and_then(|j| entry(j, "steps")).map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    let writes = steps.iter().any(|step| {
        text_of(Some(step), "id") == Some("targets")
            && text_of(Some(step), "run").is_some_and(|r| r.starts_with(run.targets_run))
    });
    if !writes {
        problems.push(format!(
            "{file}: job {release_job} needs a step `targets` that runs `{}...`",
            run.targets_run
        ));
    }
    problems
}
