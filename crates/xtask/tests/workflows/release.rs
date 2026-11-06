//! The rules of the release path, on top of the rules every workflow is held to ([`super::lint`]).
//!
//! - A workflow that tags or publishes ([`publishes`]) runs on no pull-request trigger, and it is
//!   in the one release concurrency group ([`RELEASE_GROUP`], never cancelled) unless
//!   [`RELEASE_EXEMPT`] lists it with a reason ([`publish_problems`], run over every workflow of
//!   the repository).
//! - `release.yml` ([`release_problems`]) starts only by [`RELEASE_TRIGGERS`] (a `workflow_run`
//!   of exactly `ci`, `privileged` and `artifacts`, completed, on main), has exactly the jobs
//!   `plan`, `verify` and `publish` under their exact `if:`, checks both shipped binaries
//!   ([`SHIPPED_BINARIES`]) in `verify`, and
//!   has no step that builds or signs ([`build_problems`]).
//! - `artifacts.yml` ([`artifacts_problems`]) starts only by a push to main or by hand, has its
//!   own group per commit ([`ARTIFACTS_GROUP`]), computes the version on a whole history, builds
//!   the coordinator's image twice, compares the two, checks that the image holds the operator
//!   web app and runs the image's entrypoint ([`IMAGE_BUILD`], [`IMAGE_COMPARE`],
//!   [`IMAGE_UI_CHECK`], [`IMAGE_RUN`]), and its `attest` job, under its exact
//!   `if:`, runs no repository code: no checkout, and no `run:` but the checks of
//!   [`ATTEST_RUNS`] and the bundle copies of [`BUNDLE_COPY`]. It attests every package of
//!   [`PACKAGES`] for every platform of [`PLATFORMS`] and copies the bundle of each beside it
//!   ([`package_problems`]): a host trusts only attested packages, so a release asset list that
//!   names a package's bundle needs the steps that make it.
//!
//! Which job may hold which write scope, under which `if:`, is [`super::lint::WRITE_ALLOWED`].

use super::lint::{entries, entry, runs_on_pull_requests, triggers};
use super::yaml::{self, Node, Value};

/// The `if:` of the attest job: a push to main. The only condition under which a token can sign.
pub const SIGN_IF: &str = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}";

/// The `if:` of the plan job: a run of main, by hand, on the schedule, or the completion of a
/// push run on main of this repository.
pub const PLAN_IF: &str = "${{ github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' || github.event_name == 'schedule' || (github.event_name == 'workflow_run' && github.event.workflow_run.event == 'push' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository)) }}";

/// The `if:` of the jobs after the plan: the plan said to release, on a run of main.
pub const RELEASE_IF: &str =
    "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' }}";

/// The group every workflow that tags or publishes is in: a run of main of this repository joins
/// `release-main`; any other takes a group of its own, so it cannot replace the release waiting.
pub const RELEASE_GROUP: &str = "release-${{ (github.ref == 'refs/heads/main' && (github.event_name != 'workflow_run' || (github.event.workflow_run.event == 'push' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository))) && 'main' || github.run_id }}";

/// The group of artifacts.yml: one per commit.
pub const ARTIFACTS_GROUP: &str = "artifacts-${{ github.sha }}";

/// Workflows outside the release group on purpose, each with its reason.
pub const RELEASE_EXEMPT: &[(&str, &str)] = &[(
    "artifacts.yml",
    "builds and attests one commit and never tags or publishes; its own per-commit group",
)];

/// The triggers release.yml may have.
pub const RELEASE_TRIGGERS: [&str; 3] = ["schedule", "workflow_dispatch", "workflow_run"];

/// The workflows whose completion starts release.yml, sorted: the three a release requires.
pub const REQUIRED_WORKFLOWS: [&str; 3] = ["artifacts", "ci", "privileged"];

/// The binaries a release ships, each of which the verify job of release.yml runs.
pub const SHIPPED_BINARIES: [&str; 2] = ["hostkeeper", "hostkeeper-coordinator"];

/// How each of the two builds of the coordinator's image in the image job of artifacts.yml
/// starts; each must also pass `--no-cache`.
pub const IMAGE_BUILD: &str = "docker buildx build --builder image-";

/// How the image job's check that the two builds are byte for byte equal starts.
pub const IMAGE_COMPARE: &str = "cargo xtask release compare-images ";

/// The image job's check that the image holds the operator web app (`ui/` of the commit).
pub const IMAGE_UI_CHECK: &str =
    "cargo xtask release check-image-ui \"$RUNNER_TEMP/image-a.tar\" ui";

/// How the image job's run of the image through its entrypoint starts; it ends in `--version`
/// compared with the coordinator's stamp.
pub const IMAGE_RUN: &str = "test \"$(docker run --rm ";

/// The triggers artifacts.yml may have.
pub const ARTIFACTS_TRIGGERS: [&str; 2] = ["push", "workflow_dispatch"];

/// The `run:` steps the attest job may have, whole: the two checks of the sums.
pub const ATTEST_RUNS: [&str; 2] = [
    "/usr/bin/sha256sum --check --strict <<< \"$SUMS_SHA256  dist/SHA256SUMS\"",
    "/usr/bin/sha256sum --check --strict SHA256SUMS",
];

/// How the one other `run:` of the attest job, the copy of a bundle, starts and ends: between
/// them a file name, with no quote, no command separator and no substitution.
pub const BUNDLE_COPY: (&str, &str) = ("cp \"$BUNDLE\" \"dist/", ".sigstore.json\"");

/// The packages the release carries, each as the part of its file name before the version and the
/// part between the version and the platform: `<prefix>-<version><middle>-<platform>.tar.zst`.
/// They are hostkeeper's own, hello's, and hello's failing build.
pub const PACKAGES: [(&str, &str); 3] =
    [("hostkeeper", ""), ("hello", ""), ("hello", "-fail-ready")];

/// The platforms the release ships packages for.
pub const PLATFORMS: [&str; 3] = ["linux-x86_64", "linux-arm64", "macos-arm64"];

/// Whether the attest job may run `run`: one of [`ATTEST_RUNS`], or a [`BUNDLE_COPY`].
fn attest_may_run(run: &str) -> bool {
    let (start, end) = BUNDLE_COPY;
    let copy = run
        .strip_prefix(start)
        .and_then(|rest| rest.strip_suffix(end))
        .is_some_and(|name| {
            !name.is_empty()
                && !name.contains(['"', '\'', ';', '&', '|', '`', '\n', ' ', '<', '>'])
                && !name.contains("$(")
        });
    copy || ATTEST_RUNS.contains(&run)
}

/// What marks a workflow as tagging or publishing, on a line that is not a comment. Signing
/// alone (`id-token`, the attest actions) is not.
const PUBLISH_MARKS: [&str; 6] = [
    "contents: write",
    "gh release",
    "git tag",
    "git push",
    "refs/tags",
    "xtask release publish",
];

/// The `cargo xtask release` verbs release.yml may run: none builds, packages or signs.
const RELEASE_VERBS: [&str; 5] = ["check", "check-binary", "locate", "plan", "publish"];

/// Words of a `run:` that build, package or sign, refused in release.yml.
const BUILD_WORDS: [&str; 8] = [
    "rustc", "docker", "podman", "buildah", "nfpm", "cosign", "tar", "gzip",
];

/// Whether the workflow `text` tags or publishes: a line that is not a comment holds one of
/// [`PUBLISH_MARKS`].
pub fn publishes(text: &str) -> bool {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .any(|l| PUBLISH_MARKS.iter().any(|m| l.contains(m)))
}

/// The trigger names of `doc`, sorted.
fn trigger_names(doc: &Node) -> Vec<String> {
    let mut found = entry(doc, "on").map(triggers).unwrap_or_default();
    found.sort();
    found
}

/// The scalars of a list (or one scalar) `node`, sorted.
fn names(node: Option<&Node>) -> Vec<String> {
    let mut found: Vec<String> = match node.map(|n| &n.value) {
        Some(Value::Seq(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        Some(_) => node
            .and_then(Node::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        None => Vec::new(),
    };
    found.sort();
    found
}

/// The steps of `job`.
fn steps(job: Option<&Node>) -> &[Node] {
    match job.and_then(|j| entry(j, "steps")).map(|s| &s.value) {
        Some(Value::Seq(items)) => items,
        _ => &[],
    }
}

/// The `key:` text of each step of `job` that has one.
fn step_values<'a>(job: Option<&'a Node>, key: &str) -> Vec<&'a str> {
    steps(job)
        .iter()
        .filter_map(|s| entry(s, key).and_then(Node::as_str))
        .collect()
}

fn job<'a>(doc: &'a Node, name: &str) -> Option<&'a Node> {
    entry(doc, "jobs").and_then(|jobs| entry(jobs, name))
}

fn text_of<'a>(node: Option<&'a Node>, key: &str) -> Option<&'a str> {
    node.and_then(|n| entry(n, key)).and_then(Node::as_str)
}

/// The concurrency `group` and `cancel-in-progress` of `doc`.
fn concurrency(doc: &Node) -> (Option<&str>, Option<&str>) {
    let c = entry(doc, "concurrency");
    (text_of(c, "group"), text_of(c, "cancel-in-progress"))
}

/// One line per way the workflow `text` of the file named `file` that tags or publishes
/// ([`publishes`]) runs on a pull-request trigger or is outside the release group without being
/// exempt. A workflow that neither tags nor publishes has none. Every workflow of the repository
/// is held to this beside [`super::lint::lint`].
pub fn publish_problems(file: &str, text: &str) -> Vec<String> {
    let Ok(doc) = yaml::load(text) else {
        return Vec::new(); // the base lint reports a document it cannot read
    };
    if !publishes(text) {
        return Vec::new();
    }
    let mut problems = Vec::new();
    for t in trigger_names(&doc)
        .iter()
        .filter(|t| runs_on_pull_requests(t))
    {
        problems.push(format!(
            "{file} tags or publishes, so it may not run on {t}"
        ));
    }
    if !RELEASE_EXEMPT.iter().any(|(name, _)| *name == file) {
        let (group, cancel) = concurrency(&doc);
        if group != Some(RELEASE_GROUP) {
            problems.push(format!(
                "{file} tags or publishes, so its concurrency group must be {RELEASE_GROUP}"
            ));
        }
        if cancel != Some("false") {
            problems.push(format!(
                "{file} tags or publishes, so its concurrency must not cancel (cancel-in-progress: false)"
            ));
        }
    }
    problems
}

/// One line per rule of its own that `release.yml` or `artifacts.yml` breaks, for the file named
/// `file` with the text `text`; none for any other file.
pub fn release_rules(file: &str, text: &str) -> Vec<String> {
    let Ok(doc) = yaml::load(text) else {
        return Vec::new(); // the base lint reports a document it cannot read
    };
    match file {
        "release.yml" => release_problems(&doc),
        "artifacts.yml" => artifacts_problems(&doc),
        _ => Vec::new(),
    }
}

/// What release.yml must be beyond the shared rules.
fn release_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for t in trigger_names(doc) {
        if !RELEASE_TRIGGERS.contains(&t.as_str()) {
            problems.push(format!(
                "release.yml: trigger {t} is not one of {RELEASE_TRIGGERS:?}"
            ));
        }
    }
    let on = entry(doc, "on");
    if let Some(run) = on.and_then(|o| entry(o, "workflow_run")) {
        for (key, want) in [
            ("workflows", &REQUIRED_WORKFLOWS[..]),
            ("types", &["completed"][..]),
            ("branches", &["main"][..]),
        ] {
            let got = names(entry(run, key));
            if got != want {
                problems.push(format!(
                    "release.yml: workflow_run.{key} is {got:?}, not {want:?}"
                ));
            }
        }
    }
    let mut jobs: Vec<&str> = entry(doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    jobs.sort_unstable();
    if jobs != ["plan", "publish", "verify"] {
        problems.push(format!(
            "release.yml: jobs are {jobs:?}, not plan, verify and publish"
        ));
    }
    for (name, condition, needs) in [
        ("plan", PLAN_IF, &[][..]),
        ("verify", RELEASE_IF, &["plan"][..]),
        ("publish", RELEASE_IF, &["plan", "verify"][..]),
    ] {
        let j = job(doc, name);
        if text_of(j, "if") != Some(condition) {
            problems.push(format!(
                "release.yml: job {name} needs the exact `if: {condition}`"
            ));
        }
        let got = names(j.and_then(|j| entry(j, "needs")));
        if got != needs {
            problems.push(format!(
                "release.yml: job {name} needs {got:?}, not {needs:?}"
            ));
        }
    }
    let verify_runs = step_values(job(doc, "verify"), "run");
    for program in SHIPPED_BINARIES {
        let checks = verify_runs.iter().any(|r| {
            r.contains("cargo xtask release check-binary") && r.contains(&format!("/{program}\""))
        });
        if !checks {
            problems.push(format!(
                "release.yml: job verify runs no `cargo xtask release check-binary` of {program}"
            ));
        }
    }
    for (name, j) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        problems.extend(build_problems(name, Some(j)));
    }
    problems
}

/// One line per step of the release.yml job `name` that builds, packages or signs: a `cargo`
/// that is not `cargo xtask --help` or `cargo xtask release <verb>` of [`RELEASE_VERBS`], a word
/// of [`BUILD_WORDS`], or an action that attests or signs. Compiling xtask to run it is not a
/// build of anything released.
fn build_problems(name: &str, job: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    for step in steps(job) {
        if let Some(uses) = entry(step, "uses").and_then(Node::as_str) {
            let lower = uses.to_ascii_lowercase();
            if lower.contains("attest") || lower.contains("sigstore") || lower.contains("cosign") {
                problems.push(format!("release.yml: job {name} signs (`{uses}`)"));
            }
        }
        let Some(run) = entry(step, "run").and_then(Node::as_str) else {
            continue;
        };
        for line in run.lines() {
            if builds(line) {
                problems.push(format!(
                    "release.yml: job {name} builds (`{}`)",
                    line.trim()
                ));
            }
        }
    }
    problems
}

/// Whether the shell line `line` builds, packages or signs (see [`build_problems`]).
fn builds(line: &str) -> bool {
    let words: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || matches!(c, '|' | ';' | '&' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().any(|(at, word)| {
        let after = |n: usize| words.get(at + n).copied().unwrap_or_default();
        match *word {
            "cargo" => match (after(1), after(2)) {
                ("xtask", "--help") => false,
                ("xtask", "release") => !RELEASE_VERBS.contains(&after(3)),
                _ => true,
            },
            other => BUILD_WORDS.contains(&other) && !(other == "tar" && after(1) == "-xzf"),
        }
    })
}

/// What artifacts.yml must be beyond the shared rules.
fn artifacts_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for t in trigger_names(doc) {
        if !ARTIFACTS_TRIGGERS.contains(&t.as_str()) {
            problems.push(format!(
                "artifacts.yml: trigger {t} is not one of {ARTIFACTS_TRIGGERS:?}"
            ));
        }
    }
    let push = entry(doc, "on").and_then(|o| entry(o, "push"));
    if let Some(push) = push {
        let branches = names(entry(push, "branches"));
        if branches != ["main"] || entries(push).len() != 1 {
            problems
                .push("artifacts.yml: push must be `branches: [main]` and nothing else".to_owned());
        }
    }
    let (group, cancel) = concurrency(doc);
    if group != Some(ARTIFACTS_GROUP) || cancel != Some("false") {
        problems.push(format!(
            "artifacts.yml: concurrency must be group {ARTIFACTS_GROUP}, cancel-in-progress false"
        ));
    }
    let version = job(doc, "version");
    let computes = step_values(version, "run").iter().any(|r| {
        r.contains("cargo xtask release version")
            && r.contains("--commit \"$GITHUB_SHA\"")
            && r.contains("--version-file VERSION")
    });
    let whole = steps(version).iter().any(|s| {
        text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/checkout@"))
            && text_of(entry(s, "with"), "fetch-depth") == Some("0")
    });
    if !computes || !whole {
        problems.push("artifacts.yml: job version must check out the whole history (`fetch-depth: 0`) and run `cargo xtask release version --commit \"$GITHUB_SHA\" --version-file VERSION`".to_owned());
    }
    let image_runs = step_values(job(doc, "image"), "run");
    let builds: Vec<&&str> = image_runs
        .iter()
        .filter(|r| r.starts_with(IMAGE_BUILD))
        .collect();
    let fresh = builds.iter().all(|r| r.contains(" --no-cache "));
    let compares = image_runs.iter().any(|r| r.starts_with(IMAGE_COMPARE));
    if builds.len() != 2 || !fresh || !compares {
        problems.push(format!(
            "artifacts.yml: job image must build the image twice (`{IMAGE_BUILD}...` with \
             `--no-cache`, two steps) and run `{IMAGE_COMPARE}`"
        ));
    }
    if !image_runs.contains(&IMAGE_UI_CHECK) {
        problems.push(format!(
            "artifacts.yml: job image must check that the image holds the web app (`{IMAGE_UI_CHECK}`)"
        ));
    }
    let runs_image = image_runs.iter().any(|r| {
        r.starts_with(IMAGE_RUN)
            && r.contains(
                " --version)\" = \"hostkeeper-coordinator $RELEASE_VERSION+${GITHUB_SHA:0:12}\"",
            )
    });
    if !runs_image {
        problems.push(format!(
            "artifacts.yml: job image must run the image through its entrypoint with --version \
             (`{IMAGE_RUN}...`) and compare it with the coordinator's stamp"
        ));
    }
    let attest = job(doc, "attest");
    if text_of(attest, "if") != Some(SIGN_IF) {
        problems.push(format!(
            "artifacts.yml: job attest needs the exact `if: {SIGN_IF}`"
        ));
    }
    for uses in step_values(attest, "uses") {
        if uses.starts_with("actions/checkout@") {
            problems.push("artifacts.yml: job attest checks out the repository".to_owned());
        }
    }
    for run in step_values(attest, "run") {
        if !attest_may_run(run) {
            problems.push(format!("artifacts.yml: job attest runs `{run}`"));
        }
    }
    problems.extend(package_problems(attest));
    problems
}

/// One line per package of [`PACKAGES`], on each of [`PLATFORMS`], that the attest job does not
/// attest (a provenance step whose `subject-path` is the file) or whose bundle it does not copy
/// to `<file>.sigstore.json` from the outputs of that same step.
fn package_problems(attest: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    for (prefix, middle) in PACKAGES {
        for platform in PLATFORMS {
            let subject = format!(
                "dist/{prefix}-${{{{ needs.version.outputs.version }}}}{middle}-{platform}.tar.zst"
            );
            let file = format!("{prefix}-<version>{middle}-{platform}.tar.zst");
            let id = steps(attest)
                .iter()
                .find(|s| {
                    text_of(Some(s), "uses")
                        .is_some_and(|u| u.starts_with("actions/attest-build-provenance@"))
                        && text_of(entry(s, "with"), "subject-path") == Some(subject.as_str())
                })
                .and_then(|s| text_of(Some(s), "id"));
            let Some(id) = id else {
                problems.push(format!(
                    "artifacts.yml: job attest does not attest {file} (a provenance step with an id and the subject-path {subject})"
                ));
                continue;
            };
            let bundle = format!("steps.{id}.outputs.bundle-path");
            let copy = format!(
                "cp \"$BUNDLE\" \"dist/{prefix}-${{V}}{middle}-{platform}.tar.zst.sigstore.json\""
            );
            let copied = steps(attest).iter().any(|s| {
                text_of(Some(s), "run") == Some(copy.as_str())
                    && text_of(entry(s, "env"), "BUNDLE").is_some_and(|b| {
                        b.trim_start_matches("${{ ").trim_end_matches(" }}") == bundle
                    })
            });
            if !copied {
                problems.push(format!(
                    "artifacts.yml: job attest does not copy the bundle of {file} from the step {id} to dist/{prefix}-${{V}}{middle}-{platform}.tar.zst.sigstore.json"
                ));
            }
        }
    }
    problems
}
