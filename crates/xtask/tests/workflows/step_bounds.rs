//! The rule of the workflows that reach the build farm ([`FILES`]): every job has a
//! `timeout-minutes` of its own, and every step of it one too, a whole number of minutes from 1
//! and under the job's. A connection to the farm can die without a word, and a step without a
//! bound of its own waits on it until the job's limit; with one, a step that hangs ends at its
//! own bound. Each bound is sized from the longest successful run of its step (docs/farm.md,
//! "Time limits"); the numbers themselves are reviewed, not checked here.
//!
//! `step_bounds_tests.rs` shows each defect refused.

use super::lint::{entries, entry};
use super::yaml::{Node, Value};

/// The workflows the rule holds: the farm lane and the macOS build on a farm Mac.
pub const FILES: [&str; 2] = [super::farm::FILE, super::mac_farm::FILE];

/// A `timeout-minutes` value read as a whole number of minutes from 1, `None` when it is not one
/// (an expression, a fraction, 0, text).
fn minutes(node: &Node) -> Option<u32> {
    let text = entry(node, "timeout-minutes")?.as_str()?;
    text.parse().ok().filter(|m| *m > 0)
}

/// How a step is named in a problem: its `name:`, else its `run:` or `uses:` up to the end of its
/// first line.
fn label(step: &Node) -> &str {
    ["name", "run", "uses"]
        .iter()
        .find_map(|key| entry(step, key).and_then(Node::as_str))
        .and_then(|text| text.lines().next())
        .unwrap_or_default()
}

/// One line per job of `doc` without a bound of its own, and per step without one or with one
/// that is not under its job's.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, job) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        let bound = minutes(job);
        if bound.is_none() {
            problems.push(format!(
                "job {name}: no `timeout-minutes` of its own, a whole number of minutes from 1"
            ));
        }
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(steps)) => &steps[..],
            _ => &[],
        };
        for (at, step) in steps.iter().enumerate() {
            let who = format!("job {name}: step {} (`{}`)", at + 1, label(step));
            match (minutes(step), bound) {
                (None, _) => problems.push(format!(
                    "{who} has no `timeout-minutes`, a whole number of minutes from 1"
                )),
                (Some(step), Some(job)) if step >= job => problems.push(format!(
                    "{who}: `timeout-minutes: {step}` is not under the job's {job}"
                )),
                _ => {}
            }
        }
    }
    problems
}
