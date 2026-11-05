//! The checks `set-desired.yml` runs are the checks a consumer runs: steps 2, 3 and 6 of
//! [`DOC`]. [`problems`] reads each of those commands out of the document, writes its
//! placeholders the way the workflow names the same things ([`NAMES`]), and refuses the workflow
//! when:
//! - the job's `HOSTKEEPER_TAG` is not the document's tag, or its `HOSTKEEPER_ASSET` is not the
//!   asset the document's table lists as [`BINARY`], for the platform [`PLATFORM`], which the
//!   document's sentence defining [`PLATFORMS`] lists;
//! - the `checksum` step does not run the document's step 2 command word for word, in
//!   [`RELEASE_DIR`] (the document's `cd release`);
//! - the `attestation` step does not run the document's step 3 `gh attestation verify` word for
//!   word, its flags in the same order, followed only by [`ATTESTATION_OUTPUT`], the file the
//!   commit step reads;
//! - the `version` step does not compare the output of the document's step 6 `--version` command
//!   (the step's `tar` line is not compared: the workflow unpacks only `hostkeeper`, without
//!   owners, into a directory of its own)
//!   with the document's expected output, and nothing else, or does not take `HOSTKEEPER_VERSION`
//!   and `HOSTKEEPER_COMMIT` from their inputs ([`VERSION_ENV`]).
//!
//! A command or an expected output missing from the document is a problem too, so a change to
//! either file alone is refused. The commands are read as text and never run.

use super::lint::entry;
use super::set_desired::{COMMIT_INPUT, JOB, PROGRAM, logical_lines};
use super::yaml::{Node, Value};

/// The consumer's check of a release, from the root of the repository.
pub const DOC: &str = "docs/deploy/verify-a-release.md";
/// How the workflow names what the document writes as a placeholder, in the order they are
/// replaced (the commit's first 12 digits before the commit).
pub const NAMES: [(&str, &str); 6] = [
    ("<first 12 hex digits of $PIN>", "${HOSTKEEPER_COMMIT:0:12}"),
    ("<X.Y.Z>", "$HOSTKEEPER_VERSION"),
    ("$PIN", "$HOSTKEEPER_COMMIT"),
    (
        "release/<asset>",
        "\"$RUNNER_TEMP/hostkeeper-release/$HOSTKEEPER_ASSET\"",
    ),
    ("cd release && ", ""),
    ("./hostkeeper", PROGRAM),
];
/// The directory the workflow downloads the release to, where the checksum step runs.
pub const RELEASE_DIR: &str = "${{ runner.temp }}/hostkeeper-release";
/// What the attestation step adds to the document's command: the verified attestations, as JSON,
/// in the file the commit step reads.
pub const ATTESTATION_OUTPUT: [&str; 4] = [
    "--format",
    "json",
    ">",
    "\"$RUNNER_TEMP/hostkeeper-release/attestation.json\"",
];
/// What the document's table says the binary tarball holds, the one asset the workflow unpacks.
pub const BINARY: &str = "the daemon binary";
/// The start of the document's sentence that lists the platforms, up to its `;`.
pub const PLATFORMS: &str = "`<platform>` is ";
/// The platform of the runner, as the document names it.
pub const PLATFORM: &str = "linux-x86_64";
/// The hostkeeper version, as the job's `env` names it.
pub const VERSION_INPUT: &str = "${{ inputs.hostkeeper_version }}";
/// The variables the version step compares with, and the inputs they must be.
pub const VERSION_ENV: [(&str, &str); 2] = [
    ("HOSTKEEPER_VERSION", VERSION_INPUT),
    ("HOSTKEEPER_COMMIT", COMMIT_INPUT),
];
/// The sentence of step 6 that gives the expected output, up to the output's opening backquote.
pub const EXPECTED: &str = "Expected output, exactly: `";

/// `text` with the document's placeholders written as the workflow names them.
fn named(text: &str) -> String {
    NAMES
        .iter()
        .fold(text.to_owned(), |text, &(from, to)| text.replace(from, to))
}

/// The words of a script, split on whitespace with their quotes kept, its logical lines
/// separated by `;`: a script of one command is that command's words.
fn script_words(script: &str) -> Vec<String> {
    logical_lines(script)
        .join(" ; ")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// The text of step `n` of the document, from its heading up to the next heading.
fn section(doc: &str, n: u8) -> Option<&str> {
    let start = doc.find(&format!("\n### {n}. "))? + 1;
    let rest = &doc[start..];
    Some(&rest[..rest.find("\n##").unwrap_or(rest.len())])
}

/// The words of the first command of step `n`'s `sh` blocks for which `is` holds, with the
/// placeholders named as the workflow names them; or the problem that there is none.
fn command(doc: &str, n: u8, what: &str, is: fn(&[&str]) -> bool) -> Result<Vec<String>, String> {
    section(doc, n)
        .into_iter()
        .flat_map(|s| s.split("```sh\n").skip(1))
        .flat_map(|block| logical_lines(block.split("\n```").next().unwrap_or_default()))
        .find(|line| is(&line.split_whitespace().collect::<Vec<_>>()))
        .map(|line| script_words(&named(&line)))
        .ok_or_else(|| format!("{DOC} has no `{what}` command in step {n}"))
}

/// The step of the one job with `id: id`.
fn step<'a>(workflow: &'a Node, id: &str) -> Option<&'a Node> {
    let steps = entry(workflow, "jobs")
        .and_then(|jobs| entry(jobs, JOB))
        .and_then(|job| entry(job, "steps"));
    match steps.map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps
            .iter()
            .find(|s| entry(s, "id").and_then(Node::as_str) == Some(id)),
        _ => None,
    }
}

/// The value of `key` in the `env` of `node`.
fn env<'a>(node: Option<&'a Node>, key: &str) -> Option<&'a str> {
    node.and_then(|n| entry(n, "env"))
        .and_then(|e| entry(e, key))
        .and_then(Node::as_str)
}

/// The words of the `run:` of step `id`, empty when there is no such step.
fn run_words(workflow: &Node, id: &str) -> Vec<String> {
    script_words(
        step(workflow, id)
            .and_then(|s| entry(s, "run"))
            .and_then(Node::as_str)
            .unwrap_or_default(),
    )
}

/// The problem that step `id` runs `got` and not `want`, the command of the document's step `n`.
fn differs(id: &str, got: &[String], want: &[String], n: u8) -> Option<String> {
    (got != want).then(|| {
        format!(
            "the {id} step runs `{}`, not `{}` of {DOC} step {n}",
            got.join(" "),
            want.join(" ")
        )
    })
}

/// One line per way `workflow`, the CI client, departs from `doc`, the text of [`DOC`].
pub fn problems(doc: &str, workflow: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let job = entry(workflow, "jobs").and_then(|jobs| entry(jobs, JOB));
    let tag = env(job, "HOSTKEEPER_TAG").unwrap_or_default();
    if !doc.contains(&format!("| `{}` |", tag.replace(VERSION_INPUT, "<X.Y.Z>"))) {
        problems.push(format!(
            "the job's `HOSTKEEPER_TAG` `{tag}` is not the tag of {DOC}"
        ));
    }
    let asset = env(job, "HOSTKEEPER_ASSET").unwrap_or_default();
    let listed = asset
        .replace(VERSION_INPUT, "<version>")
        .replace(PLATFORM, "<platform>");
    if !(doc.contains(&format!("| `{listed}` | {BINARY} |"))
        && doc
            .split_once(PLATFORMS)
            .and_then(|(_, rest)| rest.split(';').next())
            .is_some_and(|platforms| platforms.contains(&format!("`{PLATFORM}`"))))
    {
        problems.push(format!(
            "the job's `HOSTKEEPER_ASSET` `{asset}` is not the binary tarball of {DOC} for `{PLATFORM}`"
        ));
    }

    match command(doc, 2, "sha256sum", |w| w.contains(&"sha256sum")) {
        Ok(want) => problems.extend(differs(
            "checksum",
            &run_words(workflow, "checksum"),
            &want,
            2,
        )),
        Err(e) => problems.push(e),
    }
    let checksum = step(workflow, "checksum");
    if checksum
        .and_then(|s| entry(s, "working-directory"))
        .and_then(Node::as_str)
        != Some(RELEASE_DIR)
    {
        problems.push(format!(
            "the checksum step does not run in `{RELEASE_DIR}`, the release directory of {DOC} step 2"
        ));
    }

    let verify = command(doc, 3, "gh attestation verify", |w| {
        w.starts_with(&["gh", "attestation", "verify"])
    });
    match verify {
        Ok(mut want) => {
            want.extend(ATTESTATION_OUTPUT.map(str::to_owned));
            let got = run_words(workflow, "attestation");
            problems.extend(differs("attestation", &got, &want, 3));
        }
        Err(e) => problems.push(e),
    }

    let expected = section(doc, 6)
        .and_then(|s| s.split_once(EXPECTED))
        .and_then(|(_, rest)| rest.split('`').next());
    let version = command(doc, 6, "--version", |w| w.contains(&"--version"));
    match (version, expected) {
        (Ok(run), Some(expected)) => {
            let program = run.first().map_or("", String::as_str);
            let args = run.get(1..).unwrap_or_default().join(" ");
            let want = script_words(&format!(
                "test \"$(\"{program}\" {args})\" = \"{}\"",
                named(expected)
            ));
            problems.extend(differs(
                "version",
                &run_words(workflow, "version"),
                &want,
                6,
            ));
        }
        (Err(e), _) => problems.push(e),
        (Ok(_), None) => problems.push(format!("{DOC} has no expected output in step 6")),
    }
    let version_step = step(workflow, "version");
    for (key, want) in VERSION_ENV {
        if env(version_step, key) != Some(want) {
            problems.push(format!("the version step's `{key}` is not `{want}`"));
        }
    }
    problems
}
