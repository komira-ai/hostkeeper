//! Where a workflow stands for one commit, from its runs.

use super::contract::{self, Commit};
use super::ports::Run;

/// Where a workflow stands for one commit: its latest push run on `main` for that commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunState {
    /// No push run on `main` for the commit.
    Missing,
    /// The latest such run has not completed.
    Pending(u64),
    /// The latest such run succeeded.
    Succeeded(u64),
    /// The latest such run failed (`failure`, `timed_out` or `startup_failure`).
    Failed(u64),
    /// The latest such run completed otherwise (`cancelled`, `skipped`, ...): not green, not
    /// red.
    Other(u64, String),
}

/// The state of a workflow for `commit` from its `runs`: only push runs on `main` whose head is
/// exactly `commit` count, and of those the latest (highest id). A pull-request run, a run on
/// another branch and a run for another commit never count.
pub(crate) fn state(runs: &[Run], commit: &Commit) -> RunState {
    let latest = runs
        .iter()
        .filter(|r| {
            (r.event == "push") & (r.branch == contract::MAIN) & (r.head_sha == commit.as_str())
        })
        .max_by_key(|r| r.id);
    let Some(run) = latest else {
        return RunState::Missing;
    };
    match (run.status.as_str(), run.conclusion.as_deref()) {
        ("completed", Some("success")) => RunState::Succeeded(run.id),
        ("completed", Some("failure" | "timed_out" | "startup_failure")) => {
            RunState::Failed(run.id)
        }
        ("completed", other) => RunState::Other(run.id, other.unwrap_or("none").to_owned()),
        _ => RunState::Pending(run.id),
    }
}
