//! `release locate`: a commit is released only from `main`, after each required workflow has a
//! successful push run for it. The verb refuses any other commit and prints the id of the run
//! that built the assets.

use std::path::Path;

use super::contract::{Commit, MAIN, REQUIRED_WORKFLOWS, Repo};
use super::ports::GitHubRuns;
use super::{Error, RunState, answer, state};

/// Checks that `commit` is on `main` and that each of [`REQUIRED_WORKFLOWS`] has a successful
/// push run for it; prints that, and appends `run-id=<id of the artifacts run>` to `output`.
/// Every workflow without such a run is a line of the refusal.
pub(crate) fn locate(
    runs: &dyn GitHubRuns,
    repo: &Repo,
    commit: &Commit,
    output: Option<&Path>,
) -> Result<Vec<String>, Error> {
    if !runs.on_main(repo, commit)? {
        return Err(Error::Failed(format!(
            "{commit} is not on {MAIN}: a release is cut only from a commit on {MAIN}"
        )));
    }
    let mut ids = [0_u64; REQUIRED_WORKFLOWS.len()];
    let mut problems = Vec::new();
    for (slot, workflow) in ids.iter_mut().zip(REQUIRED_WORKFLOWS) {
        let words = match state(&runs.runs(repo, workflow, commit)?, commit) {
            RunState::Succeeded(id) => {
                *slot = id;
                continue;
            }
            RunState::Missing => "there is no such run".to_owned(),
            RunState::Pending(id) => format!("run {id} has not completed"),
            RunState::Failed(id) => format!("run {id} failed"),
            RunState::Other(id, conclusion) => format!("run {id} ended {conclusion}"),
        };
        problems.push(format!(
            "no successful push run of {workflow} on {MAIN} for {commit}: {words}"
        ));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    let [ci, privileged, artifacts] = ids;
    let mut lines = vec![format!(
        "{commit} is on {MAIN}; ci.yml run {ci}, privileged.yml run {privileged} and artifacts.yml run {artifacts} succeeded"
    )];
    lines.extend(answer(output, vec![format!("run-id={artifacts}")])?);
    Ok(lines)
}

#[cfg(test)]
mod tests;
