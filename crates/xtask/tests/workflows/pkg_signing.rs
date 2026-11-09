//! The rules of the signing of the installer pkg in `artifacts.yml` (docs/macos.md, "The
//! installer's signature"), on top of [`super::release`]'s:
//!
//! - **The unsigned pkg is never a release file.** The build job `build-macos` writes it with
//!   [`BUILD_PKG`] into `pkg/` (not `dist/`, which its `assets-*` artifact uploads) and uploads it
//!   as [`UNSIGNED_ARTIFACT`].
//! - **The key is held by one job that runs no repository code.** [`SIGN_JOB`] runs under
//!   [`SIGN_IF`] (a push to main) in the environment [`SIGN_ENV`], on `macos-15`, needs exactly
//!   `version` and `build-macos`, holds `contents: read` alone, and its steps are exactly
//!   [`SIGN_STEPS`], in order: no checkout, no cargo. It imports the identity into a keychain of
//!   the job (never the login keychain), signs with a trusted timestamp, and then, under
//!   [`ALWAYS`], deletes the keychain and the PKCS#12 file before it uploads the signed pkg as
//!   [`SIGNED_ARTIFACT`].
//! - **The secrets are named there alone.** No other job of the file, and nothing beside the
//!   jobs, names a secret; that job names exactly [`SECRETS`], each only in the `env:` of a step.
//! - **The signed pkg is checked before it is published.** [`CHECK_JOB`] runs under [`SIGN_IF`]
//!   after [`SIGN_JOB`], with `contents: read` alone, and runs [`CHECK_SIGNATURE`] and the
//!   comparison [`SAME_FILES`] of the expanded signed and unsigned pkgs before the one upload of
//!   the file as [`PUBLISHED_ARTIFACT`], which no other job uploads.
//! - **The sums wait for it.** `sums` needs [`CHECK_JOB`] under [`SUMS_IF`], so on a push to main
//!   SHA256SUMS, the attestation and the release take the signed pkg, and a run by hand, which
//!   signs nothing, still makes the sums.
//!
//! Both jobs name the identity [`IDENTITY`] in their `env:`.

use super::lint::{entries, entry, scalars};
use super::release::{SIGN_IF, job, names, steps, text_of};
use super::yaml::Node;

/// The job that signs.
pub const SIGN_JOB: &str = "sign-pkg";

/// The job that checks the signed pkg and publishes it.
pub const CHECK_JOB: &str = "check-pkg";

/// The environment of [`SIGN_JOB`].
pub const SIGN_ENV: &str = "pkg-signing";

/// The Developer ID Installer identity the release signs with.
pub const IDENTITY: &str = "Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)";

/// The secrets [`SIGN_JOB`] names, and no other job: the PKCS#12 (base64) and its password.
pub const SECRETS: [&str; 2] = ["MACOS_INSTALLER_P12", "MACOS_INSTALLER_P12_PASSWORD"];

/// The `if:` of the cleanup steps: they run whether the signing worked or not.
pub const ALWAYS: &str = "${{ always() }}";

/// The `if:` of `sums`: it runs when nothing it needs failed or was cancelled, so a skipped
/// [`CHECK_JOB`] (a run by hand) does not skip it.
pub const SUMS_IF: &str = "${{ !cancelled() && !failure() }}";

/// How `build-macos` builds hostkeeper's pkg: into `pkg/`.
pub const BUILD_PKG: &str = "cargo xtask release macos-pkg --version \"$RELEASE_VERSION\" --binary target/release/hostkeeper --gh \"$RUNNER_TEMP/gh-pinned/gh\" --trusted-root \"$RUNNER_TEMP/trusted_root.json\" --stage \"$RUNNER_TEMP/pkg-root\" --out pkg";

/// The artifact of the unsigned pkg, from `build-macos`.
pub const UNSIGNED_ARTIFACT: &str = "pkg-unsigned-macos-arm64";

/// The artifact of the signed pkg, from [`SIGN_JOB`].
pub const SIGNED_ARTIFACT: &str = "pkg-signed-macos-arm64";

/// The release files' artifact of the checked pkg, from [`CHECK_JOB`].
pub const PUBLISHED_ARTIFACT: &str = "assets-pkg-macos-arm64";

/// One step of [`SIGN_STEPS`] or [`CHECK_STEPS`]. Each holds every key of its step: a step
/// with any other key (`continue-on-error`, `shell`, `working-directory`, `timeout-minutes`,
/// another `with:` input such as `run-id` or `repository`) is not it. `name` is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `actions/checkout` with `persist-credentials: false` and nothing else.
    Checkout,
    /// `actions/download-artifact` of the artifact (`name`) into the directory (`path`), and
    /// no other input.
    Download(&'static str, &'static str),
    /// A `run:`, exactly, with the secret its `env:` names (the variable, then the secret it
    /// is set from) and its `if:`.
    Run {
        run: &'static str,
        secret: Option<(&'static str, &'static str)>,
        condition: Option<&'static str>,
    },
    /// `actions/upload-artifact` of the directory (`path`) as the artifact (`name`), with
    /// `retention-days: 90` and `if-no-files-found: error`, and no other input.
    Upload(&'static str, &'static str),
}

const fn run(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: None,
        condition: None,
    }
}

const fn with_password(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: Some(("PASSWORD", "MACOS_INSTALLER_P12_PASSWORD")),
        condition: None,
    }
}

const fn always(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: None,
        condition: Some(ALWAYS),
    }
}

/// The steps of [`SIGN_JOB`], exactly and in this order.
pub const SIGN_STEPS: [Step; 13] = [
    Step::Download(UNSIGNED_ARTIFACT, "unsigned"),
    Step::Run {
        run: "base64 --decode -o \"$RUNNER_TEMP/installer.p12\" <<< \"$P12\"",
        secret: Some(("P12", "MACOS_INSTALLER_P12")),
        condition: None,
    },
    with_password(
        "security create-keychain -p \"$PASSWORD\" \"$RUNNER_TEMP/pkg-signing.keychain-db\"",
    ),
    run("security set-keychain-settings -lut 21600 \"$RUNNER_TEMP/pkg-signing.keychain-db\""),
    with_password(
        "security unlock-keychain -p \"$PASSWORD\" \"$RUNNER_TEMP/pkg-signing.keychain-db\"",
    ),
    with_password(
        "security import \"$RUNNER_TEMP/installer.p12\" -k \"$RUNNER_TEMP/pkg-signing.keychain-db\" -f pkcs12 -P \"$PASSWORD\" -T /usr/bin/productsign",
    ),
    with_password(
        "security set-key-partition-list -S \"apple-tool:,apple:\" -s -k \"$PASSWORD\" \"$RUNNER_TEMP/pkg-signing.keychain-db\"",
    ),
    run("security list-keychains -d user -s \"$RUNNER_TEMP/pkg-signing.keychain-db\""),
    run("mkdir dist"),
    run(SIGN),
    always(DELETE_KEYCHAIN),
    always(DELETE_P12),
    Step::Upload(SIGNED_ARTIFACT, "dist/"),
];

/// The signing: the identity, from the job's keychain, with a trusted timestamp, from
/// `unsigned/` into `dist/`.
pub const SIGN: &str = "productsign --sign \"$IDENTITY\" --keychain \"$RUNNER_TEMP/pkg-signing.keychain-db\" --timestamp \"unsigned/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\" \"dist/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\"";

/// The deletion of the job's keychain.
pub const DELETE_KEYCHAIN: &str =
    "security delete-keychain \"$RUNNER_TEMP/pkg-signing.keychain-db\"";

/// The deletion of the PKCS#12 file.
pub const DELETE_P12: &str = "rm -f \"$RUNNER_TEMP/installer.p12\"";

/// The steps of [`CHECK_JOB`], exactly and in this order: the signature checked and the files
/// compared before the one upload, and nothing between them that could put other bytes in
/// `dist/`.
pub const CHECK_STEPS: [Step; 9] = [
    Step::Checkout,
    run("rustup toolchain install"),
    Step::Download(UNSIGNED_ARTIFACT, "unsigned"),
    Step::Download(SIGNED_ARTIFACT, "dist"),
    run(CHECK_SIGNATURE),
    run(EXPAND_UNSIGNED),
    run(EXPAND_SIGNED),
    run(SAME_FILES),
    Step::Upload(PUBLISHED_ARTIFACT, "dist/"),
];

/// The expansion of the unsigned pkg, in [`CHECK_JOB`].
pub const EXPAND_UNSIGNED: &str = "pkgutil --expand \"unsigned/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\" \"$RUNNER_TEMP/unsigned-expanded\"";

/// The expansion of the signed pkg, in [`CHECK_JOB`].
pub const EXPAND_SIGNED: &str = "pkgutil --expand \"dist/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\" \"$RUNNER_TEMP/signed-expanded\"";

/// The check of the signature, in [`CHECK_JOB`].
pub const CHECK_SIGNATURE: &str = "cargo xtask release check-pkg-signature \"dist/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\" --identity \"$IDENTITY\"";

/// The comparison of the expanded unsigned and signed pkgs, in [`CHECK_JOB`].
pub const SAME_FILES: &str =
    "diff -r \"$RUNNER_TEMP/unsigned-expanded\" \"$RUNNER_TEMP/signed-expanded\"";

/// One line per rule of this module that `doc`, artifacts.yml, breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    problems.extend(build_problems(doc));
    problems.extend(sign_problems(doc));
    problems.extend(secret_problems(doc));
    problems.extend(check_problems(doc));
    let sums = job(doc, "sums");
    if !names(sums.and_then(|j| entry(j, "needs"))).contains(&CHECK_JOB.to_owned())
        || text_of(sums, "if") != Some(SUMS_IF)
    {
        problems.push(format!(
            "artifacts.yml: job sums must need {CHECK_JOB} under the exact `if: {SUMS_IF}`"
        ));
    }
    problems
}

fn uses_of(step: &Node) -> &str {
    text_of(Some(step), "uses").unwrap_or_default()
}

/// The `name` and `path` of an upload or download step.
fn artifact(step: &Node) -> (Option<&str>, Option<&str>) {
    let with = entry(step, "with");
    (text_of(with, "name"), text_of(with, "path"))
}

fn is_upload(step: &Node) -> bool {
    uses_of(step).starts_with("actions/upload-artifact@")
}

/// What is wrong with the build job's pkg: built into another directory than `pkg/`, or not
/// uploaded as [`UNSIGNED_ARTIFACT`].
fn build_problems(doc: &Node) -> Vec<String> {
    let build = steps(job(doc, "build-macos"));
    let mut problems = Vec::new();
    if !build
        .iter()
        .any(|s| text_of(Some(s), "run") == Some(BUILD_PKG))
    {
        problems.push(format!(
            "artifacts.yml: job build-macos does not build the unsigned pkg into pkg/ (`{BUILD_PKG}`)"
        ));
    }
    let uploads = build
        .iter()
        .any(|s| is_upload(s) && artifact(s) == (Some(UNSIGNED_ARTIFACT_EXPR), Some("pkg/")));
    if !uploads {
        problems.push(format!(
            "artifacts.yml: job build-macos does not upload pkg/ as {UNSIGNED_ARTIFACT}"
        ));
    }
    problems
}

/// [`UNSIGNED_ARTIFACT`] as `build-macos` names it, from its platform.
const UNSIGNED_ARTIFACT_EXPR: &str = "pkg-unsigned-${{ env.PLATFORM }}";

/// The keys a job of the two may have: no `continue-on-error`, `defaults`, `container`,
/// `services`, `strategy` or `outputs`. [`SIGN_JOB`] also has `environment`.
const JOB_KEYS: [&str; 7] = [
    "env",
    "if",
    "needs",
    "permissions",
    "runs-on",
    "steps",
    "timeout-minutes",
];

/// The `env:` of both jobs, exactly.
const JOB_ENV: [(&str, &str); 2] = [
    ("RELEASE_VERSION", "${{ needs.version.outputs.version }}"),
    ("IDENTITY", IDENTITY),
];

/// What is wrong with the job-level keys of the job `name`: a key outside [`JOB_KEYS`] (and
/// `environment` for [`SIGN_JOB`]), another `if`, runner, `needs`, `permissions` or `env`.
fn job_problems(doc: &Node, name: &str, needs: &[&str]) -> Vec<String> {
    let j = job(doc, name);
    let mut problems = Vec::new();
    let Some(node) = j else {
        return vec![format!("artifacts.yml: has no job {name}")];
    };
    for key in entries(node).iter().filter_map(|(k, _)| k.as_str()) {
        let allowed = JOB_KEYS.contains(&key) || (name == SIGN_JOB && key == "environment");
        if !allowed {
            problems.push(format!("artifacts.yml: job {name} has the key {key}"));
        }
    }
    if text_of(j, "if") != Some(SIGN_IF) {
        problems.push(format!(
            "artifacts.yml: job {name} needs the exact `if: {SIGN_IF}`"
        ));
    }
    if text_of(j, "runs-on") != Some("macos-15") {
        problems.push(format!("artifacts.yml: job {name} must run on macos-15"));
    }
    if names(j.and_then(|j| entry(j, "needs"))) != needs {
        problems.push(format!(
            "artifacts.yml: job {name} must need exactly {needs:?}"
        ));
    }
    let permissions: Vec<(&str, &str)> = j
        .and_then(|j| entry(j, "permissions"))
        .map_or(&[][..], entries)
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?, v.as_str()?)))
        .collect();
    if permissions != [("contents", "read")] {
        problems.push(format!(
            "artifacts.yml: job {name} must hold `contents: read` and nothing else"
        ));
    }
    if pairs(entry(node, "env")) != JOB_ENV {
        problems.push(format!(
            "artifacts.yml: job {name} must have exactly the env {JOB_ENV:?}"
        ));
    }
    problems
}

/// The scalar entries of the mapping `node`, in order.
fn pairs(node: Option<&Node>) -> Vec<(&str, &str)> {
    node.map_or(&[][..], entries)
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?, v.as_str()?)))
        .collect()
}

/// One line per way the steps of the job `name` differ from `want`, named `list`: another
/// count, or a step that is not the one at its place.
fn steps_problems(name: &str, found: &[Node], want: &[Step], list: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if found.len() != want.len() {
        problems.push(format!(
            "artifacts.yml: job {name} has {} steps, not the {} of {list}",
            found.len(),
            want.len()
        ));
    }
    for (at, (step, want)) in found.iter().zip(want).enumerate() {
        if !step_is(step, want) {
            problems.push(format!(
                "artifacts.yml: job {name}: step {} is not {want:?}",
                at + 1
            ));
        }
    }
    problems
}

/// What is wrong with [`SIGN_JOB`]: its job keys, its environment, or a step that differs from
/// [`SIGN_STEPS`] (one missing, added, changed or moved).
fn sign_problems(doc: &Node) -> Vec<String> {
    let mut problems = job_problems(doc, SIGN_JOB, &["build-macos", "version"]);
    let j = job(doc, SIGN_JOB);
    if j.is_none() {
        return problems;
    }
    if text_of(j, "environment") != Some(SIGN_ENV) {
        problems.push(format!(
            "artifacts.yml: job {SIGN_JOB} must be in the environment {SIGN_ENV}"
        ));
    }
    problems.extend(steps_problems(
        SIGN_JOB,
        steps(j),
        &SIGN_STEPS,
        "SIGN_STEPS",
    ));
    problems
}

/// Whether `step` is `want`: every key of it but `name`, and every input of its `with`.
fn step_is(step: &Node, want: &Step) -> bool {
    let mut keys: Vec<&str> = entries(step)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .filter(|k| *k != "name")
        .collect();
    keys.sort_unstable();
    let with = pairs(entry(step, "with"));
    let action = |prefix: &str, inputs: &[(&str, &str)]| {
        keys == ["uses", "with"]
            && uses_of(step).starts_with(prefix)
            && with.len() == inputs.len()
            && inputs.iter().all(|i| with.contains(i))
    };
    match *want {
        Step::Checkout => action("actions/checkout@", &[("persist-credentials", "false")]),
        Step::Download(name, path) => action(
            "actions/download-artifact@",
            &[("name", name), ("path", path)],
        ),
        Step::Upload(name, path) => action(
            "actions/upload-artifact@",
            &[
                ("name", name),
                ("path", path),
                ("retention-days", "90"),
                ("if-no-files-found", "error"),
            ],
        ),
        Step::Run {
            run,
            secret,
            condition,
        } => {
            let mut want_keys = vec!["run"];
            want_keys.extend(secret.map(|_| "env"));
            want_keys.extend(condition.map(|_| "if"));
            want_keys.sort_unstable();
            let want_env: Vec<(&str, String)> = secret
                .map(|(var, name)| (var, format!("${{{{ secrets.{name} }}}}")))
                .into_iter()
                .collect();
            let env = pairs(entry(step, "env"));
            let env_ok = env.len() == want_env.len()
                && env
                    .iter()
                    .zip(&want_env)
                    .all(|((k, v), (wk, wv))| k == wk && v == wv);
            keys == want_keys
                && text_of(Some(step), "run") == Some(run)
                && env_ok
                && text_of(Some(step), "if") == condition
        }
    }
}

/// One line per job (or the part beside the jobs) of artifacts.yml that names a secret but
/// [`SIGN_JOB`], and one per secret [`SIGN_JOB`] names that is not one of [`SECRETS`].
fn secret_problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let mut beside = Vec::new();
    for (k, v) in entries(doc)
        .iter()
        .filter(|(k, _)| k.as_str() != Some("jobs"))
    {
        scalars(k, &mut beside);
        scalars(v, &mut beside);
    }
    if beside.iter().any(|s| s.contains("secrets")) {
        problems.push("artifacts.yml: names a secret beside the jobs".to_owned());
    }
    for (name, j) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        let mut values = Vec::new();
        scalars(j, &mut values);
        let named: Vec<&str> = values
            .iter()
            .flat_map(|s| s.match_indices("secrets").map(move |(at, _)| &s[at..]))
            .collect();
        if name != SIGN_JOB {
            if !named.is_empty() {
                problems.push(format!(
                    "artifacts.yml: job {name} names a secret; only {SIGN_JOB} may"
                ));
            }
            continue;
        }
        let in_steps = SIGN_STEPS
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    Step::Run {
                        secret: Some(_),
                        ..
                    }
                )
            })
            .count();
        if named.len() != in_steps {
            problems.push(format!(
                "artifacts.yml: job {SIGN_JOB} names a secret {} times, not once in the env of each step of SIGN_STEPS that takes one ({in_steps})",
                named.len()
            ));
        }
        for text in named {
            let secret = text
                .strip_prefix("secrets.")
                .map(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
                .and_then(|mut words| words.next())
                .unwrap_or_default();
            if !SECRETS.contains(&secret) {
                problems.push(format!(
                    "artifacts.yml: job {SIGN_JOB} names a secret other than {SECRETS:?} (`{}`)",
                    text.lines().next().unwrap_or_default()
                ));
            }
        }
    }
    problems
}

/// What is wrong with [`CHECK_JOB`]: its job keys, a step that differs from [`CHECK_STEPS`],
/// or another job uploading an `assets-pkg-*` artifact.
fn check_problems(doc: &Node) -> Vec<String> {
    let mut problems = job_problems(doc, CHECK_JOB, &["sign-pkg", "version"]);
    if let Some(j) = job(doc, CHECK_JOB) {
        problems.extend(steps_problems(
            CHECK_JOB,
            steps(Some(j)),
            &CHECK_STEPS,
            "CHECK_STEPS",
        ));
    }
    for (name, j) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        let other = steps(Some(j)).iter().any(|s| {
            uses_of(s).starts_with("actions/upload-artifact@")
                && text_of(entry(s, "with"), "name").is_some_and(|a| a.starts_with("assets-pkg-"))
        });
        if name != CHECK_JOB && other {
            problems.push(format!(
                "artifacts.yml: job {name} uploads an assets-pkg-* artifact; only {CHECK_JOB} publishes the pkg"
            ));
        }
    }
    problems
}
