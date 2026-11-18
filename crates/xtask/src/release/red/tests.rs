//! `release red`: a red run of `release.yml` or `privileged.yml` on `main` opens one issue per
//! workflow (or brings it up to date) and says whether the deploy went out; a later success that
//! has the red commit in its history closes it; everything else changes nothing. Each issue body
//! is asserted whole.

use super::*;
use crate::release::fakes::{FakeHistory, Fakes};
use crate::release::ports::{Adapters, GitHubRuns, PortError};

const RELEASE_TITLE: &str = "RELEASE-RED: hostkeeper release: a job of the release run failed";
const PRIVILEGED_TITLE: &str =
    "RELEASE-RED: hostkeeper privileged: the device tests failed on main";

fn repo() -> Repo {
    "komira-ai/hostkeeper".parse().expect("a repository")
}

/// A history of three commits, oldest first, and the fakes over it.
fn three() -> (Fakes, [Commit; 3]) {
    let mut h = FakeHistory::default();
    let a = h.commit(None, &[("src/lib.rs", Some("1"))]);
    let b = h.commit(Some(&a), &[("src/lib.rs", Some("2"))]);
    let c = h.commit(Some(&b), &[("src/lib.rs", Some("3"))]);
    (Fakes::over(h), [a, b, c])
}

fn request(workflow: &str, run_id: u64, commit: &Commit) -> Request {
    Request {
        repo: repo(),
        workflow: workflow.to_owned(),
        run_id,
        commit: commit.clone(),
    }
}

/// A run of `workflow` on main for `commit` that ended `conclusion`, with `jobs` (name, end).
fn run_of(
    fakes: &Fakes,
    workflow: &str,
    commit: &Commit,
    conclusion: Option<&str>,
    jobs: &[(&str, &str)],
) -> u64 {
    let id = fakes
        .runs
        .add(workflow, commit, "workflow_run", "main", conclusion);
    let jobs = jobs
        .iter()
        .map(|(name, end)| Job {
            name: (*name).to_owned(),
            conclusion: Some((*end).to_owned()),
        })
        .collect();
    fakes.runs.jobs.borrow_mut().insert(id, jobs);
    id
}

fn red_of(fakes: &Fakes, request: &Request) -> Result<Vec<String>, Error> {
    red(&fakes.ports(), request)
}

/// The jobs of a release run that deployed, then failed in the Mac's beta stage and an install
/// check.
const DEPLOYED_THEN_RED: [(&str, &str); 6] = [
    ("plan", "success"),
    ("publish (0.1.4, v0.1.4, c)", "success"),
    ("set-desired / set-desired", "success"),
    ("macos-test", "failure"),
    ("check-apt (hostkeeper, arm64)", "timed_out"),
    ("stable-apt-read", "skipped"),
];

#[test]
fn a_release_run_red_after_the_deploy_opens_one_issue_that_says_the_deploy_went_out() {
    // Catches: no alert for a red beta stage, an alert that does not name the failed jobs or
    // the run, or one that says the deploy failed when it went out.
    let (fakes, [_, _, c]) = three();
    let id = run_of(
        &fakes,
        "release.yml",
        &c,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", id, &c)),
        Ok(vec![format!(
            "release.yml run {id} of {c} is red: issue #1, {RELEASE_TITLE}"
        )])
    );
    let issues = fakes.runs.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].title, RELEASE_TITLE);
    assert_eq!(
        issues[0].body,
        format!(
            "RELEASE-RED: hostkeeper release.yml run {id} ended failure on {c}.\n\n\
             https://github.com/komira-ai/hostkeeper/actions/runs/{id}\n\n\
             The jobs that failed:\n- macos-test\n- check-apt (hostkeeper, arm64)\n\n\
             The deploy went out: no job of the deploy failed. The failed jobs run beside or \
             after it (the Mac's beta stage, the install checks, the promotion to stable) and \
             hold no release; fix forward.\n\n\
             The next run of release.yml on main that succeeds closes this issue.\n\n\
             Red commit: {c}\n"
        )
    );
    assert_eq!(*fakes.runs.writes.borrow(), ["open #1".to_owned()]);
}

#[test]
fn a_release_run_whose_deploy_failed_says_so_and_names_the_deploy_jobs() {
    // Catches: a failed deploy reported as one that went out, or a failed job after the deploy
    // named among the deploy's.
    let (fakes, [_, _, c]) = three();
    let id = run_of(
        &fakes,
        "release.yml",
        &c,
        Some("failure"),
        &[
            ("publish (0.1.4, v0.1.4, c)", "success"),
            ("testing-apt-write", "failure"),
            ("cp-deploy / deploy hostkeeper-coordinator", "timed_out"),
            ("macos-device", "failure"),
            ("set-desired / set-desired", "skipped"),
        ],
    );
    // The workflow given by its path, as the run's event names it, is read by its file name.
    red_of(&fakes, &request(".github/workflows/release.yml", id, &c)).expect("red");
    let body = &fakes.runs.issues()[0].body;
    assert!(
        body.contains(
            "The jobs that failed:\n- testing-apt-write\n- cp-deploy / deploy \
             hostkeeper-coordinator\n- macos-device\n\n\
             The deploy did not complete: testing-apt-write, cp-deploy / deploy \
             hostkeeper-coordinator failed. The next release run releases this release again \
             (`release plan`); fix forward or re-run the failed jobs.\n\n"
        ),
        "{body}"
    );
}

#[test]
fn a_second_red_run_brings_the_open_issue_up_to_date_and_the_same_one_writes_nothing() {
    // Catches: a second issue opened for the same workflow, the open one left naming an older
    // run, or a write for an issue that already says it.
    let (fakes, [_, b, c]) = three();
    fakes
        .runs
        .open_issue(&repo(), "Something else", "unrelated")
        .expect("an unrelated issue");
    let first = run_of(
        &fakes,
        "release.yml",
        &b,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    red_of(&fakes, &request("release.yml", first, &b)).expect("first");
    let second = run_of(
        &fakes,
        "release.yml",
        &c,
        Some("timed_out"),
        &DEPLOYED_THEN_RED,
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", second, &c)),
        Ok(vec![format!(
            "release.yml run {second} of {c} is red: issue #2, {RELEASE_TITLE}"
        )])
    );
    red_of(&fakes, &request("release.yml", second, &c)).expect("again");
    assert_eq!(
        *fakes.runs.writes.borrow(),
        ["open #1", "open #2", "edit #2"].map(str::to_owned)
    );
    let issues = fakes.runs.issues();
    assert_eq!(issues[0].body, "unrelated");
    assert!(issues[1].body.starts_with(&format!(
        "RELEASE-RED: hostkeeper release.yml run {second} ended timed_out on {c}."
    )));
    assert!(issues[1].body.ends_with(&format!("Red commit: {c}\n")));
}

#[test]
fn a_red_privileged_run_opens_its_own_issue_and_says_no_release_waits() {
    // Catches: the privileged tests' alert folded into the release's issue (one success would
    // close both), or an alert that reads as a held release.
    let (fakes, [_, b, _]) = three();
    let release = run_of(
        &fakes,
        "release.yml",
        &b,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    red_of(&fakes, &request("release.yml", release, &b)).expect("release");
    let id = run_of(
        &fakes,
        "privileged.yml",
        &b,
        Some("failure"),
        &[("privileged", "failure"), ("openrc-device", "success")],
    );
    assert_eq!(
        red_of(&fakes, &request("privileged.yml", id, &b)),
        Ok(vec![format!(
            "privileged.yml run {id} of {b} is red: issue #2, {PRIVILEGED_TITLE}"
        )])
    );
    assert_eq!(
        fakes.runs.issues()[1].body,
        format!(
            "RELEASE-RED: hostkeeper privileged.yml run {id} ended failure on {b}.\n\n\
             https://github.com/komira-ai/hostkeeper/actions/runs/{id}\n\n\
             The jobs that failed:\n- privileged\n\n\
             No release waits for the privileged tests: releases go on. Fix forward in a new \
             pull request.\n\n\
             The next run of privileged.yml on main that succeeds closes this issue.\n\n\
             Red commit: {b}\n"
        )
    );
}

#[test]
fn a_run_that_never_started_its_jobs_is_reported_without_a_job() {
    // Catches: a run that failed to start (a workflow file GitHub refuses) reported as nothing.
    let (fakes, [a, ..]) = three();
    let id = run_of(&fakes, "privileged.yml", &a, Some("startup_failure"), &[]);
    red_of(&fakes, &request("privileged.yml", id, &a)).expect("red");
    assert!(fakes.runs.issues()[0].body.contains(
        "The jobs that failed:\n- (no job of the run failed: the run itself did not start or \
         did not finish)\n\n"
    ));
}

#[test]
fn a_later_success_closes_the_issue_and_an_older_one_or_one_that_released_nothing_does_not() {
    // Catches: an issue left open after main is green again, closed by the success of an older
    // commit that finished late, or closed by a release run that released nothing.
    let (fakes, [a, b, c]) = three();
    let red_run = run_of(
        &fakes,
        "release.yml",
        &b,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    red_of(&fakes, &request("release.yml", red_run, &b)).expect("red");

    let older = run_of(
        &fakes,
        "release.yml",
        &a,
        Some("success"),
        &[("publish (0.1.2, v0.1.2, a)", "success")],
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", older, &a)),
        Ok(vec![format!(
            "release.yml run {older} of {a} succeeded, but {b} of issue #1 is not in its \
             history: it stays open"
        )])
    );
    let plan_only = run_of(
        &fakes,
        "release.yml",
        &c,
        Some("success"),
        &[("plan", "success")],
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", plan_only, &c)),
        Ok(vec![format!(
            "release.yml run {plan_only} of {c} succeeded but released nothing: the open issue \
             stays open"
        )])
    );
    assert_eq!(fakes.runs.issues().len(), 1);

    let green = run_of(
        &fakes,
        "release.yml",
        &c,
        Some("success"),
        &[("publish (0.1.4, v0.1.4, c)", "success")],
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", green, &c)),
        Ok(vec![format!(
            "release.yml run {green} of {c} succeeded: closed issue #1"
        )])
    );
    assert_eq!(
        *fakes.runs.writes.borrow(),
        [
            "open #1".to_owned(),
            format!(
                "close #1: Run {green} succeeded on {c}: \
                 https://github.com/komira-ai/hostkeeper/actions/runs/{green}"
            ),
        ]
    );
    assert!(fakes.runs.issues().is_empty());
    assert_eq!(
        red_of(&fakes, &request("release.yml", green, &c)),
        Ok(vec![format!(
            "release.yml run {green} of {c} succeeded; no RELEASE-RED: hostkeeper issue of \
             release.yml is open"
        )])
    );
}

#[test]
fn a_privileged_success_on_the_red_commit_closes_its_issue_and_one_without_a_record_closes_too() {
    // Catches: a privileged success that needs a publish job before it closes, a success on the
    // red commit itself (a re-run) not closing, or an issue whose body lost its record of the
    // red commit staying open for ever.
    let (fakes, [_, b, c]) = three();
    let red_run = run_of(&fakes, "privileged.yml", &b, Some("failure"), &[]);
    red_of(&fakes, &request("privileged.yml", red_run, &b)).expect("red");
    let rerun = run_of(&fakes, "privileged.yml", &b, Some("success"), &[]);
    assert_eq!(
        red_of(&fakes, &request("privileged.yml", rerun, &b)),
        Ok(vec![format!(
            "privileged.yml run {rerun} of {b} succeeded: closed issue #1"
        )])
    );
    let unrecorded = fakes
        .runs
        .open_issue(&repo(), PRIVILEGED_TITLE, "edited by hand")
        .expect("an issue with no record");
    let green = run_of(&fakes, "privileged.yml", &c, Some("success"), &[]);
    assert_eq!(
        red_of(&fakes, &request("privileged.yml", green, &c)),
        Ok(vec![format!(
            "privileged.yml run {green} of {c} succeeded: closed issue #{unrecorded}"
        )])
    );
    assert!(fakes.runs.issues().is_empty());
}

#[test]
fn a_cancelled_run_and_a_run_of_another_branch_change_nothing() {
    // Catches: a cancelled run (a pending release replaced in its group) or a fork's branch
    // reported as red, or taken for a success that closes the issue.
    let (fakes, [a, b, _]) = three();
    let red_run = run_of(
        &fakes,
        "release.yml",
        &a,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    red_of(&fakes, &request("release.yml", red_run, &a)).expect("red");
    let cancelled = run_of(&fakes, "release.yml", &b, Some("cancelled"), &[]);
    assert_eq!(
        red_of(&fakes, &request("release.yml", cancelled, &b)),
        Ok(vec![format!(
            "release.yml run {cancelled} of {b} ended cancelled: nothing to report"
        )])
    );
    let branch = fakes.runs.add(
        "release.yml",
        &b,
        "workflow_run",
        "feature",
        Some("failure"),
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", branch, &b)),
        Ok(vec![format!(
            "release.yml run {branch} of {b}: a run of feature, not main: nothing to report"
        )])
    );
    assert_eq!(*fakes.runs.writes.borrow(), ["open #1".to_owned()]);
}

#[test]
fn another_workflow_an_unknown_run_and_a_run_still_going_are_refused() {
    // Catches: a run of a workflow no issue is kept for, a run id that is not the commit's, or
    // a run not yet completed being reported, each of which would write a wrong issue.
    let (fakes, [a, ..]) = three();
    assert_eq!(
        red_of(&fakes, &request("ci.yml", 1, &a)),
        Err(Error::Failed(
            "release red reports on release.yml and privileged.yml, not ci.yml".to_owned()
        ))
    );
    assert_eq!(
        red_of(&fakes, &request("release.yml", 77, &a)),
        Err(Error::Failed(format!("release.yml has no run 77 for {a}")))
    );
    let going = fakes
        .runs
        .add("release.yml", &a, "workflow_run", "main", None);
    assert_eq!(
        red_of(&fakes, &request("release.yml", going, &a)),
        Err(Error::Failed(format!(
            "release.yml run {going} of {a} has not completed (in_progress)"
        )))
    );
    assert!(fakes.runs.writes.borrow().is_empty());
}

/// Runs whose jobs cannot be listed.
struct NoJobs<'a>(&'a dyn GitHubRuns);

impl GitHubRuns for NoJobs<'_> {
    fn runs(&self, repo: &Repo, workflow: &str, commit: &Commit) -> Result<Vec<Run>, PortError> {
        self.0.runs(repo, workflow, commit)
    }
    fn jobs(&self, _repo: &Repo, run: u64) -> Result<Vec<Job>, PortError> {
        Err(PortError::new(format!("gh jobs {run}"), "HTTP 502"))
    }
    fn on_main(&self, repo: &Repo, commit: &Commit) -> Result<bool, PortError> {
        self.0.on_main(repo, commit)
    }
    fn open_issues(&self, repo: &Repo) -> Result<Vec<Issue>, PortError> {
        self.0.open_issues(repo)
    }
    fn open_issue(&self, repo: &Repo, title: &str, body: &str) -> Result<u64, PortError> {
        self.0.open_issue(repo, title, body)
    }
    fn edit_issue(&self, repo: &Repo, number: u64, body: &str) -> Result<(), PortError> {
        self.0.edit_issue(repo, number, body)
    }
    fn close_issue(&self, repo: &Repo, number: u64, comment: &str) -> Result<(), PortError> {
        self.0.close_issue(repo, number, comment)
    }
}

#[test]
fn a_github_that_cannot_list_the_runs_jobs_fails_and_writes_nothing() {
    // Catches: a run whose jobs could not be read reported with no failed job, or taken for a
    // success that releases nothing.
    let (fakes, [a, ..]) = three();
    let id = run_of(
        &fakes,
        "release.yml",
        &a,
        Some("failure"),
        &DEPLOYED_THEN_RED,
    );
    let runs = NoJobs(&fakes.runs);
    let ports = Ports {
        runs: &runs,
        ..fakes.ports()
    };
    assert_eq!(
        red(&ports, &request("release.yml", id, &a)),
        Err(Error::Port(PortError::new(
            format!("gh jobs {id}"),
            "HTTP 502"
        )))
    );
    assert!(fakes.runs.writes.borrow().is_empty());
}
