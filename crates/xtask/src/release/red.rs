//! `release red`: the alert of a red run of a suite that holds no release. A release waits only
//! for the unit tests and the build ([`REQUIRED_WORKFLOWS`](super::contract::REQUIRED_WORKFLOWS));
//! the privileged tests run beside it and the Mac's beta stage, the install checks and the
//! promotion to stable run after its deploy. When a run of one of them on `main` fails, this verb,
//! run by `release-red.yml` on the completion of each run of `release.yml` and `privileged.yml`,
//! opens one issue per workflow titled `RELEASE-RED: hostkeeper <workflow>: ...` (or brings the
//! open one up to date) that names the run, its commit and its failed jobs, and says whether the
//! deploy went out. A later run of that workflow that succeeds on a commit that has the red one
//! in its first-parent history closes it; a successful release run counts only when it released
//! something (it ran a `publish` job), since a run that released nothing tested nothing.
//!
//! The verb reads the run back from GitHub by its workflow, commit and id rather than trusting
//! what started it, and acts only on a run of `main`. A run that ended otherwise (cancelled,
//! skipped) changes nothing.

use super::Error;
use super::contract::{Commit, DEPLOY_JOBS, MAIN, PRIVILEGED_WORKFLOW, RELEASE_WORKFLOW, Repo};
use super::plan::base_name;
use super::ports::{Issue, Job, Ports, Run};

/// What `release red` is asked: the run that completed.
pub(crate) struct Request {
    pub(crate) repo: Repo,
    /// The run's workflow file, by its name or its path (`.github/workflows/release.yml`).
    pub(crate) workflow: String,
    pub(crate) run_id: u64,
    /// The run's head commit.
    pub(crate) commit: Commit,
}

/// The first words of the title of every issue this verb opens.
pub(crate) const PREFIX: &str = "RELEASE-RED: hostkeeper";

/// The conclusions of a red run.
const RED: [&str; 3] = ["failure", "timed_out", "startup_failure"];

/// The line of an issue's body that records the commit of the red run, which decides whether a
/// later success closes it.
const COMMIT_LINE: &str = "Red commit: ";

/// A workflow this verb reports on: its file, the title of its one issue, and what its red run
/// means for a release.
struct Watched {
    file: &'static str,
    title: &'static str,
    meaning: fn(&[Job]) -> String,
}

/// The workflows this verb reports on.
const WATCHED: [Watched; 2] = [
    Watched {
        file: RELEASE_WORKFLOW,
        title: "RELEASE-RED: hostkeeper release: a job of the release run failed",
        meaning: release_meaning,
    },
    Watched {
        file: PRIVILEGED_WORKFLOW,
        title: "RELEASE-RED: hostkeeper privileged: the device tests failed on main",
        meaning: privileged_meaning,
    },
];

/// Reports the completed run of the request: see the module.
pub(crate) fn red(ports: &Ports<'_>, request: &Request) -> Result<Vec<String>, Error> {
    let file = request
        .workflow
        .rsplit('/')
        .next()
        .unwrap_or(&request.workflow);
    let watched = WATCHED.iter().find(|w| w.file == file).ok_or_else(|| {
        Error::Failed(format!(
            "release red reports on {RELEASE_WORKFLOW} and {PRIVILEGED_WORKFLOW}, not {file}"
        ))
    })?;
    let runs = ports.runs.runs(&request.repo, file, &request.commit)?;
    let run = runs
        .iter()
        .find(|r| r.id == request.run_id)
        .ok_or_else(|| {
            Error::Failed(format!(
                "{file} has no run {} for {}",
                request.run_id, request.commit
            ))
        })?;
    let label = format!("{file} run {} of {}", run.id, request.commit);
    if run.branch != MAIN {
        return Ok(vec![format!(
            "{label}: a run of {}, not {MAIN}: nothing to report",
            run.branch
        )]);
    }
    let conclusion = match (run.status.as_str(), run.conclusion.as_deref()) {
        ("completed", Some(conclusion)) => conclusion,
        (status, _) => {
            return Err(Error::Failed(format!(
                "{label} has not completed ({status})"
            )));
        }
    };
    let jobs = ports.runs.jobs(&request.repo, run.id)?;
    let issues = ports.runs.open_issues(&request.repo)?;
    let open = issues.iter().find(|i| i.title == watched.title);
    if RED.contains(&conclusion) {
        let body = red_body(&request.repo, run, &request.commit, watched, &jobs);
        return report(ports, &request.repo, watched.title, open, &body, &label);
    }
    let released = jobs
        .iter()
        .any(|j| base_name(&j.name) == "publish" && j.conclusion.as_deref() == Some("success"));
    match (conclusion, open) {
        ("success", None) => Ok(vec![format!(
            "{label} succeeded; no {PREFIX} issue of {file} is open"
        )]),
        ("success", Some(_)) if file == RELEASE_WORKFLOW && !released => Ok(vec![format!(
            "{label} succeeded but released nothing: the open issue stays open"
        )]),
        ("success", Some(issue)) => close(ports, request, issue, &label),
        (other, _) => Ok(vec![format!("{label} ended {other}: nothing to report")]),
    }
}

/// Opens the issue titled `title` with `body`, or brings the open one to `body`.
fn report(
    ports: &Ports<'_>,
    repo: &Repo,
    title: &str,
    open: Option<&Issue>,
    body: &str,
    label: &str,
) -> Result<Vec<String>, Error> {
    let number = match open {
        None => ports.runs.open_issue(repo, title, body)?,
        Some(issue) if issue.body == body => issue.number,
        Some(issue) => {
            ports.runs.edit_issue(repo, issue.number, body)?;
            issue.number
        }
    };
    Ok(vec![format!("{label} is red: issue #{number}, {title}")])
}

/// Closes `issue` when the red commit it records is the request's commit or in its first-parent
/// history (or it records none); otherwise the success is of an older commit and it stays open.
fn close(
    ports: &Ports<'_>,
    request: &Request,
    issue: &Issue,
    label: &str,
) -> Result<Vec<String>, Error> {
    let recorded = issue
        .body
        .lines()
        .find_map(|l| l.strip_prefix(COMMIT_LINE))
        .map(str::trim);
    if let Some(red) = recorded {
        let history = ports.git.first_parents(&request.commit)?;
        if !history.iter().any(|c| c.as_str() == red) {
            return Ok(vec![format!(
                "{label} succeeded, but {red} of issue #{} is not in its history: it stays open",
                issue.number
            )]);
        }
    }
    let comment = format!(
        "Run {} succeeded on {}: {}",
        request.run_id,
        request.commit,
        run_url(&request.repo, request.run_id)
    );
    ports
        .runs
        .close_issue(&request.repo, issue.number, &comment)?;
    Ok(vec![format!(
        "{label} succeeded: closed issue #{}",
        issue.number
    )])
}

/// The address of the run `id` of `repo`.
fn run_url(repo: &Repo, id: u64) -> String {
    format!("https://github.com/{repo}/actions/runs/{id}")
}

/// The body of the issue of the red run `run`.
fn red_body(repo: &Repo, run: &Run, commit: &Commit, watched: &Watched, jobs: &[Job]) -> String {
    let failed = red_jobs(jobs);
    let listed = if failed.is_empty() {
        "- (no job of the run failed: the run itself did not start or did not finish)".to_owned()
    } else {
        failed
            .iter()
            .map(|j| format!("- {j}"))
            .collect::<Vec<String>>()
            .join("\n")
    };
    format!(
        "{PREFIX} {file} run {id} ended {conclusion} on {commit}.\n\n{url}\n\n\
         The jobs that failed:\n{listed}\n\n{meaning}\n\n\
         The next run of {file} on {MAIN} that succeeds closes this issue.\n\n{COMMIT_LINE}{commit}\n",
        file = watched.file,
        id = run.id,
        conclusion = run.conclusion.as_deref().unwrap_or("none"),
        url = run_url(repo, run.id),
        meaning = (watched.meaning)(jobs),
    )
}

/// The names of the jobs of a run that failed or timed out, in the run's order.
fn red_jobs(jobs: &[Job]) -> Vec<&str> {
    jobs.iter()
        .filter(|j| matches!(j.conclusion.as_deref(), Some("failure" | "timed_out")))
        .map(|j| j.name.as_str())
        .collect()
}

/// What a red release run means: whether its deploy went out.
fn release_meaning(jobs: &[Job]) -> String {
    let deploy: Vec<&str> = red_jobs(jobs)
        .into_iter()
        .filter(|name| DEPLOY_JOBS.contains(&base_name(name)))
        .collect();
    if deploy.is_empty() {
        "The deploy went out: no job of the deploy failed. The failed jobs run beside or after \
         it (the Mac's beta stage, the install checks, the promotion to stable) and hold no \
         release; fix forward."
            .to_owned()
    } else {
        format!(
            "The deploy did not complete: {} failed. The next release run releases this release \
             again (`release plan`); fix forward or re-run the failed jobs.",
            deploy.join(", ")
        )
    }
}

/// What a red privileged run means.
fn privileged_meaning(_jobs: &[Job]) -> String {
    "No release waits for the privileged tests: releases go on. Fix forward in a new pull request."
        .to_owned()
}

#[cfg(test)]
mod tests;
