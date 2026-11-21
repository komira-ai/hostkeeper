//! The rules of the signing of the release's Mach-O programs in `artifacts.yml` (docs/macos.md,
//! "The programs' signature"), on top of [`super::pkg_signing`]'s and [`super::release`]'s:
//!
//! - **The programs are compiled by a job that holds no secret.** [`COMPILE_JOB`] needs exactly
//!   `version`, runs on `macos-15` with `contents: read` alone, and uploads the four programs
//!   of [`PROGRAMS`] as [`UNSIGNED_MACHO`].
//! - **The key is held by one job that runs no repository code.** [`SIGN_CODE_JOB`] runs under
//!   [`SIGN_IF`] in the environment [`SIGN_ENV`] on `macos-15`, needs exactly [`COMPILE_JOB`],
//!   holds `contents: read` alone, has the `env:` [`CODE_IDENTITY`] alone, and its steps are
//!   exactly [`SIGN_CODE_STEPS`], in order: no checkout, no cargo. A secret that is not set stops
//!   it by its name. It signs each of [`PROGRAMS`] under its identifier with `--options runtime
//!   --timestamp`, and uploads the signed programs as [`SIGNED_MACHO`] after the cleanup that
//!   runs under [`ALWAYS`].
//! - **The secrets are named there alone** ([`CODE_SECRETS`]; `super::pkg_signing` holds the
//!   installer's two to its own job).
//! - **The pkgs and the tarballs hold the signed programs.** `build-macos` needs [`COMPILE_JOB`]
//!   and [`SIGN_CODE_JOB`] and runs under [`SUMS_IF`] (a run by hand skips the signing); it
//!   builds nothing, downloads [`UNSIGNED_MACHO`] into `target/release`, then, under [`SIGN_IF`],
//!   [`SIGNED_MACHO`] over it, then sets the modes; under [`SIGN_IF`] it checks the signature of
//!   the packaged `hostkeeper` and `hk`.
//! - **Each signed pkg's program is checked.** `check-pkg` expands each signed pkg with its payload
//!   and runs `check-code-signature` on the program at its payload path ([`CHECK_HOSTKEEPER_CODE`],
//!   [`CHECK_HELLO_CODE`], [`CHECK_BOOTSTRAP_CODE`]); those are part of
//!   [`super::pkg_signing::CHECK_STEPS`].

use super::lint::entry;
use super::pkg_signing::{
    ALWAYS, SIGN_ENV, SUMS_IF, Step, artifact, job_problems, pairs, steps_problems, uses_of,
};
use super::release::{SIGN_IF, job, names, steps, text_of};
use super::yaml::Node;

/// The job that compiles the programs.
pub const COMPILE_JOB: &str = "compile-macos";

/// The job that signs them.
pub const SIGN_CODE_JOB: &str = "sign-code";

/// The job that packages them.
pub const BUILD_JOB: &str = "build-macos";

/// The Developer ID Application identity the release signs the programs with.
pub const CODE_IDENTITY: &str = "Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)";

/// The secrets [`SIGN_CODE_JOB`] names, and no other job: the PKCS#12 (base64) and its password.
pub const CODE_SECRETS: [&str; 2] = ["MACOS_APPLICATION_P12", "MACOS_APPLICATION_P12_PASSWORD"];

/// The artifact of the compiled programs, from [`COMPILE_JOB`].
pub const UNSIGNED_MACHO: &str = "macho-unsigned-macos-arm64";

/// The artifact of the signed programs, from [`SIGN_CODE_JOB`].
pub const SIGNED_MACHO: &str = "macho-signed-macos-arm64";

/// The programs and their signing identifiers (the table of xtask's `mac_programs.rs`, which a
/// unit test of xtask holds to the workflow too).
pub const PROGRAMS: [(&str, &str); 4] = [
    ("hostkeeper", "dev.hostkeeper.hostkeeper"),
    ("hk", "dev.hostkeeper.hk"),
    ("hello", "dev.hostkeeper.hello"),
    ("hostkeeper-bootstrap", "dev.hostkeeper.bootstrap"),
];

/// The `path:` of the upload of the compiled programs, as a block scalar reads.
pub const MACHO_PATH: &str = "target/release/hostkeeper\ntarget/release/hk\ntarget/release/hello\ntarget/release/hostkeeper-bootstrap\n";

/// The chmod of `build-macos`, after the downloads (an artifact does not keep a file's mode).
pub const CHMOD: &str = "chmod 0755 target/release/hostkeeper target/release/hk target/release/hello target/release/hostkeeper-bootstrap";

/// The check of the packaged hostkeeper in `build-macos`.
pub const CHECK_PACKAGED_HOSTKEEPER: &str = "cargo xtask release check-code-signature \"$RUNNER_TEMP/unpacked/hostkeeper\" --program hostkeeper";

/// The check of the packaged hk in `build-macos`.
pub const CHECK_PACKAGED_HK: &str =
    "cargo xtask release check-code-signature \"$RUNNER_TEMP/unpacked/hk\" --program hk";

/// The expansion, with payload, of the signed installer pkg, in `check-pkg`.
pub const EXPAND_FULL: &str = "pkgutil --expand-full \"dist/hostkeeper-$RELEASE_VERSION-macos-arm64.pkg\" \"$RUNNER_TEMP/signed-full\"";

/// The check of hostkeeper at its payload path in the installer pkg.
pub const CHECK_HOSTKEEPER_CODE: &str = "cargo xtask release check-code-signature \"$RUNNER_TEMP/signed-full/hostkeeper.pkg/Payload/usr/local/bin/hostkeeper\" --program hostkeeper";

/// The expansion, with payload, of hello's signed `.pkg`.
pub const EXPAND_HELLO_FULL: &str = "pkgutil --expand-full \"dist/hello-$RELEASE_VERSION-macos-arm64.pkg\" \"$RUNNER_TEMP/hello-signed-full\"";

/// The check of hello at its payload path in its pkg.
pub const CHECK_HELLO_CODE: &str = "cargo xtask release check-code-signature \"$RUNNER_TEMP/hello-signed-full/hello.pkg/Payload/Library/Application Support/hello/bin/hello\" --program hello";

/// The expansion, with payload, of the signed bootstrap pkg.
pub const EXPAND_BOOTSTRAP_FULL: &str = "pkgutil --expand-full \"dist/hostkeeper-bootstrap-$RELEASE_VERSION-macos-arm64.pkg\" \"$RUNNER_TEMP/bootstrap-signed-full\"";

/// The check of the bootstrap at its payload path in its pkg.
pub const CHECK_BOOTSTRAP_CODE: &str = "cargo xtask release check-code-signature \"$RUNNER_TEMP/bootstrap-signed-full/hostkeeper-bootstrap.pkg/Payload/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap\" --program hostkeeper-bootstrap";

/// The message of a step that stops the job by the name of a secret that is not set.
pub const NOT_SET_P12: &str = "test -n \"$P12\" || { echo \"::error::the secret MACOS_APPLICATION_P12 of the environment pkg-signing is not set (docs/operations/mac-full-disk-access.md says how the owner makes it)\"; exit 1; }";

/// As [`NOT_SET_P12`], for the password.
pub const NOT_SET_PASSWORD: &str = "test -n \"$PASSWORD\" || { echo \"::error::the secret MACOS_APPLICATION_P12_PASSWORD of the environment pkg-signing is not set (docs/operations/mac-full-disk-access.md says how the owner makes it)\"; exit 1; }";

/// The signing of hostkeeper, from `unsigned/` into `signed/`.
pub const SIGN_HOSTKEEPER: &str = "cp unsigned/hostkeeper signed/hostkeeper && codesign --force --sign \"$IDENTITY\" --keychain \"$RUNNER_TEMP/code-signing.keychain-db\" --identifier dev.hostkeeper.hostkeeper --options runtime --timestamp signed/hostkeeper";

/// The signing of hk.
pub const SIGN_HK: &str = "cp unsigned/hk signed/hk && codesign --force --sign \"$IDENTITY\" --keychain \"$RUNNER_TEMP/code-signing.keychain-db\" --identifier dev.hostkeeper.hk --options runtime --timestamp signed/hk";

/// The signing of hello.
pub const SIGN_HELLO_CODE: &str = "cp unsigned/hello signed/hello && codesign --force --sign \"$IDENTITY\" --keychain \"$RUNNER_TEMP/code-signing.keychain-db\" --identifier dev.hostkeeper.hello --options runtime --timestamp signed/hello";

/// The signing of the bootstrap.
pub const SIGN_BOOTSTRAP_CODE: &str = "cp unsigned/hostkeeper-bootstrap signed/hostkeeper-bootstrap && codesign --force --sign \"$IDENTITY\" --keychain \"$RUNNER_TEMP/code-signing.keychain-db\" --identifier dev.hostkeeper.bootstrap --options runtime --timestamp signed/hostkeeper-bootstrap";

/// The deletion of the job's keychain.
pub const DELETE_CODE_KEYCHAIN: &str =
    "security delete-keychain \"$RUNNER_TEMP/code-signing.keychain-db\"";

/// The deletion of the PKCS#12 file.
pub const DELETE_CODE_P12: &str = "rm -f \"$RUNNER_TEMP/application.p12\"";

const fn run(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: None,
        condition: None,
    }
}

const fn with_p12(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: Some(("P12", "MACOS_APPLICATION_P12")),
        condition: None,
    }
}

const fn with_password(run: &'static str) -> Step {
    Step::Run {
        run,
        secret: Some(("PASSWORD", "MACOS_APPLICATION_P12_PASSWORD")),
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

/// The steps of [`SIGN_CODE_JOB`], exactly and in this order.
pub const SIGN_CODE_STEPS: [Step; 18] = [
    Step::Download(UNSIGNED_MACHO, "unsigned"),
    with_p12(NOT_SET_P12),
    with_password(NOT_SET_PASSWORD),
    with_p12("base64 --decode -o \"$RUNNER_TEMP/application.p12\" <<< \"$P12\""),
    with_password(
        "security create-keychain -p \"$PASSWORD\" \"$RUNNER_TEMP/code-signing.keychain-db\"",
    ),
    run("security set-keychain-settings -lut 21600 \"$RUNNER_TEMP/code-signing.keychain-db\""),
    with_password(
        "security unlock-keychain -p \"$PASSWORD\" \"$RUNNER_TEMP/code-signing.keychain-db\"",
    ),
    with_password(
        "security import \"$RUNNER_TEMP/application.p12\" -k \"$RUNNER_TEMP/code-signing.keychain-db\" -f pkcs12 -P \"$PASSWORD\" -T /usr/bin/codesign",
    ),
    with_password(
        "security set-key-partition-list -S \"apple-tool:,apple:\" -s -k \"$PASSWORD\" \"$RUNNER_TEMP/code-signing.keychain-db\"",
    ),
    run("security list-keychains -d user -s \"$RUNNER_TEMP/code-signing.keychain-db\""),
    run("mkdir signed"),
    run(SIGN_HOSTKEEPER),
    run(SIGN_HK),
    run(SIGN_HELLO_CODE),
    run(SIGN_BOOTSTRAP_CODE),
    always(DELETE_CODE_KEYCHAIN),
    always(DELETE_CODE_P12),
    Step::Upload(SIGNED_MACHO, "signed/"),
];

/// One line per rule of this module that `doc`, artifacts.yml, breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = compile_problems(doc);
    problems.extend(sign_code_problems(doc));
    problems.extend(build_problems(doc));
    problems
}

/// What is wrong with [`COMPILE_JOB`]: its needs, runner or scope, or the upload of the compiled
/// programs.
fn compile_problems(doc: &Node) -> Vec<String> {
    let Some(j) = job(doc, COMPILE_JOB) else {
        return vec![format!("artifacts.yml: has no job {COMPILE_JOB}")];
    };
    let mut problems = Vec::new();
    if names(entry(j, "needs")) != ["version"] {
        problems.push(format!(
            "artifacts.yml: job {COMPILE_JOB} must need exactly [\"version\"]"
        ));
    }
    if text_of(Some(j), "runs-on") != Some("macos-15") {
        problems.push(format!(
            "artifacts.yml: job {COMPILE_JOB} must run on macos-15"
        ));
    }
    if pairs(entry(j, "permissions")) != [("contents", "read")] {
        problems.push(format!(
            "artifacts.yml: job {COMPILE_JOB} must hold `contents: read` and nothing else"
        ));
    }
    if entry(j, "environment").is_some() {
        problems.push(format!(
            "artifacts.yml: job {COMPILE_JOB} must be in no environment"
        ));
    }
    let uploads = steps(Some(j))
        .iter()
        .any(|s| super::pkg_signing::step_is(s, &Step::Upload(UNSIGNED_MACHO, MACHO_PATH)));
    if !uploads {
        problems.push(format!(
            "artifacts.yml: job {COMPILE_JOB} does not upload the four programs as {UNSIGNED_MACHO}"
        ));
    }
    problems
}

/// What is wrong with [`SIGN_CODE_JOB`]: its job keys, its environment, a step that differs
/// from [`SIGN_CODE_STEPS`], or a program not signed under its identifier with the hardened
/// runtime and a timestamp.
fn sign_code_problems(doc: &Node) -> Vec<String> {
    let mut problems = job_problems(
        doc,
        SIGN_CODE_JOB,
        &[COMPILE_JOB],
        &[("IDENTITY", CODE_IDENTITY)],
    );
    let j = job(doc, SIGN_CODE_JOB);
    if j.is_none() {
        return problems;
    }
    if text_of(j, "environment") != Some(SIGN_ENV) {
        problems.push(format!(
            "artifacts.yml: job {SIGN_CODE_JOB} must be in the environment {SIGN_ENV}"
        ));
    }
    problems.extend(steps_problems(
        SIGN_CODE_JOB,
        steps(j),
        &SIGN_CODE_STEPS,
        "SIGN_CODE_STEPS",
    ));
    for (name, identifier) in PROGRAMS {
        let wanted =
            format!("--identifier {identifier} --options runtime --timestamp signed/{name}");
        let signed = steps(j).iter().any(|s| {
            text_of(Some(s), "run").is_some_and(|r| {
                r.contains("codesign --force --sign \"$IDENTITY\"") && r.ends_with(&wanted)
            })
        });
        if !signed {
            problems.push(format!(
                "artifacts.yml: job {SIGN_CODE_JOB} does not sign {name} with `{wanted}`"
            ));
        }
    }
    problems
}

/// Whether `step` downloads `name` into `target/release` under the condition `condition`.
fn downloads(step: &Node, name: &str, condition: Option<&str>) -> bool {
    uses_of(step).starts_with("actions/download-artifact@")
        && artifact(step) == (Some(name), Some("target/release"))
        && pairs(entry(step, "with")).len() == 2
        && text_of(Some(step), "if") == condition
}

/// What is wrong with [`BUILD_JOB`] as the packager of signed programs: its needs and `if:`, a
/// program built there, the downloads of the programs and the modes, or the checks of the
/// packaged programs' signatures.
fn build_problems(doc: &Node) -> Vec<String> {
    let Some(j) = job(doc, BUILD_JOB) else {
        return vec![format!("artifacts.yml: has no job {BUILD_JOB}")];
    };
    let mut problems = Vec::new();
    if names(entry(j, "needs")) != [COMPILE_JOB, SIGN_CODE_JOB, "version"] {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} must need exactly [\"{COMPILE_JOB}\", \"{SIGN_CODE_JOB}\", \"version\"]"
        ));
    }
    if text_of(Some(j), "if") != Some(SUMS_IF) {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} needs the exact `if: {SUMS_IF}`"
        ));
    }
    let all = steps(Some(j));
    if all
        .iter()
        .any(|s| text_of(Some(s), "run").is_some_and(|r| r.starts_with("cargo build")))
    {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} builds a program; the programs it packages are compiled by {COMPILE_JOB} and signed by {SIGN_CODE_JOB}"
        ));
    }
    let at = |f: &dyn Fn(&Node) -> bool| all.iter().position(f);
    let unsigned = at(&|s| downloads(s, UNSIGNED_MACHO, None));
    let signed = at(&|s| downloads(s, SIGNED_MACHO, Some(SIGN_IF)));
    let modes = at(&|s| text_of(Some(s), "run") == Some(CHMOD) && entry(s, "if").is_none());
    if unsigned.is_none() {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} does not download {UNSIGNED_MACHO} into target/release"
        ));
    }
    if signed.is_none() {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} does not download {SIGNED_MACHO} into target/release under the exact `if: {SIGN_IF}`"
        ));
    }
    if modes.is_none() {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} does not run `{CHMOD}`"
        ));
    }
    if let (Some(u), Some(s), Some(m)) = (unsigned, signed, modes)
        && !(u < s && s < m)
    {
        problems.push(format!(
            "artifacts.yml: job {BUILD_JOB} must download {UNSIGNED_MACHO}, then {SIGNED_MACHO}, then set the modes"
        ));
    }
    for command in [CHECK_PACKAGED_HOSTKEEPER, CHECK_PACKAGED_HK] {
        let checked = all.iter().any(|s| {
            text_of(Some(s), "run") == Some(command) && text_of(Some(s), "if") == Some(SIGN_IF)
        });
        if !checked {
            problems.push(format!(
                "artifacts.yml: job {BUILD_JOB} does not run `{command}` under the exact `if: {SIGN_IF}`"
            ));
        }
    }
    problems
}
