//! The plan's resume of the newest published release: released again when the latest release
//! run that released it completed other than successfully; left when that run succeeded, is
//! still going, or no run released it; never when a newer commit is released; and a GitHub that
//! cannot list a run's jobs fails the plan. Each by its full lines.

use super::*;
use crate::release::fakes::{FakeHistory, FakeRuns, Fakes, nth_commit};
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

/// A history of a root commit (version file `0.1`) released as v0.1.1, and the fakes over it.
fn released_root() -> (FakeHistory, Commit) {
    let mut h = FakeHistory::default();
    let r = h.commit(
        None,
        &[
            ("VERSION", Some("0.1\n")),
            ("src/lib.rs", Some("fn a() {}")),
        ],
    );
    (h, r)
}

fn fakes_over(h: FakeHistory, root: &Commit) -> Fakes {
    let fakes = Fakes::over(h);
    fakes.store.insert(Release {
        tag: "v0.1.1".to_owned(),
        commit: root.to_string(),
        draft: false,
        assets: Vec::new(),
    });
    fakes
}

/// The job of a release run that publishes v0.1.1 of `commit`.
fn publish_job(commit: &Commit) -> String {
    format!("publish (0.1.1, v0.1.1, {commit})")
}

/// A run of release.yml on `branch` for the head `head` that ended `conclusion` (none: still
/// going), with the jobs `jobs`, each ended as the run did; its id.
fn release_run(
    fakes: &Fakes,
    head: &Commit,
    branch: &str,
    conclusion: Option<&str>,
    jobs: &[String],
) -> u64 {
    let id = fakes
        .runs
        .add("release.yml", head, "workflow_run", branch, conclusion);
    let jobs = jobs
        .iter()
        .map(|name| Job {
            name: name.clone(),
            conclusion: conclusion.map(str::to_owned),
        })
        .collect();
    fakes.runs.jobs.borrow_mut().insert(id, jobs);
    id
}

/// The jobs of a run that released v0.1.1 of `commit`.
fn released(commit: &Commit) -> Vec<String> {
    vec![
        "plan".to_owned(),
        format!("verify (0.1.1, v0.1.1, {commit})"),
        publish_job(commit),
        format!("image (0.1.1, v0.1.1, {commit})"),
        "set-desired / set-desired".to_owned(),
    ]
}

fn again_matrix(commit: &Commit) -> String {
    format!(
        "matrix={{\"include\":[{{\"version\":\"0.1.1\",\"tag\":\"v0.1.1\",\"commit\":\"{commit}\"}}]}}"
    )
}

const NOTHING: [&str; 2] = ["release=false", "matrix={\"include\":[]}"];

fn nothing_but(line: Option<String>) -> Vec<String> {
    line.into_iter()
        .chain(NOTHING.iter().map(|l| (*l).to_owned()))
        .collect()
}

#[test]
fn a_release_whose_release_run_failed_is_released_again_while_the_next_commit_waits() {
    // Catches: the run's failure (the image push refused, then set-desired) left for good
    // because the next run found the version published already and planned nothing.
    let (mut h, r) = released_root();
    let a = h.commit(Some(&r), &[("src/lib.rs", Some("fn b() {}"))]);
    let fakes = fakes_over(h, &r);
    let plan_only = release_run(&fakes, &r, "main", Some("success"), &["plan".to_owned()]);
    let failed = release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    let this_run = release_run(&fakes, &a, "main", None, &[]);
    assert!(plan_only < failed && failed < this_run);
    fakes.runs.push("privileged.yml", &a, "success");
    fakes.runs.push("artifacts.yml", &a, "success");
    let ci = fakes.runs.add("ci.yml", &a, "push", "main", None);
    assert_eq!(
        plan(&fakes.ports(), &request(&a)),
        Ok(vec![
            format!("no release of {a} (0.1.2): waiting for ci.yml: run {ci} has not completed"),
            format!(
                "release v0.1.1 of {r} again: its release run {failed} ended failure (1 of 3 \
                 runs in a row that did not succeed)"
            ),
            "release=true".to_owned(),
            again_matrix(&r),
        ])
    );
}

#[test]
fn a_release_whose_release_run_succeeded_is_left() {
    // Catches: every plan releasing the newest release again, whatever its run did.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    let ok = release_run(&fakes, &r, "main", Some("success"), &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(nothing_but(Some(format!(
            "v0.1.1 of {r}: its release run {ok} succeeded"
        ))))
    );
}

#[test]
fn the_latest_release_run_of_the_release_decides() {
    // Catches: an older run read before a newer one, so a release that a later run completed is
    // released again for ever, or one whose later run was cancelled is left.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    let ok = release_run(&fakes, &r, "main", Some("success"), &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(nothing_but(Some(format!(
            "v0.1.1 of {r}: its release run {ok} succeeded"
        ))))
    );
    let cancelled = release_run(&fakes, &r, "main", Some("cancelled"), &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(vec![
            format!(
                "release v0.1.1 of {r} again: its release run {cancelled} ended cancelled (1 of \
                 3 runs in a row that did not succeed)"
            ),
            "release=true".to_owned(),
            again_matrix(&r),
        ])
    );
}

#[test]
fn a_release_run_still_going_is_waited_for() {
    // Catches: a second run releasing a release the running one is still pushing or setting.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    let going = release_run(&fakes, &r, "main", None, &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(nothing_but(Some(format!(
            "v0.1.1 of {r}: waiting for its release run {going}, which has not completed"
        ))))
    );
}

#[test]
fn a_run_of_another_branch_or_of_another_release_never_counts() {
    // Catches: a run of a branch named otherwise, or a failed run that released another version
    // or another commit, taken for the release's run.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    release_run(&fakes, &r, "feature", Some("failure"), &released(&r));
    let other = nth_commit(99);
    release_run(&fakes, &r, "main", Some("failure"), &released(&other));
    release_run(
        &fakes,
        &r,
        "main",
        Some("failure"),
        &[format!("publish (0.1.0, v0.1.0, {r})")],
    );
    assert_eq!(plan(&fakes.ports(), &request(&r)), Ok(nothing_but(None)));
}

#[test]
fn a_newer_commit_released_supersedes_a_failed_release_run() {
    // Catches: an older release planned before a newer one in the same run, which a permanent
    // failure of the older one would hold for ever.
    let (mut h, r) = released_root();
    let a = h.commit(Some(&r), &[("src/lib.rs", Some("fn b() {}"))]);
    let fakes = fakes_over(h, &r);
    release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    for workflow in REQUIRED_WORKFLOWS {
        fakes.runs.push(workflow, &a, "success");
    }
    assert_eq!(
        plan(&fakes.ports(), &request(&a)),
        Ok(vec![
            format!("release v0.1.2 of {a}"),
            "release=true".to_owned(),
            format!(
                "matrix={{\"include\":[{{\"version\":\"0.1.2\",\"tag\":\"v0.1.2\",\"commit\":\"{a}\"}}]}}"
            ),
        ])
    );
}

/// The runs of `runs`, whose jobs cannot be listed.
struct NoJobs<'a>(&'a FakeRuns);

impl GitHubRuns for NoJobs<'_> {
    fn runs(&self, repo: &Repo, workflow: &str, commit: &Commit) -> Result<Vec<Run>, PortError> {
        self.0.runs(repo, workflow, commit)
    }
    fn jobs(&self, _: &Repo, run: u64) -> Result<Vec<Job>, PortError> {
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
fn a_github_that_cannot_list_a_runs_jobs_fails_the_plan() {
    // Catches: a run whose jobs could not be read taken for a run without the release, which
    // would leave a failed release as it is.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    let id = release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    let runs = NoJobs(&fakes.runs);
    let ports = Ports {
        runs: &runs,
        ..fakes.ports()
    };
    assert_eq!(
        plan(&ports, &request(&r)),
        Err(Error::Port(PortError::new(
            format!("gh jobs {id}"),
            "HTTP 502"
        )))
    );
}

#[test]
fn a_release_is_released_again_until_three_of_its_runs_in_a_row_did_not_succeed() {
    // Catches: a failure every attempt meets (a coordinator that refuses the set) releasing the
    // same release again in every run for ever, or the count not reset by a success between.
    let (h, r) = released_root();
    let fakes = fakes_over(h, &r);
    release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    release_run(&fakes, &r, "main", Some("success"), &released(&r));
    release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    let second = release_run(&fakes, &r, "main", Some("timed_out"), &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(vec![
            format!(
                "release v0.1.1 of {r} again: its release run {second} ended timed_out (2 of 3 \
                 runs in a row that did not succeed)"
            ),
            "release=true".to_owned(),
            again_matrix(&r),
        ])
    );
    let third = release_run(&fakes, &r, "main", Some("failure"), &released(&r));
    assert_eq!(
        plan(&fakes.ports(), &request(&r)),
        Ok(nothing_but(Some(format!(
            "v0.1.1 of {r} is not released again: its last 3 release runs ended failure, \
             timed_out, failure (the latest {third}); re-run its failed jobs once the cause is \
             fixed"
        ))))
    );
}
