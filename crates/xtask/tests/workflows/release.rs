//! The rules of the release path, on top of the rules every workflow is held to ([`super::lint`]).
//!
//! - A workflow that tags or publishes ([`publishes`]) runs on no pull-request trigger, and it is
//!   in the one release concurrency group ([`RELEASE_GROUP`], never cancelled) unless
//!   [`RELEASE_EXEMPT`] lists it with a reason ([`publish_problems`], run over every workflow of
//!   the repository).
//! - `release.yml` ([`release_problems`]) starts only by [`RELEASE_TRIGGERS`] (a `workflow_run`
//!   of exactly `ci`, `privileged` and `artifacts`, completed, on main), has exactly the jobs
//!   of [`RELEASE_JOBS`]: `plan`, `verify` and `publish` under their exact `if:`, and the two jobs
//!   of hostkeeper's own deploy, held to [`desired::problems`]; plans with the one step of
//!   [`PLAN_RUN`], under no `if:`, on a checkout of the tip, for every trigger; publishes in the
//!   job-level group of [`PUBLISH_GROUP`], never cancelled; checks every shipped binary
//!   ([`SHIPPED_BINARIES`]) in `verify`, and
//!   has no step that builds or signs ([`build_problems`]), but the jobs of the package channels
//!   ([`super::packages`]), which run `cargo xtask channels` (signing the indexes of the apt and
//!   rpm repositories, never a release file) and, in the two install checks, one `docker` command
//!   each.
//! - `artifacts.yml` ([`artifacts_problems`]) starts only by a push to main or by hand, has its
//!   own group per commit ([`ARTIFACTS_GROUP`]), computes the version on a whole history, builds
//!   the coordinator's image twice, compares the two, checks that the image holds the operator
//!   web app and runs the image's entrypoint ([`IMAGE_BUILD`], [`IMAGE_COMPARE`],
//!   [`IMAGE_UI_CHECK`], [`IMAGE_RUN`]), and its `attest` job, under its exact
//!   `if:`, runs no repository code: no checkout, and no `run:` but the checks of
//!   [`ATTEST_RUNS`] and the bundle copies of [`BUNDLE_COPY`]. It attests every package of
//!   [`PACKAGES`] for every platform of [`PLATFORMS`], each binary tarball of [`TARBALLS`], each
//!   file of [`ATTESTED_FILES`] and each versioned file of [`VERSIONED_FILES`], and copies the
//!   bundle of each beside it
//!   ([`package_problems`]), and attests the SBOM of each binary tarball ([`sbom_problem`]): a
//!   host trusts only attested packages, so a release asset list that names a bundle needs the
//!   steps that make it. Its build job makes the mdm package with the steps of [`MDM_STEPS`],
//!   each under [`MDM_IF`], and its sums job writes and uploads the mdm index ([`MDM_INDEX`],
//!   [`mdm_problems`]).
//!
//! Which job may hold which write scope, under which `if:`, is [`super::lint::WRITE_ALLOWED`].

use super::desired;
use super::lint::{entries, entry, pinned_by_digest, runs_on_pull_requests, triggers};
use super::packages;
use super::pkg_signing;
use super::yaml::{self, Node, Value};

/// The `if:` of the attest job: a push to main. The only condition under which a token can sign.
pub const SIGN_IF: &str = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}";

/// The `if:` of the plan job: a run of main, by hand, on the schedule, or the completion of a
/// push run on main of this repository.
pub const PLAN_IF: &str = "${{ github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' || github.event_name == 'schedule' || (github.event_name == 'workflow_run' && github.event.workflow_run.event == 'push' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository)) }}";

/// The `if:` of the jobs after the plan: the plan said to release, on a run of main.
pub const RELEASE_IF: &str =
    "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' }}";

/// The group every workflow that tags or publishes is in: a dispatch or schedule run of main
/// joins `release-main-dispatch`, so the completion of a push run never replaces it while it
/// waits; the completion of a push run on main of this repository joins `release-main`; any other
/// run takes a group of its own, so it cannot replace a release waiting.
pub const RELEASE_GROUP: &str = "release-${{ (github.ref == 'refs/heads/main' && github.event_name != 'workflow_run') && 'main-dispatch' || (github.ref == 'refs/heads/main' && github.event_name == 'workflow_run' && github.event.workflow_run.event == 'push' && github.event.workflow_run.head_branch == 'main' && github.event.workflow_run.head_repository.full_name == github.repository) && 'main' || github.run_id }}";

/// The job-level group of release.yml's `publish`: one per tag, so the two groups of main never
/// publish one tag at once (two drafts of one tag would hold every later publish of it).
pub const PUBLISH_GROUP: &str = "release-publish-${{ matrix.tag }}";

/// The one plan step of release.yml, the same for every trigger: every commit from the tip of
/// main down to the last release is planned, so a run replaced while it waited leaves nothing
/// behind.
pub const PLAN_RUN: &str = "cargo xtask release plan --repo \"$GITHUB_REPOSITORY\" --commit \"$GITHUB_SHA\" --version-file VERSION --freeze \"$FREEZE\" --output \"$GITHUB_OUTPUT\"";

/// The group of artifacts.yml: one per commit.
pub const ARTIFACTS_GROUP: &str = "artifacts-${{ github.sha }}";

/// Workflows outside the release group on purpose, each with its reason.
pub const RELEASE_EXEMPT: &[(&str, &str)] = &[(
    "artifacts.yml",
    "builds and attests one commit and never tags or publishes; its own per-commit group",
)];

/// The jobs of release.yml, sorted, but the package channels ([`packages::job_names`]): the release
/// (`plan`, `verify`, `publish`, `image`) and hostkeeper's own deploy ([`desired::CHANNEL_JOB`],
/// [`desired::JOB`]).
pub const RELEASE_JOBS: [&str; 6] = [
    "channel",
    "image",
    "plan",
    "publish",
    "set-desired",
    "verify",
];

/// The triggers release.yml may have.
pub const RELEASE_TRIGGERS: [&str; 3] = ["schedule", "workflow_dispatch", "workflow_run"];

/// The workflows whose completion starts release.yml, sorted: the three a release requires.
pub const REQUIRED_WORKFLOWS: [&str; 3] = ["artifacts", "ci", "privileged"];

/// The binaries a release ships, each of which the verify job of release.yml runs.
pub const SHIPPED_BINARIES: [&str; 3] = ["hostkeeper", "hostkeeper-coordinator", "hostkeeper-mdm"];

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

/// The platforms the release ships packages for.
pub const PLATFORMS: [&str; 3] = ["linux-x86_64", "linux-arm64", "macos-arm64"];

/// The packages the release carries, each as the part of its file name before the version, the
/// part between the version and the platform (`<prefix>-<version><middle>-<platform>.tar.zst`)
/// and the platforms it is built for. They are hostkeeper's own, hello's, and hello's failing
/// build on every platform, and mdm's on linux-x86_64 alone.
pub const PACKAGES: [(&str, &str, &[&str]); 4] = [
    ("hostkeeper", "", &PLATFORMS),
    ("hello", "", &PLATFORMS),
    ("hello", "-fail-ready", &PLATFORMS),
    ("mdm", "", &["linux-x86_64"]),
];

/// The architectures of the two Linux platforms as the packages name them: the deb's, then the
/// rpm's.
pub const LINUX_ARCHES: [(&str, &str); 2] = [("amd64", "x86_64"), ("arm64", "aarch64")];

/// The programs of the Linux packages the attest job signs: the daemon's deb and rpm
/// (packaging/linux/nfpm.yaml) and the coordinator's (packaging/coordinator/nfpm.yaml).
pub const LINUX_PACKAGE_PROGRAMS: [&str; 2] = ["hostkeeper", "hostkeeper-coordinator"];

/// The `if:` of each step of the build job that makes the mdm package: the linux-x86_64 leg of
/// its matrix, the one platform the package is built for.
pub const MDM_IF: &str = "matrix.platform == 'linux-x86_64'";

/// The steps of the build job that make the mdm package, in order, each under [`MDM_IF`]: its
/// program built with the commit, its stamp checked, the pinned archives fetched and checked
/// against their pins, and the package built from them.
pub const MDM_STEPS: [&str; 4] = [
    "cargo build --locked --release -p hostkeeper-mdm --bin mdm",
    "cargo xtask release check-binary target/release/mdm \"$RELEASE_VERSION\" \"$GITHUB_SHA\"",
    "cargo xtask release mdm-fetch --dir \"$RUNNER_TEMP/mdm-archives\"",
    "cargo xtask release package --service mdm --platform \"$PLATFORM\" --version \"$RELEASE_VERSION\" --commit \"$GITHUB_SHA\" --binary target/release/mdm --archives \"$RUNNER_TEMP/mdm-archives\" --out dist",
];

/// The step of the sums job that writes the mdm package's index, and the file its upload of the
/// indexes must list, so the attest job finds every file SHA256SUMS lists.
pub const MDM_INDEX: (&str, &str) = (
    "cargo xtask release index --service mdm --version \"$RELEASE_VERSION\" --dir dist",
    "dist/mdm-index.json",
);

/// The binary tarballs the release carries, each as its program (the stem of its file name,
/// `<program>-<version>-<platform>.tar.gz`) and the platforms it ships for: hostkeeper's on
/// every platform, the coordinator's and the MDM kit's on the two Linux platforms. Each has a
/// bundle and an attested SBOM (`<program>-<version>-<platform>.cdx.json`).
pub const TARBALLS: [(&str, &[&str]); 3] = [
    ("hostkeeper", &PLATFORMS),
    ("hostkeeper-coordinator", &["linux-x86_64", "linux-arm64"]),
    ("hostkeeper-mdm", &["linux-x86_64", "linux-arm64"]),
];

/// The release's other files that the attest job attests alone, each as its `subject-path` and
/// its name: `netboot.json`, the boot sets of network install, whose attestation is what the
/// design's `hostkeeper-pxe fetch` (docs/design/network-install.md, item N8) is to trust its
/// digests by.
pub const ATTESTED_FILES: [(&str, &str); 1] = [("dist/netboot.json", "netboot.json")];

/// The release's other files that the attest job attests alone and whose name carries the
/// version, each as the stem before `-<version>`: `hkctl-<version>`, the command line of the
/// coordinator, one script for every platform.
pub const VERSIONED_FILES: [&str; 1] = ["hkctl"];

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
const RELEASE_VERBS: [&str; 7] = [
    "check",
    "check-binary",
    "image-layout",
    "locate",
    "plan",
    "publish",
    "targets",
];

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
pub fn names(node: Option<&Node>) -> Vec<String> {
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
pub fn steps(job: Option<&Node>) -> &[Node] {
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

pub fn job<'a>(doc: &'a Node, name: &str) -> Option<&'a Node> {
    entry(doc, "jobs").and_then(|jobs| entry(jobs, name))
}

pub fn text_of<'a>(node: Option<&'a Node>, key: &str) -> Option<&'a str> {
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
    let mut want: Vec<String> = RELEASE_JOBS.iter().map(|j| (*j).to_owned()).collect();
    want.extend(packages::job_names());
    want.sort();
    if jobs != want {
        problems.push(format!(
            "release.yml: jobs are {jobs:?}, not plan, verify, publish, image, channel, set-desired and the jobs of the package channels ({want:?})"
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
    problems.extend(plan_problems(job(doc, "plan")));
    let publish = job(doc, "publish").and_then(|j| entry(j, "concurrency"));
    if (
        text_of(publish, "group"),
        text_of(publish, "cancel-in-progress"),
    ) != (Some(PUBLISH_GROUP), Some("false"))
    {
        problems.push(format!(
            "release.yml: job publish needs `concurrency:` group {PUBLISH_GROUP}, cancel-in-progress false"
        ));
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
    problems.extend(desired::problems(doc));
    problems.extend(packages::problems(doc));
    problems.extend(packages::promote_check_problems(doc));
    if let Some(check) = job(doc, "check-rpm") {
        problems.extend(packages::matrix_image_problems(
            "check-rpm",
            check,
            pinned_by_digest,
        ));
    }
    problems
}

/// One line per way the plan job `plan` of release.yml plans other than with the one step of
/// [`PLAN_RUN`], under no `if:`, on a checkout of the tip (no `ref:`): a second plan step, or one
/// that a trigger skips or points elsewhere, would plan differently for some trigger.
fn plan_problems(plan: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    let planning: Vec<&Node> = steps(plan)
        .iter()
        .filter(|s| text_of(Some(s), "run").is_some_and(|r| r.contains("xtask release plan")))
        .collect();
    match planning[..] {
        [step] if text_of(Some(step), "run") == Some(PLAN_RUN) => {
            if entry(step, "if").is_some() {
                problems.push(
                    "release.yml: the plan step must run for every trigger, under no `if:`"
                        .to_owned(),
                );
            }
        }
        _ => problems.push(format!(
            "release.yml: job plan needs exactly one plan step, `{PLAN_RUN}`"
        )),
    }
    let checkouts_elsewhere = steps(plan).iter().any(|s| {
        text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/checkout@"))
            && entry(s, "with").and_then(|w| entry(w, "ref")).is_some()
    });
    if checkouts_elsewhere {
        problems
            .push("release.yml: job plan must check out the tip of main (no `ref:`)".to_owned());
    }
    problems
}

/// One line per step of the release.yml job `name` that builds, packages or signs: a `cargo`
/// that is not `cargo xtask --help` or `cargo xtask release <verb>` of [`RELEASE_VERBS`] (nor, in
/// a job of the package channels, `cargo xtask channels`), a word of [`BUILD_WORDS`] (but the one
/// `docker` command of an install check, [`packages::docker_line_ok`]), or an action that attests
/// or signs. Compiling xtask to run it is not a
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
        let channels_ok = packages::job_names().iter().any(|j| j == name);
        for line in run.lines() {
            if packages::docker_line_ok(name, line.trim()) {
                continue;
            }
            if builds(line, channels_ok) {
                problems.push(format!(
                    "release.yml: job {name} builds (`{}`)",
                    line.trim()
                ));
            }
        }
    }
    problems
}

/// Whether the shell line `line` builds, packages or signs (see [`build_problems`]). A job of the
/// package channels (`channels_ok`) may run `cargo xtask channels`, which is held to its row.
fn builds(line: &str, channels_ok: bool) -> bool {
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
                ("xtask", "channels") => !channels_ok,
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
    problems.extend(mdm_problems(doc));
    problems.extend(pkg_signing::problems(doc));
    problems
}

/// What is wrong with the making of the mdm package: a step of [`MDM_STEPS`] missing from the
/// build job, out of order, or under another `if:` than [`MDM_IF`] (the program built without
/// the commit, a package built from archives no step fetched, or a step on a platform the
/// package is not built for); or the sums job not writing its index ([`MDM_INDEX`]) or not
/// uploading it with the other indexes.
fn mdm_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let build = steps(job(doc, "build"));
    let mut at = 0;
    for want in MDM_STEPS {
        let found = build[at..]
            .iter()
            .position(|s| text_of(Some(s), "run") == Some(want));
        let Some(n) = found else {
            problems.push(format!(
                "artifacts.yml: job build does not run `{want}` (after the mdm steps before it)"
            ));
            continue;
        };
        let step = &build[at + n];
        at += n + 1;
        if text_of(Some(step), "if") != Some(MDM_IF) {
            problems.push(format!(
                "artifacts.yml: job build runs `{want}` without the exact `if: {MDM_IF}`"
            ));
        }
        let stamped = text_of(entry(step, "env"), "RELEASE_COMMIT") == Some("${{ github.sha }}");
        if want.starts_with("cargo build") && !stamped {
            problems.push(format!(
                "artifacts.yml: job build runs `{want}` without RELEASE_COMMIT: ${{{{ github.sha }}}}"
            ));
        }
    }
    let sums = job(doc, "sums");
    let (index, uploaded) = MDM_INDEX;
    if !step_values(sums, "run").contains(&index) {
        problems.push(format!("artifacts.yml: job sums does not run `{index}`"));
    }
    let uploads = steps(sums).iter().any(|s| {
        text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/upload-artifact@"))
            && text_of(entry(s, "with"), "path")
                .is_some_and(|p| p.lines().any(|l| l.trim() == uploaded))
    });
    if !uploads {
        problems.push(format!(
            "artifacts.yml: job sums does not upload {uploaded} with the other indexes"
        ));
    }
    problems
}

/// One line per package of [`PACKAGES`], on each of [`PLATFORMS`], per Linux package (the deb
/// and the rpm) of each of [`LINUX_ARCHES`], per binary tarball of
/// [`TARBALLS`], per file of [`ATTESTED_FILES`] and per file of [`VERSIONED_FILES`], that the
/// attest job does not attest (a
/// provenance step whose `subject-path` is the file) or whose bundle it does not copy to
/// `<file>.sigstore.json` from the outputs of that same step; and one per binary tarball whose
/// SBOM it does not attest ([`sbom_problem`]).
fn package_problems(attest: Option<&Node>) -> Vec<String> {
    let mut problems = Vec::new();
    for (prefix, middle, platforms) in PACKAGES {
        for platform in platforms {
            let subject = format!(
                "dist/{prefix}-${{{{ needs.version.outputs.version }}}}{middle}-{platform}.tar.zst"
            );
            let file = format!("{prefix}-<version>{middle}-{platform}.tar.zst");
            let copied = format!("{prefix}-${{V}}{middle}-{platform}.tar.zst");
            problems.extend(attest_problem(attest, &subject, &file, &copied));
        }
    }
    let version = "${{ needs.version.outputs.version }}";
    for ((deb, rpm), program) in LINUX_ARCHES
        .into_iter()
        .flat_map(|arches| LINUX_PACKAGE_PROGRAMS.map(|program| (arches, program)))
    {
        let linux_packages = [
            (
                format!("{program}_{version}_{deb}.deb"),
                format!("{program}_<version>_{deb}.deb"),
                format!("{program}_${{V}}_{deb}.deb"),
            ),
            (
                format!("{program}-{version}-1.{rpm}.rpm"),
                format!("{program}-<version>-1.{rpm}.rpm"),
                format!("{program}-${{V}}-1.{rpm}.rpm"),
            ),
        ];
        for (subject, file, copied) in linux_packages {
            let subject = format!("dist/{subject}");
            problems.extend(attest_problem(attest, &subject, &file, &copied));
        }
    }
    problems.extend(attest_problem(
        attest,
        &format!("dist/hostkeeper-coordinator-{version}.image.json"),
        "hostkeeper-coordinator-<version>.image.json",
        "hostkeeper-coordinator-${V}.image.json",
    ));
    for (program, platforms) in TARBALLS {
        for platform in platforms {
            let stem = format!("{program}-${{{{ needs.version.outputs.version }}}}-{platform}");
            let subject = format!("dist/{stem}.tar.gz");
            let file = format!("{program}-<version>-{platform}.tar.gz");
            let copied = format!("{program}-${{V}}-{platform}.tar.gz");
            problems.extend(attest_problem(attest, &subject, &file, &copied));
            let sbom = format!("dist/{stem}.cdx.json");
            problems.extend(sbom_problem(attest, &subject, &sbom, &file));
        }
    }
    for (subject, file) in ATTESTED_FILES {
        problems.extend(attest_problem(attest, subject, file, file));
    }
    for stem in VERSIONED_FILES {
        let subject = format!("dist/{stem}-${{{{ needs.version.outputs.version }}}}");
        let file = format!("{stem}-<version>");
        let copied = format!("{stem}-${{V}}");
        problems.extend(attest_problem(attest, &subject, &file, &copied));
    }
    problems
}

/// What is wrong, if anything, with the SBOM attestation of the binary tarball `file`: no
/// `attest-sbom` step whose `subject-path` is `subject` and whose `sbom-path` is `sbom`.
fn sbom_problem(attest: Option<&Node>, subject: &str, sbom: &str, file: &str) -> Option<String> {
    let found = steps(attest).iter().any(|s| {
        let with = entry(s, "with");
        text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/attest-sbom@"))
            && text_of(with, "subject-path") == Some(subject)
            && text_of(with, "sbom-path") == Some(sbom)
    });
    (!found).then(|| {
        format!(
            "artifacts.yml: job attest does not attest the SBOM of {file} (an attest-sbom step with the subject-path {subject} and the sbom-path {sbom})"
        )
    })
}

/// What is wrong, if anything, with the attestation of `file` in the attest job: no provenance
/// step with an id whose `subject-path` is `subject`, or no copy of that step's bundle to
/// `dist/<copied>.sigstore.json` (`copied` the file's name as the copy spells it).
fn attest_problem(
    attest: Option<&Node>,
    subject: &str,
    file: &str,
    copied: &str,
) -> Option<String> {
    let id = steps(attest)
        .iter()
        .find(|s| {
            text_of(Some(s), "uses")
                .is_some_and(|u| u.starts_with("actions/attest-build-provenance@"))
                && text_of(entry(s, "with"), "subject-path") == Some(subject)
        })
        .and_then(|s| text_of(Some(s), "id"));
    let Some(id) = id else {
        return Some(format!(
            "artifacts.yml: job attest does not attest {file} (a provenance step with an id and the subject-path {subject})"
        ));
    };
    let bundle = format!("steps.{id}.outputs.bundle-path");
    let copy = format!("cp \"$BUNDLE\" \"dist/{copied}.sigstore.json\"");
    let done = steps(attest).iter().any(|s| {
        text_of(Some(s), "run") == Some(copy.as_str())
            && text_of(entry(s, "env"), "BUNDLE")
                .is_some_and(|b| b.trim_start_matches("${{ ").trim_end_matches(" }}") == bundle)
    });
    (!done).then(|| {
        format!(
            "artifacts.yml: job attest does not copy the bundle of {file} from the step {id} to dist/{copied}.sigstore.json"
        )
    })
}
