//! The signing run of `artifacts.yml`: a run started by hand with the input `sign: true` signs
//! and checks the macOS programs and pkgs exactly as a push to main does, so the signing is
//! proved on a branch before it merges (docs/macos.md, "The programs' signature").
//!
//! - **One input.** The `workflow_dispatch` trigger has exactly the input [`INPUT`], of
//!   `type: boolean` and `default: false`, with a description and no other key: a run by hand
//!   that does not ask signs nothing, as before.
//! - **Where it signs.** The signing jobs (`sign-code`, `sign-pkg`, `check-pkg`) and the steps of
//!   `build-macos` that take or check the signed programs run under [`MACOS_SIGN_IF`] (held there
//!   by [`super::pkg_signing`] and [`super::code_signing`]): a push to main, or a signing run on a
//!   branch of this repository ([`ALLOWED`]). `attest` and `verify` stay under
//!   [`super::release::SIGN_IF`], a push to main alone, so a signing run attests nothing, and
//!   release.yml plans only the completion of a push run on main, so it releases nothing.
//! - **Refused by name elsewhere.** The first step of the job `version`, which every job needs,
//!   is [`REFUSE_RUN`] under [`REFUSE_IF`], the negation of [`ALLOWED`]: a signing run on a tag or
//!   in a fork's copy of the file fails by name instead of going green unsigned.
//! - **Nothing else reads the input.** Every scalar of the file that names `inputs.` is one of
//!   those two conditions, so no other job, step, `env:` or `with:` takes it.

use super::lint::{entries, entry, scalars};
use super::release::{job, steps, text_of};
use super::yaml::Node;

/// Where a signing run may sign: a branch of this repository. A macro so that [`MACOS_SIGN_IF`]
/// and [`REFUSE_IF`] are built from the one text.
macro_rules! allowed {
    () => {
        "github.repository == 'komira-ai/hostkeeper' && startsWith(github.ref, 'refs/heads/')"
    };
}

/// The condition [`MACOS_SIGN_IF`] and [`REFUSE_IF`] hold a signing run to.
pub const ALLOWED: &str = allowed!();

/// The input of `workflow_dispatch` that asks for a signing run.
pub const INPUT: &str = "sign";

/// The `if:` of the signing jobs and of the steps of `build-macos` that take or check the signed
/// programs: a push to main, or a signing run on a branch of this repository.
pub const MACOS_SIGN_IF: &str = concat!(
    "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main') || (github.event_name == 'workflow_dispatch' && inputs.sign && ",
    allowed!(),
    ") }}"
);

/// The `if:` of the refusal: a signing run anywhere [`ALLOWED`] does not hold.
pub const REFUSE_IF: &str = concat!(
    "${{ github.event_name == 'workflow_dispatch' && inputs.sign && !(",
    allowed!(),
    ") }}"
);

/// The refusal, by name, as the first step of the job `version`.
pub const REFUSE_RUN: &str = "echo \"::error::a signing run (sign: true) runs only on a branch of komira-ai/hostkeeper, not on $GITHUB_REF of $GITHUB_REPOSITORY\"; exit 1";

/// The problem of a dispatch trigger that is not the one input.
pub const INPUT_PROBLEM: &str = "artifacts.yml: workflow_dispatch must have exactly the input `sign`: `type: boolean`, `default: false` and a description";

/// The problem of a job `version` whose first step is not the refusal.
pub const REFUSE_PROBLEM: &str = "artifacts.yml: the first step of job version must be the refusal of a signing run outside a branch of this repository (REFUSE_RUN under REFUSE_IF)";

/// One line per rule of this module that `doc`, artifacts.yml, breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !one_input(doc) {
        problems.push(INPUT_PROBLEM.to_owned());
    }
    if !refuses_first(doc) {
        problems.push(REFUSE_PROBLEM.to_owned());
    }
    let mut all = Vec::new();
    scalars(doc, &mut all);
    for text in all {
        if text.contains("inputs.") && text != MACOS_SIGN_IF && text != REFUSE_IF {
            problems.push(format!(
                "artifacts.yml: `{text}` reads an input; only the signing conditions may"
            ));
        }
    }
    problems
}

/// Whether the dispatch trigger has exactly [`INPUT`], a boolean defaulting to false with a
/// description, and nothing else.
fn one_input(doc: &Node) -> bool {
    let dispatch = entry(doc, "on").and_then(|on| entry(on, "workflow_dispatch"));
    let Some(dispatch) = dispatch else {
        return false;
    };
    let keys: Vec<&str> = entries(dispatch)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    let Some(inputs) = entry(dispatch, "inputs") else {
        return false;
    };
    let names: Vec<&str> = entries(inputs)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    let Some(sign) = entry(inputs, INPUT) else {
        return false;
    };
    let mut fields: Vec<&str> = entries(sign)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    fields.sort_unstable();
    keys == ["inputs"]
        && names == [INPUT]
        && fields == ["default", "description", "type"]
        && text_of(Some(sign), "type") == Some("boolean")
        && text_of(Some(sign), "default") == Some("false")
        && text_of(Some(sign), "description").is_some_and(|d| !d.trim().is_empty())
}

/// Whether the first step of the job `version` is [`REFUSE_RUN`] under [`REFUSE_IF`], with a
/// name and no other key.
fn refuses_first(doc: &Node) -> bool {
    let Some(first) = steps(job(doc, "version")).first() else {
        return false;
    };
    let mut keys: Vec<&str> = entries(first)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    keys.sort_unstable();
    keys == ["if", "name", "run"]
        && text_of(Some(first), "if") == Some(REFUSE_IF)
        && text_of(Some(first), "run") == Some(REFUSE_RUN)
}
