//! The plan's pass-over of a commit not yet released whose own verify job failed in each of its
//! latest three release runs: passed over, with the runs named, while the newer commits are
//! released; released again while fewer failed in a row, or when a run between ended the job
//! otherwise; never moved by a run without the job, a run of another branch, or the verify job
//! of another commit or version; each run's jobs read once; and a GitHub that cannot list a
//! run's jobs fails the plan. Each by its full lines.

use std::cell::Cell;

use super::*;
use crate::release::fakes::{FakeHistory, FakeRuns, Fakes};
use crate::release::ports::{Adapters, GitHubRuns, Issue, Job, PortError, Run};

fn repo() -> Repo {
    "komira-ai/hostkeeper".parse().expect("a repository")
}

fn request(commit: &Commit) -> Request {
    Request {
        repo: repo(),
        commit: commit.clone(),
        version_file: "VERSION".to_owned(),
        freeze: String::new(),
        output: None,
    }
}

/// A root commit (version file `0.1`) released as v0.1.1, then `a` (0.1.2) and `b` (0.1.3), each
/// with a successful push run of every required workflow; the fakes and the three commits.
fn two_after_a_release() -> (Fakes, Commit, Commit, Commit) {
    let mut h = FakeHistory::default();
    let r = h.commit(
        None,
        &[
            ("VERSION", Some("0.1\n")),
            ("src/lib.rs", Some("fn a() {}")),
        ],
    );
    let a = h.commit(Some(&r), &[("src/lib.rs", Some("fn b() {}"))]);
    let b = h.commit(Some(&a), &[("src/lib.rs", Some("fn c() {}"))]);
    let fakes = Fakes::over(h);
    fakes.store.insert(Release {
        tag: "v0.1.1".to_owned(),
        commit: r.to_string(),
        draft: false,
        assets: Vec::new(),
    });
    for commit in [&a, &b] {
        for workflow in REQUIRED_WORKFLOWS {
            fakes.runs.push(workflow, commit, "success");
        }
    }
    (fakes, r, a, b)
}

/// The verify job of `version` of `commit` that ended `conclusion` (none: not ended).
fn verify(version: &str, commit: &Commit, conclusion: Option<&str>) -> Job {
    Job {
        name: format!("verify ({version}, v{version}, {commit})"),
        conclusion: conclusion.map(str::to_owned),
    }
}

fn plan_job() -> Job {
    Job {
        name: "plan".to_owned(),
        conclusion: Some("success".to_owned()),
    }
}

/// A completed run of release.yml on `branch` for the head `head`, with the jobs `jobs`; its id.
fn release_run(fakes: &Fakes, head: &Commit, branch: &str, jobs: Vec<Job>) -> u64 {
    let id = fakes
        .runs
        .add("release.yml", head, "workflow_run", branch, Some("failure"));
    fakes.runs.jobs.borrow_mut().insert(id, jobs);
    id
}

/// A release run on main for the head `b` that verified `a` (0.1.2), whose verify ended
/// `a_ended`, and `b` (0.1.3), whose verify ended `b_ended`.
fn both(fakes: &Fakes, a: &Commit, b: &Commit, a_ended: &str, b_ended: &str) -> u64 {
    release_run(
        fakes,
        b,
        "main",
        vec![
            plan_job(),
            verify("0.1.2", a, Some(a_ended)),
            verify("0.1.3", b, Some(b_ended)),
        ],
    )
}

fn matrix(entries: &[(&str, &Commit)]) -> String {
    let entries: Vec<String> = entries
        .iter()
        .map(|(v, c)| format!("{{\"version\":\"{v}\",\"tag\":\"v{v}\",\"commit\":\"{c}\"}}"))
        .collect();
    format!("matrix={{\"include\":[{}]}}", entries.join(","))
}

fn passed_over_line(commit: &Commit, version: &str, runs: [u64; 3]) -> String {
    format!(
        "no release of {commit} ({version}): passed over: its verify job failed in each of its \
         last 3 release runs ({}, {}, {}, the latest first), a failure every attempt meets; the \
         newer commits are released without it",
        runs[0], runs[1], runs[2]
    )
}

fn both_released(a: &Commit, b: &Commit) -> Vec<String> {
    vec![
        format!("release v0.1.2 of {a}"),
        format!("release v0.1.3 of {b}"),
        "release=true".to_owned(),
        matrix(&[("0.1.2", a), ("0.1.3", b)]),
    ]
}

#[test]
fn a_commit_whose_verify_failed_in_three_release_runs_in_a_row_is_passed_over_for_the_newer() {
    // Catches: a commit whose verify can never pass (its build lacks a file the workflow names)
    // planned first in every release run for ever, its failure cancelling the newer commit's
    // verify each time, so no release ever happens again.
    let (fakes, _, a, b) = two_after_a_release();
    let first = both(&fakes, &a, &b, "failure", "cancelled");
    let second = both(&fakes, &a, &b, "failure", "cancelled");
    let third = both(&fakes, &a, &b, "failure", "cancelled");
    assert_eq!(
        plan(&fakes.ports(), &request(&b)),
        Ok(vec![
            passed_over_line(&a, "0.1.2", [third, second, first]),
            format!("release v0.1.3 of {b}"),
            "release=true".to_owned(),
            matrix(&[("0.1.3", &b)]),
        ])
    );
}

#[test]
fn a_commit_whose_verify_failed_in_fewer_than_three_runs_in_a_row_is_released_again() {
    // Catches: a commit passed over at its first or second failure, which a passing re-run
    // (an attestation service down for one run) would have released under its own number.
    let (fakes, _, a, b) = two_after_a_release();
    assert_eq!(
        plan(&fakes.ports(), &request(&b)),
        Ok(both_released(&a, &b))
    );
    both(&fakes, &a, &b, "failure", "cancelled");
    both(&fakes, &a, &b, "failure", "cancelled");
    assert_eq!(
        plan(&fakes.ports(), &request(&b)),
        Ok(both_released(&a, &b))
    );
}

#[test]
fn a_verify_job_ended_otherwise_between_the_failures_resets_the_count() {
    // Catches: failures counted across a verify that was cancelled, succeeded, or had not ended,
    // so a commit whose verify passed once (the cause fixed, the publish then failing) is passed
    // over, or one whose job never ran to its end is taken for one that cannot pass.
    for between in [Some("cancelled"), Some("success"), None] {
        let (fakes, _, a, b) = two_after_a_release();
        both(&fakes, &a, &b, "failure", "cancelled");
        release_run(&fakes, &b, "main", vec![verify("0.1.2", &a, between)]);
        both(&fakes, &a, &b, "failure", "cancelled");
        both(&fakes, &a, &b, "failure", "cancelled");
        assert_eq!(
            plan(&fakes.ports(), &request(&b)),
            Ok(both_released(&a, &b)),
            "a verify that ended {between:?} between"
        );
    }
}

#[test]
fn a_run_without_the_verify_job_is_passed_by_and_does_not_reset_the_count() {
    // Catches: a run that planned nothing for the commit (one replaced while it waited, the
    // current run itself, a run of only the plan) taken as a verify that did not fail, so the
    // count never reaches three while such runs come between.
    let (fakes, _, a, b) = two_after_a_release();
    let first = both(&fakes, &a, &b, "failure", "cancelled");
    release_run(&fakes, &b, "main", vec![plan_job()]);
    let second = both(&fakes, &a, &b, "failure", "cancelled");
    release_run(&fakes, &b, "main", Vec::new());
    let third = both(&fakes, &a, &b, "failure", "cancelled");
    fakes
        .runs
        .add("release.yml", &b, "workflow_run", "main", None);
    assert_eq!(
        plan(&fakes.ports(), &request(&b)),
        Ok(vec![
            passed_over_line(&a, "0.1.2", [third, second, first]),
            format!("release v0.1.3 of {b}"),
            "release=true".to_owned(),
            matrix(&[("0.1.3", &b)]),
        ])
    );
}

#[test]
fn a_run_of_another_branch_or_the_verify_of_another_version_or_commit_never_counts() {
    // Catches: a run of a branch named otherwise, or a failed verify of another number of the
    // commit or of another commit under its number, taken for the commit's own failures.
    let (fakes, r, a, b) = two_after_a_release();
    for _ in 0..3 {
        release_run(
            &fakes,
            &b,
            "feature",
            vec![verify("0.1.2", &a, Some("failure"))],
        );
        release_run(
            &fakes,
            &b,
            "main",
            vec![
                verify("0.1.9", &a, Some("failure")),
                verify("0.1.2", &r, Some("failure")),
            ],
        );
    }
    assert_eq!(
        plan(&fakes.ports(), &request(&b)),
        Ok(both_released(&a, &b))
    );
}

#[test]
fn a_commit_passed_over_is_left_out_of_the_matrix_and_alone_releases_nothing() {
    // Catches: a commit passed over still put in the matrix when no newer commit is released,
    // or the plan failing when every commit of the window is passed over.
    let (fakes, _, a, b) = two_after_a_release();
    let runs: Vec<u64> = (0..3)
        .map(|_| both(&fakes, &a, &b, "success", "failure"))
        .collect();
    let mut lines = vec![
        format!("release v0.1.2 of {a}"),
        passed_over_line(&b, "0.1.3", [runs[2], runs[1], runs[0]]),
    ];
    lines.extend(["release=true".to_owned(), matrix(&[("0.1.2", &a)])]);
    assert_eq!(plan(&fakes.ports(), &request(&b)), Ok(lines));
    let (fakes, _, a, b) = two_after_a_release();
    let runs: Vec<u64> = (0..3)
        .map(|_| both(&fakes, &a, &b, "failure", "cancelled"))
        .collect();
    assert_eq!(
        plan(&fakes.ports(), &request(&a)),
        Ok(vec![
            passed_over_line(&a, "0.1.2", [runs[2], runs[1], runs[0]]),
            "release=false".to_owned(),
            matrix(&[]),
        ])
    );
}

/// The runs of `runs`, counting each listing of a run's jobs, or failing every one.
struct Counted<'a> {
    runs: &'a FakeRuns,
    listed: Cell<usize>,
    down: bool,
}

impl GitHubRuns for Counted<'_> {
    fn runs(&self, repo: &Repo, workflow: &str, commit: &Commit) -> Result<Vec<Run>, PortError> {
        self.runs.runs(repo, workflow, commit)
    }
    fn jobs(&self, repo: &Repo, run: u64) -> Result<Vec<Job>, PortError> {
        self.listed.set(self.listed.get() + 1);
        if self.down {
            return Err(PortError::new(format!("gh jobs {run}"), "HTTP 502"));
        }
        self.runs.jobs(repo, run)
    }
    fn on_main(&self, repo: &Repo, commit: &Commit) -> Result<bool, PortError> {
        self.runs.on_main(repo, commit)
    }
    fn open_issues(&self, repo: &Repo) -> Result<Vec<Issue>, PortError> {
        self.runs.open_issues(repo)
    }
    fn open_issue(&self, repo: &Repo, title: &str, body: &str) -> Result<u64, PortError> {
        self.runs.open_issue(repo, title, body)
    }
    fn edit_issue(&self, repo: &Repo, number: u64, body: &str) -> Result<(), PortError> {
        self.runs.edit_issue(repo, number, body)
    }
    fn close_issue(&self, repo: &Repo, number: u64, comment: &str) -> Result<(), PortError> {
        self.runs.close_issue(repo, number, comment)
    }
}

#[test]
fn each_release_runs_jobs_are_listed_once_per_plan() {
    // Catches: the jobs of every release run listed again for each commit of the window, which
    // multiplies the calls of a plan by the commits waiting while main is held.
    let (fakes, _, a, b) = two_after_a_release();
    both(&fakes, &a, &b, "failure", "cancelled");
    both(&fakes, &a, &b, "failure", "cancelled");
    let counted = Counted {
        runs: &fakes.runs,
        listed: Cell::new(0),
        down: false,
    };
    let ports = Ports {
        runs: &counted,
        ..fakes.ports()
    };
    assert_eq!(plan(&ports, &request(&b)), Ok(both_released(&a, &b)));
    assert_eq!(counted.listed.get(), 2);
}

#[test]
fn a_github_that_cannot_list_a_runs_jobs_fails_the_plan_of_a_commit_to_release() {
    // Catches: a run whose jobs could not be read taken for a run without the commit's verify,
    // which would release a commit the plan should pass over, or pass over one by error.
    let (fakes, _, a, b) = two_after_a_release();
    let id = both(&fakes, &a, &b, "failure", "cancelled");
    let counted = Counted {
        runs: &fakes.runs,
        listed: Cell::new(0),
        down: true,
    };
    let ports = Ports {
        runs: &counted,
        ..fakes.ports()
    };
    assert_eq!(
        plan(&ports, &request(&b)),
        Err(Error::Port(PortError::new(
            format!("gh jobs {id}"),
            "HTTP 502"
        )))
    );
}
