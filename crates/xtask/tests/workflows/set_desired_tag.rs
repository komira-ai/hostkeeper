//! The rules of the tailnet tag of `set-desired.yml`: the tag the join gives the runner's node is
//! the caller's input [`TAG_INPUT`], `tag:hk-ci` unless the caller names another, and the check
//! step [`TAG_STEP`] refuses any value that is not `tag:` and a Tailscale tag name before the
//! join reads it. [`set_desired`](super::set_desired) applies them.
//!
//! [`problems`] refuses the workflow when:
//! - it has no input [`TAG_INPUT`] of `type: string` with `default: tag:hk-ci` ([`TAG_DEFAULT`]);
//! - the check step's `env` does not give [`TAG_ENV`] exactly, or its script is not one
//!   line [`TAG_COMMAND`], a space and one `'`-quoted program, and nothing else (so nothing can
//!   run on past it, as `|| true` would);
//! - the check step comes after the join step [`JOIN_STEP`];
//! - a step other than those two, or the `env` of the workflow or of the job, names
//!   [`TAG_VALUE`]: the join is the one step that uses the tag, and only once it is checked.
//!
//! The check step is one of [`CHECKED`](super::set_desired::CHECKED), so it has no `if:` and no
//! `continue-on-error`, and one of [`ORDERED`](super::set_desired::ORDERED), so it is there
//! exactly once. What its program accepts is shown by the workflow lint's tests, which run the
//! program of the real file with jaq.

use super::lint::{entry, scalars};
use super::set_desired::{JOB, JOIN_STEP, label, text};
use super::yaml::{Node, Value};

/// The input that names the tag of the runner's node.
pub const TAG_INPUT: &str = "tailnet_tag";
/// Its default, the tag of docs/deploy/ci-client.md's ACL: every caller that names no tag joins
/// as before.
pub const TAG_DEFAULT: &str = "tag:hk-ci";
/// The input as a step names it.
pub const TAG_VALUE: &str = "${{ inputs.tailnet_tag }}";
/// The step that checks the tag, before the join reads it.
pub const TAG_STEP: &str = "tailnet-tag";
/// What the check step's `env` gives, exactly: the caller's input, never written into the script.
pub const TAG_ENV: (&str, &str) = ("TAILNET_TAG", TAG_VALUE);
/// The check step's one command, up to its program.
pub const TAG_COMMAND: &str = "jq --null-input --arg tag \"$TAILNET_TAG\"";

/// One line per problem of the tailnet tag of the CI client workflow `doc`; `steps` are its one
/// job's steps.
pub fn problems(doc: &Node, steps: &[Node]) -> Vec<String> {
    let mut problems = Vec::new();
    let input = entry(doc, "on")
        .and_then(|on| entry(on, "workflow_call"))
        .and_then(|c| entry(c, "inputs"))
        .and_then(|i| entry(i, TAG_INPUT));
    if input.and_then(|i| text(i, "type")) != Some("string")
        || input.and_then(|i| text(i, "default")) != Some(TAG_DEFAULT)
    {
        problems.push(format!(
            "no input `{TAG_INPUT}` of `type: string` with `default: {TAG_DEFAULT}`"
        ));
    }
    let job = entry(doc, "jobs").and_then(|jobs| entry(jobs, JOB));
    for (owner, node) in [("workflow", Some(doc)), ("job", job)] {
        if node.and_then(|n| entry(n, "env")).is_some_and(names_tag) {
            problems.push(format!("the {owner}'s `env` names `{TAG_VALUE}`"));
        }
    }
    let at = |id: &str| steps.iter().position(|s| text(s, "id") == Some(id));
    for (i, step) in steps.iter().enumerate() {
        let id = text(step, "id");
        if id != Some(TAG_STEP) && id != Some(JOIN_STEP) && names_tag(step) {
            problems.push(format!(
                "{} names `{TAG_VALUE}`: only the join reads the tag, once `{TAG_STEP}` checked it",
                label(i, step)
            ));
        }
    }
    let Some(check) = at(TAG_STEP) else {
        return problems;
    };
    let step = &steps[check];
    let who = format!("the tag step `{TAG_STEP}` (step {})", check + 1);
    let (key, want) = TAG_ENV;
    if entry(step, "env").and_then(|e| text(e, key)) != Some(want) {
        problems.push(format!("{who}'s `{key}` is not `{want}`"));
    }
    if tag_program_of(step).is_none() {
        problems.push(format!(
            "{who} does not run `{TAG_COMMAND} '<program>'` as its one command"
        ));
    }
    if let Some(join) = at(JOIN_STEP).filter(|&j| j < check) {
        problems.push(format!(
            "{who} comes after the join step `{JOIN_STEP}` (step {}), which reads the tag",
            join + 1
        ));
    }
    problems
}

/// Whether any scalar of `node` names the tag input.
fn names_tag(node: &Node) -> bool {
    let mut values = Vec::new();
    scalars(node, &mut values);
    values.iter().any(|v| v.contains(TAG_VALUE))
}

/// The program of the check step `step`: its script is one line, [`TAG_COMMAND`], a space and the
/// program in `'`, with no `'` in it, and nothing after it.
fn tag_program_of(step: &Node) -> Option<&str> {
    let run = text(step, "run")?;
    let lines: Vec<&str> = run.lines().filter(|l| !l.trim().is_empty()).collect();
    let [line] = lines.as_slice() else {
        return None;
    };
    let program = line
        .trim()
        .strip_prefix(TAG_COMMAND)?
        .strip_prefix(" '")?
        .strip_suffix('\'')?;
    (!program.contains('\'')).then_some(program)
}

/// The check step's program in the workflow `doc`.
pub fn tag_program(doc: &Node) -> Option<&str> {
    let steps = entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, JOB))
        .and_then(|job| entry(job, "steps"));
    match steps.map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps
            .iter()
            .find(|s| text(s, "id") == Some(TAG_STEP))
            .and_then(tag_program_of),
        _ => None,
    }
}
