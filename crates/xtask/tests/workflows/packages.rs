//! The rules of the package-channel jobs of `release.yml` (docs/deploy/package-repositories.md):
//! the jobs after `publish` and `image` that put the release's deb and rpm packages into the apt and
//! rpm repositories and its macOS installer into the pool directory, `testing` first, check them
//! from there, and make the same bytes `stable` once [`STAGES`] all passed.
//!
//! Every job has a row ([`rows`]): its `needs`, its exact `if:`, its environment, its exact
//! permissions, and the `cargo xtask channels` verbs it runs. [`problems`] refuses `release.yml`
//! when a job lacks its row or differs from it, and so:
//! - the signing key (environment `apt-signing`, the only place the key secrets may be named) and
//!   the cloud role (environment `apt-publish`, `id-token: write`, `channels assume`) are never in
//!   one job: no row has both;
//! - `id-token: write` is held by exactly the jobs of the rows that assume the role, each under its
//!   exact `if:` ([`id_token_condition`], used by the shared permission rule);
//! - a job that writes starts with its gate (`channels gate --label <job>`) after the checkout and
//!   the build of xtask, and every step after the gate runs only when the gate is open, so a
//!   repository without the settings gets a green job that did nothing;
//! - every job runs the xtask of the newest release's commit, the files of that release's own
//!   artifact, and a `--session-name` that starts with the product's prefix;
//! - the last attempt of a write passes `--last` (the third of `testing` and `stable`; the only one
//!   of `testing-fail`, hello's failing build, which no stage of `promote-check` waits for), the
//!   writes of `stable` pass `--no-pool`, and a job names only the suite its row says;
//! - `docker` is run only by the two install checks, with an image pinned by digest, in the
//!   one command each is allowed ([`docker_line_ok`]);
//! - `promote-check` gives `channels decide` every stage of [`STAGES`], each the result of its own
//!   job;
//! - a job that signs or publishes takes the release's files only to check them first: in every
//!   gated job that downloads the release's artifact, the two steps right after the download
//!   check the files against `SHA256SUMS` ([`SUMS_CHECK`]) and against their attestations by this
//!   repository's workflow at the release's commit ([`ATTESTATION_CHECK`], with the job's token),
//!   and no `channels` verb but the gate runs before them ([`release_files_problems`]). So the
//!   bytes the repository's key signs and the store serves, hostkeeper's own `.deb` and
//!   installer among them, are the ones the release attested (docs/design/packages-native.md
//!   section 10.3).

use super::lint::{entries, entry, scalars};
use super::release::{names, steps, text_of};
use super::xtask_calls::{commands, xtask_args};
use super::yaml::{Node, Value};

/// The workflow the jobs are in.
pub const FILE: &str = "release.yml";

/// The `if:` of the jobs that run for a release of main.
const P: &str = "needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main'";

/// The `if:` of a step after the gate: the gate found the job's settings.
pub const GATE_IF: &str = "${{ steps.gate.outputs.enabled == 'true' }}";

/// The job that names the newest release of the run.
pub const NEWEST_JOB: &str = "newest";

/// The one step of [`NEWEST_JOB`]: the last entry of the plan's matrix, which is oldest first.
pub const NEWEST_RUN: &str = "jq --raw-output '.include[-1] | \"version=\\(.version)\", \"tag=\\(.tag)\", \"commit=\\(.commit)\"' <<< \"$MATRIX\" >> \"$GITHUB_OUTPUT\"";

/// The commit and the tag every package job works on: the newest release of the run.
pub const NEWEST_COMMIT: &str = "${{ needs.newest.outputs.commit }}";
pub const NEWEST_TAG: &str = "${{ needs.newest.outputs.tag }}";

/// The artifact that holds the release's files (`verify` uploaded it).
pub const RELEASE_ARTIFACT: &str = "release-${{ needs.newest.outputs.tag }}";

/// The directory a job downloads the release's files into.
pub const RELEASE_DIR: &str = "dist";

/// The step right after the download of the release's files, run in [`RELEASE_DIR`]: every file
/// is the one `SHA256SUMS` lists.
pub const SUMS_CHECK: &str = "sha256sum --check --strict SHA256SUMS";

/// The step after [`SUMS_CHECK`]: every asset is attested by this repository's workflow at the
/// release's commit, online and by its bundle (`cargo xtask release check`), with the job's
/// token as [`ATTESTATION_TOKEN`].
pub const ATTESTATION_CHECK: &str =
    "cargo xtask release check dist --version \"${TAG#v}\" --commit \"$COMMIT\"";

/// The `GH_TOKEN` of the [`ATTESTATION_CHECK`] step.
pub const ATTESTATION_TOKEN: &str = "${{ github.token }}";

/// The value of `--token-url` of every `channels assume` step: the token request the runner
/// offers a job with `id-token: write`, passed as a flag and not read from the environment.
pub const TOKEN_URL: &str = "$ACTIONS_ID_TOKEN_REQUEST_URL";

/// The prefix of the name of every session the role is assumed under (`spec.rs` of the verbs).
pub const SESSION_PREFIX: &str = "hostkeeper-";

/// The Ubuntu image of the apt install check.
pub const UBUNTU_IMAGE: &str =
    "ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852";

/// The stages `promote-check` gives to `channels decide`: the jobs whose results decide the
/// promotion. The verb's own list is `Product::stages` (`crates/xtask/src/channels/spec.rs`), and
/// a test of the verb reads this workflow against it.
pub const STAGES: [&str; 10] = [
    "testing-apt-write",
    "testing-apt-write-2",
    "testing-apt-write-3",
    "testing-rpm-write",
    "testing-rpm-write-2",
    "testing-rpm-write-3",
    "testing-pkg-upload",
    "check-apt",
    "check-rpm",
    "check-pkg",
];

/// The environment that holds the cloud role and no key.
pub const PUBLISH_ENV: &str = "apt-publish";

/// The environment that holds the signing key and no role.
pub const SIGNING_ENV: &str = "apt-signing";

/// The secrets of the signing environment: the only secrets any of these jobs names.
pub const KEY_SECRETS: [&str; 2] = ["APT_GPG_PRIVATE_KEY", "APT_GPG_PASSPHRASE"];

/// The variable that holds the role: named by the jobs that assume it, and by the gate that checks
/// it is set.
pub const ROLE_VAR: &str = "APT_PUBLISH_ROLE_ARN";

/// What a job of the package channels must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub needs: Vec<String>,
    pub condition: String,
    pub environment: Option<&'static str>,
    /// Scope and level, sorted.
    pub permissions: Vec<(&'static str, &'static str)>,
    /// The `cargo xtask channels` verbs it runs, one entry per call, sorted.
    pub verbs: Vec<String>,
    /// Whether it starts with its gate.
    pub gated: bool,
    /// For a write: its attempt, the last attempt of its family (3, or 1 for `testing-fail`),
    /// and whether it writes the pool.
    pub write: Option<(usize, usize, bool)>,
    /// The `--suite` values it passes, sorted and without duplicates.
    pub suites: Vec<&'static str>,
}

fn strings(items: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|s| (*s).to_owned()).collect();
    v.sort();
    v
}

fn suffix(attempt: usize) -> String {
    if attempt == 1 {
        String::new()
    } else {
        format!("-{attempt}")
    }
}

fn if_of(parts: &[String]) -> String {
    format!("${{{{ {} }}}}", parts.join(" && "))
}

/// The row of attempt `attempt` of `step` (`read`, `index`, `write`) of `kind` (`apt`, `rpm`) for
/// `suite` (`testing`, `stable`, or `testing-fail`, which is apt alone and has the one attempt).
fn attempt_row(suite: &'static str, kind: &str, step: &str, attempt: usize) -> Row {
    let stable = suite == "stable";
    let job = |s: &str, n: usize| format!("{suite}-{kind}-{s}{}", suffix(n));
    let name = job(step, attempt);
    let prev_write = job("write", attempt.max(2) - 1);
    let (read, index) = (job("read", attempt), job("index", attempt));
    let mut needs = vec!["plan".to_owned(), "newest".to_owned()];
    if attempt == 1 {
        if stable {
            needs.push("promote-check".to_owned());
        }
        match step {
            "read" if stable => needs.push(format!("testing-{kind}-read")),
            "read" => {}
            "index" if stable => {
                needs.extend([read.clone(), format!("testing-{kind}-index")]);
            }
            "index" => needs.push(read.clone()),
            _ if stable => {
                needs.extend([read.clone(), index.clone(), format!("testing-{kind}-write")])
            }
            _ => needs.extend([read.clone(), index.clone()]),
        }
    } else {
        match step {
            "read" => needs.push(prev_write.clone()),
            "index" => needs.push(read.clone()),
            _ => needs.extend([read.clone(), index.clone()]),
        }
    }
    needs.sort();
    let mut parts = Vec::new();
    let first = attempt == 1;
    // A later attempt's jobs run after skipped ones, so they say it: `!failure()` lets a skipped
    // job through, and the explicit results stop that.
    if stable || (!first && step != "read") || attempt == 3 {
        parts.extend(["!cancelled()".to_owned(), "!failure()".to_owned()]);
    }
    parts.push(P.to_owned());
    match (first, step) {
        (true, _) if stable => {
            parts.push("needs.promote-check.outputs.promote == 'true'".to_owned())
        }
        (true, _) => {}
        (false, "read") => {
            parts.push(format!("needs.{prev_write}.outputs.conflict == 'true'"));
        }
        (false, "index") => parts.push(format!("needs.{read}.result == 'success'")),
        (false, _) => parts.extend([
            format!("needs.{read}.result == 'success'"),
            format!("needs.{index}.result == 'success'"),
        ]),
    }
    let role = step != "index";
    let mut permissions = vec![("contents", "read")];
    if step == "index" || step == "write" && !stable {
        permissions.push(("attestations", "read"));
    }
    if role {
        permissions.push(("id-token", "write"));
    }
    permissions.sort_unstable();
    let kind_verb = |v: &str| {
        if kind == "apt" {
            format!("apt {v}")
        } else if v == "check-signed" || v == "sign" || v == "promote" {
            format!("rpm {v}")
        } else {
            format!("rpm repo {v}")
        }
    };
    let mut verbs = vec!["gate".to_owned()];
    match step {
        "read" => {
            verbs.extend(["assume".to_owned(), kind_verb("read")]);
            if stable {
                verbs.push(kind_verb("read"));
            }
        }
        "index" => {
            let promote = if stable { "promote" } else { "index" };
            if kind == "rpm" {
                verbs.push("rpm sign".to_owned());
            }
            verbs.push(if kind == "rpm" && !stable {
                "rpm repo index".to_owned()
            } else {
                kind_verb(promote)
            });
            verbs.push(if kind == "apt" {
                "apt sign".to_owned()
            } else {
                "rpm repo sign".to_owned()
            });
        }
        _ => {
            verbs.extend(["assume".to_owned(), kind_verb("write")]);
            if kind == "rpm" && !stable {
                verbs.push("rpm check-signed".to_owned());
            }
        }
    }
    verbs.sort();
    let suites = match (step, stable) {
        ("read", true) => vec!["stable", "testing"],
        ("read" | "index" | "write", false) => vec![suite],
        ("write", true) => vec!["stable"],
        // The promotion of the index takes no suite: it reads the two directories it is given.
        _ => vec![],
    };
    Row {
        name,
        needs,
        condition: if_of(&parts),
        environment: Some(if step == "index" {
            SIGNING_ENV
        } else {
            PUBLISH_ENV
        }),
        permissions,
        verbs,
        gated: true,
        write: (step == "write").then_some((
            attempt,
            if suite == "testing-fail" { 1 } else { 3 },
            !stable,
        )),
        suites,
    }
}

fn plain(name: &str, needs: &[&str], condition: String, verbs: &[&str]) -> Row {
    Row {
        name: name.to_owned(),
        needs: strings(needs),
        condition,
        environment: None,
        permissions: vec![("contents", "read")],
        verbs: strings(verbs),
        gated: false,
        write: None,
        suites: vec![],
    }
}

/// Every job of the package channels, with `newest`.
pub fn rows() -> Vec<Row> {
    let mut rows = vec![plain(
        NEWEST_JOB,
        &["image", "plan", "publish"],
        format!("${{{{ {P} }}}}"),
        &[],
    )];
    for kind in ["apt", "rpm"] {
        for step in ["read", "index", "write"] {
            for attempt in 1..=3 {
                rows.push(attempt_row("testing", kind, step, attempt));
            }
        }
    }
    // Hello's failing build: the three jobs of one attempt, which no stage waits for.
    for step in ["read", "index", "write"] {
        rows.push(attempt_row("testing-fail", "apt", step, 1));
    }
    let mut upload = plain(
        "testing-pkg-upload",
        &["newest", "plan"],
        format!("${{{{ {P} }}}}"),
        &["assume", "gate", "pkg upload"],
    );
    upload.environment = Some(PUBLISH_ENV);
    upload.permissions = vec![
        ("attestations", "read"),
        ("contents", "read"),
        ("id-token", "write"),
    ];
    upload.gated = true;
    rows.push(upload);
    let written = |kind: &str| {
        ["", "-2", "-3"]
            .iter()
            .map(|s| format!("needs.testing-{kind}-write{s}.outputs.conflict == 'false'"))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    for (kind, verb) in [("apt", "check-apt"), ("rpm", "check-rpm")] {
        let needs = ["newest", "plan"]
            .into_iter()
            .map(str::to_owned)
            .chain(["", "-2", "-3"].map(|s| format!("testing-{kind}-write{s}")))
            .collect::<Vec<_>>();
        let needs: Vec<&str> = needs.iter().map(String::as_str).collect();
        rows.push(plain(
            verb,
            &needs,
            format!(
                "${{{{ !cancelled() && !failure() && {P} && ({}) }}}}",
                written(kind)
            ),
            &[verb],
        ));
    }
    rows.push(plain(
        "check-pkg",
        &["newest", "plan", "testing-pkg-upload"],
        format!(
            "${{{{ !cancelled() && !failure() && {P} && needs.testing-pkg-upload.outputs.enabled == 'true' }}}}"
        ),
        &["pkg check"],
    ));
    let mut needs = vec!["newest", "plan"];
    needs.extend(STAGES);
    rows.push(plain(
        "promote-check",
        &needs,
        format!("${{{{ !cancelled() && !failure() && {P} }}}}"),
        &["decide"],
    ));
    for kind in ["apt", "rpm"] {
        for step in ["read", "index", "write"] {
            for attempt in 1..=3 {
                rows.push(attempt_row("stable", kind, step, attempt));
            }
        }
    }
    let mut promote = plain(
        "promote-pkg",
        &["newest", "plan", "promote-check", "testing-pkg-upload"],
        format!(
            "${{{{ !cancelled() && !failure() && {P} && needs.promote-check.outputs.promote == 'true' }}}}"
        ),
        &["assume", "gate", "pkg promote"],
    );
    promote.environment = Some(PUBLISH_ENV);
    promote.permissions = vec![
        ("attestations", "read"),
        ("contents", "read"),
        ("id-token", "write"),
    ];
    promote.gated = true;
    rows.push(promote);
    rows
}

/// The names of the jobs of [`rows`], sorted.
pub fn job_names() -> Vec<String> {
    let mut found: Vec<String> = rows().into_iter().map(|r| r.name).collect();
    found.sort();
    found
}

/// The `if:` a job of `file` named `name` must have to hold `id-token: write`, if a row gives it
/// the right: the jobs that assume the cloud role.
pub fn id_token_condition(file: &str, name: &str) -> Option<String> {
    if file != FILE {
        return None;
    }
    rows()
        .into_iter()
        .find(|r| r.name == name && r.permissions.contains(&("id-token", "write")))
        .map(|r| r.condition)
}

/// Whether `line` (trimmed) is a `docker` command the install check `job` may run: the one command
/// of each, with its image pinned by digest, no privilege, no mount but the xtask binary.
pub fn docker_line_ok(job: &str, line: &str) -> bool {
    match job {
        "check-apt" => {
            line == format!(
                "docker run --rm --volume \"$PWD/target/debug/xtask:/usr/local/bin/xtask:ro\" {UBUNTU_IMAGE} xtask channels check-apt --package \"$PACKAGE\" --arch \"$ARCH\" --version \"${{TAG#v}}\" --commit \"$COMMIT\""
            )
        }
        "check-rpm" => line == "docker run --detach --name check-rpm \"$IMAGE\" sleep infinity",
        _ => false,
    }
}

/// The verb of a `cargo xtask channels ...` call, from the words after `xtask`: `gate`,
/// `apt read`, `rpm repo write`, `pkg upload`, `check-apt`.
fn channel_verb(args: &[String]) -> Option<String> {
    let [first, rest @ ..] = args else {
        return None;
    };
    if first != "channels" {
        return None;
    }
    let word = |i: usize| rest.get(i).map_or("", String::as_str);
    Some(match word(0) {
        "apt" | "pkg" => format!("{} {}", word(0), word(1)),
        "rpm" if word(1) == "repo" => format!("rpm repo {}", word(2)),
        "rpm" => format!("rpm {}", word(1)),
        other => other.to_owned(),
    })
}

/// The `run:` scripts of `job`.
fn runs(job: &Node) -> Vec<&str> {
    steps(Some(job))
        .iter()
        .filter_map(|s| text_of(Some(s), "run"))
        .collect()
}

/// The sorted verbs of the `channels` calls of `job`, and the calls' words.
fn calls(job: &Node) -> (Vec<String>, Vec<Vec<String>>) {
    let mut verbs = Vec::new();
    let mut all = Vec::new();
    for run in runs(job) {
        for command in commands(run) {
            let Some(args) = xtask_args(&command) else {
                continue;
            };
            if let Some(verb) = channel_verb(args) {
                verbs.push(verb);
                all.push(args.to_vec());
            }
        }
    }
    verbs.sort();
    (verbs, all)
}

fn pairs(node: Option<&Node>) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = node
        .map(entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), v.as_str()?.to_owned())))
        .collect();
    found.sort();
    found
}

/// The values after `flag` in `calls`.
fn flag_values<'a>(calls: &'a [Vec<String>], flag: &str) -> Vec<&'a str> {
    calls
        .iter()
        .flat_map(|c| c.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()))
        .collect()
}

fn has_flag(calls: &[Vec<String>], verb: &str, flag: &str) -> bool {
    calls
        .iter()
        .filter(|c| channel_verb(c).as_deref() == Some(verb))
        .any(|c| c.iter().any(|w| w == flag))
}

/// One line per way the package-channel jobs of `release.yml` (`doc`) differ from [`rows`].
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let jobs = entry(doc, "jobs");
    for row in rows() {
        let Some(job) = jobs.and_then(|j| entry(j, &row.name)) else {
            problems.push(format!("release.yml: job {} is missing", row.name));
            continue;
        };
        problems.extend(row_problems(&row, job));
    }
    problems
}

fn row_problems(row: &Row, job: &Node) -> Vec<String> {
    let name = &row.name;
    let mut problems = Vec::new();
    let mut say = |text: String| problems.push(format!("release.yml: job {name} {text}"));
    let got = names(entry(job, "needs"));
    if got != row.needs {
        say(format!("needs {got:?}, not {:?}", row.needs));
    }
    if text_of(Some(job), "if") != Some(&row.condition) {
        say(format!("needs the exact `if: {}`", row.condition));
    }
    if text_of(Some(job), "environment") != row.environment {
        say(format!(
            "is in the environment {:?}, not {:?}",
            text_of(Some(job), "environment"),
            row.environment
        ));
    }
    let want: Vec<(String, String)> = row
        .permissions
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    if pairs(entry(job, "permissions")) != want {
        say(format!("needs exactly the permissions {want:?}"));
    }
    if entry(job, "continue-on-error").is_some() {
        say("has continue-on-error".to_owned());
    }
    let (verbs, calls) = calls(job);
    if verbs != row.verbs {
        say(format!(
            "runs the `channels` verbs {verbs:?}, not {:?}",
            row.verbs
        ));
    }
    let mut all = Vec::new();
    scalars(job, &mut all);
    let secrets: Vec<&str> = all
        .iter()
        .flat_map(|s| s.match_indices("secrets.").map(|(at, _)| &s[at + 8..]))
        .collect();
    for secret in &secrets {
        let key = secret
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next();
        if row.environment != Some(SIGNING_ENV) || !key.is_some_and(|k| KEY_SECRETS.contains(&k)) {
            say(format!(
                "names the secret {key:?}, which only the signing jobs may name, and only the key's"
            ));
        }
    }
    let role = row.permissions.contains(&("id-token", "write"));
    if !role && all.iter().any(|s| s.contains(ROLE_VAR)) {
        say(format!(
            "names {ROLE_VAR}, the role, which only the jobs with `id-token: write` assume"
        ));
    }
    let mut suites: Vec<&str> = flag_values(&calls, "--suite");
    suites.sort_unstable();
    suites.dedup();
    if suites != row.suites {
        say(format!("passes --suite {suites:?}, not {:?}", row.suites));
    }
    if let Some((attempt, last, pool)) = row.write {
        let verb = if name.contains("-apt-") {
            "apt write"
        } else {
            "rpm repo write"
        };
        if has_flag(&calls, verb, "--last") != (attempt == last) {
            say(format!(
                "passes --last exactly in the {} attempt, which fails on a lost race instead of asking for another",
                if last == 1 { "only" } else { "third" }
            ));
        }
        if has_flag(&calls, verb, "--no-pool") == pool {
            say("passes --no-pool exactly when it writes the index of stable, whose packages are in the pool already".to_owned());
        }
    }
    for session in flag_values(&calls, "--session-name") {
        if !session.starts_with(SESSION_PREFIX) {
            say(format!(
                "assumes the role under the session {session:?}, which does not start with {SESSION_PREFIX}"
            ));
        }
    }
    for call in calls
        .iter()
        .filter(|c| channel_verb(c).as_deref() == Some("assume"))
    {
        if flag_values(std::slice::from_ref(call), "--token-url") != [TOKEN_URL] {
            say(format!(
                "assumes the role without `--token-url \"{TOKEN_URL}\"`: the verb takes the token request's URL as a flag"
            ));
        }
    }
    problems.extend(step_problems(row, job));
    problems
}

/// The shape of the steps: the newest release's commit, its files, the gate.
fn step_problems(row: &Row, job: &Node) -> Vec<String> {
    let name = &row.name;
    let mut problems = Vec::new();
    let mut say = |text: String| problems.push(format!("release.yml: job {name} {text}"));
    let list = steps(Some(job));
    if row.name == NEWEST_JOB {
        let runs: Vec<&str> = list
            .iter()
            .filter_map(|s| text_of(Some(s), "run"))
            .collect();
        if list.len() != 1 || runs != [NEWEST_RUN] {
            say(format!("needs exactly the one step `{NEWEST_RUN}`"));
        }
        return problems;
    }
    let env = pairs(entry(job, "env"));
    for (key, want) in [("COMMIT", NEWEST_COMMIT), ("TAG", NEWEST_TAG)] {
        if !env.contains(&(key.to_owned(), want.to_owned())) {
            say(format!(
                "needs `env: {key}: {want}`: it works on the newest release of the run"
            ));
        }
    }
    let checkout = list
        .first()
        .filter(|s| text_of(Some(s), "uses").is_some_and(|u| u.starts_with("actions/checkout@")));
    let with = checkout.and_then(|s| entry(s, "with"));
    if text_of(with, "ref") != Some(NEWEST_COMMIT) {
        say(format!(
            "needs a first step that checks out `ref: {NEWEST_COMMIT}`"
        ));
    }
    for step in list {
        let uses = text_of(Some(step), "uses").unwrap_or_default();
        if uses.starts_with("actions/download-artifact@") {
            let artifact = text_of(entry(step, "with"), "name");
            if artifact != Some(RELEASE_ARTIFACT) && !is_job_artifact(artifact) {
                say(format!(
                    "downloads the artifact {artifact:?}, which is neither the release's ({RELEASE_ARTIFACT}) nor one of the channel jobs"
                ));
            }
        }
    }
    if !row.gated {
        return problems;
    }
    let gate = list
        .iter()
        .position(|s| text_of(Some(s), "id") == Some("gate"));
    let Some(gate) = gate else {
        say("has no step `id: gate`".to_owned());
        return problems;
    };
    if gate != 3 {
        say(
            "must run its gate right after the checkout, the toolchain and the compile of xtask"
                .to_owned(),
        );
    }
    let label = format!("cargo xtask channels gate --label {name} --output \"$GITHUB_OUTPUT\"");
    if !text_of(Some(&list[gate]), "run").is_some_and(|r| r.starts_with(&label)) {
        say(format!("needs a gate that starts `{label}`"));
    }
    for (at, step) in list.iter().enumerate().skip(gate + 1) {
        if text_of(Some(step), "if") != Some(GATE_IF) {
            say(format!(
                "has step {} that does not run under `if: {GATE_IF}`",
                at + 1
            ));
        }
    }
    if name == "testing-rpm-read" {
        let run = text_of(Some(&list[gate]), "run").unwrap_or_default();
        if !run.contains("--require \"RPM_REPO_READY=$RPM_REPO_READY\"") {
            say(
                "must be off while the repository variable RPM_REPO_READY is not `true`".to_owned(),
            );
        }
    }
    problems.extend(release_files_problems(name, list));
    problems
}

/// Whether `step` downloads the release's files.
fn takes_release_files(step: &Node) -> bool {
    text_of(Some(step), "uses").is_some_and(|u| u.starts_with("actions/download-artifact@"))
        && text_of(entry(step, "with"), "name") == Some(RELEASE_ARTIFACT)
}

/// One line per way the gated job `name`, with the steps `list`, uses the release's files before
/// it checked them: the download is into [`RELEASE_DIR`], the step right after it is
/// [`SUMS_CHECK`] in that directory, the next [`ATTESTATION_CHECK`] with [`ATTESTATION_TOKEN`],
/// and no step before those two runs a `channels` verb but the gate. Nothing for a job that does
/// not download the files.
pub fn release_files_problems(name: &str, list: &[Node]) -> Vec<String> {
    let Some(at) = list.iter().position(takes_release_files) else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let mut say = |text: String| problems.push(format!("release.yml: job {name} {text}"));
    if text_of(entry(&list[at], "with"), "path") != Some(RELEASE_DIR) {
        say(format!(
            "downloads the release's files elsewhere than `{RELEASE_DIR}`, which its checks read"
        ));
    }
    let sums = list.get(at + 1);
    if text_of(sums, "run") != Some(SUMS_CHECK)
        || text_of(sums, "working-directory") != Some(RELEASE_DIR)
    {
        say(format!(
            "needs the step `{SUMS_CHECK}` in `{RELEASE_DIR}` right after the download of the release's files"
        ));
    }
    let attested = list.get(at + 2);
    if text_of(attested, "run") != Some(ATTESTATION_CHECK)
        || !pairs(attested.and_then(|s| entry(s, "env")))
            .contains(&("GH_TOKEN".to_owned(), ATTESTATION_TOKEN.to_owned()))
    {
        say(format!(
            "needs the step `{ATTESTATION_CHECK}` with `GH_TOKEN: {ATTESTATION_TOKEN}` right after the check of SHA256SUMS: it signs or publishes only attested bytes"
        ));
    }
    for step in &list[..(at + 3).min(list.len())] {
        let run = text_of(Some(step), "run").unwrap_or_default();
        let early = commands(run)
            .iter()
            .filter_map(|c| xtask_args(c))
            .filter_map(channel_verb)
            .find(|verb| verb != "gate");
        if let Some(verb) = early {
            say(format!(
                "runs `channels {verb}` before it checked the release's files"
            ));
        }
    }
    problems
}

/// The artifacts the jobs hand each other: the repository read, the index built (`apt-fail-` for
/// the suite `testing-fail`), the signed rpms.
fn is_job_artifact(name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        let stem = n.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-');
        [
            "apt-current",
            "apt-next",
            "apt-fail-current",
            "apt-fail-next",
            "apt-read",
            "apt-promoted",
            "rpm-current",
            "rpm-next",
            "rpm-signed",
            "rpm-read",
            "rpm-promoted",
        ]
        .contains(&stem)
    })
}

/// One line per way the job `promote-check` does not give `channels decide` the result of each of
/// [`STAGES`], from the job of that name.
pub fn promote_check_problems(doc: &Node) -> Vec<String> {
    let Some(job) = entry(doc, "jobs").and_then(|j| entry(j, "promote-check")) else {
        return Vec::new(); // the rows report a job that is missing
    };
    let mut problems = Vec::new();
    let (_, all) = calls(job);
    let step = steps(Some(job))
        .iter()
        .find(|s| text_of(Some(s), "id") == Some("decide"));
    let env = pairs(step.and_then(|s| entry(s, "env")));
    let given = flag_values(&all, "--result");
    for stage in STAGES {
        let var = stage.to_uppercase().replace('-', "_");
        if !given.contains(&format!("{stage}=${var}").as_str()) {
            problems.push(format!(
                "release.yml: job promote-check gives `channels decide` no `--result {stage}=${var}`"
            ));
        }
        let want = format!("${{{{ needs.{stage}.result }}}}");
        if !env.contains(&(var.clone(), want.clone())) {
            problems.push(format!(
                "release.yml: job promote-check needs `{var}: {want}` in the env of its decide step"
            ));
        }
    }
    if given.len() != STAGES.len() {
        problems.push(format!(
            "release.yml: job promote-check gives `channels decide` {} results, not the {} stages",
            given.len(),
            STAGES.len()
        ));
    }
    problems
}

/// Whether the matrix of `job` lists only images pinned by `pin` (the base lint's rule), and the
/// step that starts the container gets its image from the matrix. One line per image that is not
/// pinned.
pub fn matrix_image_problems(name: &str, job: &Node, pinned: impl Fn(&str) -> bool) -> Vec<String> {
    let mut problems = Vec::new();
    let images = entry(job, "strategy")
        .and_then(|s| entry(s, "matrix"))
        .and_then(|m| entry(m, "image"));
    match images.map(|i| &i.value) {
        Some(Value::Seq(list)) => {
            for image in list {
                match image.as_str() {
                    Some(i) if pinned(i) => {}
                    other => problems.push(format!(
                        "release.yml: job {name}: the image {other:?} is not pinned by digest (name@sha256:<64 hex>)"
                    )),
                }
            }
        }
        _ => problems.push(format!(
            "release.yml: job {name} needs `strategy.matrix.image`, a list of images pinned by digest"
        )),
    }
    let started = steps(Some(job)).iter().any(|s| {
        text_of(Some(s), "run").is_some_and(|r| r.starts_with("docker run --detach"))
            && text_of(entry(s, "env"), "IMAGE") == Some("${{ matrix.image }}")
    });
    if !started {
        problems.push(format!(
            "release.yml: job {name} must start its container from `IMAGE: ${{{{ matrix.image }}}}`"
        ));
    }
    problems
}
