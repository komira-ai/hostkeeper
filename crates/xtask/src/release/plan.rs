//! `release plan`: which commits since the last release are released, oldest first, and under
//! which versions.
//!
//! Every run of `release.yml` plans the same way, whatever started it: it walks the first-parent
//! history from the tip it is given down to the last release and judges each commit. A run
//! started by the completion of one commit's run therefore also releases every earlier commit
//! whose runs are done, so a run that a newer one replaced in the release concurrency group
//! leaves nothing behind.
//!
//! Releases keep the order of their versions: a commit with a required run that has not
//! completed holds every newer commit that would be released until that run completes (a newer
//! release would end the walk above it, and it would never be released). A missing or failed run
//! holds nothing: it may never change.
//!
//! A commit is released when it is not frozen, not already released, not documentation only,
//! not held by a red privileged run, and every required workflow has a successful push run on
//! `main` for it. Versions come from the first-parent history ([`version::version_in`]), never
//! from tags, so a documentation-only commit has its number and later numbers count it.
//!
//! A release is published before its image is pushed and before it is set as the desired
//! version, so a run can publish a release and then fail. When the plan releases no commit, it
//! therefore looks for the latest run of `release.yml` on `main` that released the newest
//! published release (the one with its job `publish (<version>, <tag>, <commit>)`): when that run
//! completed other than successfully, the plan releases that release again, and every job after
//! it runs again for it (publishing a release that holds the same files, and pushing the same
//! image, change nothing). A run that has not completed is waited for; when no run is found the
//! release is left as it is; after [`RELEASE_RUNS`] such runs in a row that did not succeed it
//! is left too, and says so (a maintainer re-runs the failed jobs once the cause is fixed). A
//! newer commit released supersedes it: the run sets the newer one.
//!
//! A commit not yet released that the rules above would release is passed over when its own
//! verify job (`verify (<version>, <tag>, <commit>)`) ended `failure` in each of its latest
//! [`RELEASE_RUNS`] release runs on `main` that ran it, with no other end of that job between
//! ([`passed_over`]): a failure that every attempt meets, such as a file the release's build never
//! made, would otherwise be planned first in every run for ever, and, since the jobs of a release
//! run wait for every verify, hold every newer commit with it. The line of the commit says so and
//! names the runs; the newer commits are released, and its version, below the next release, is
//! never planned again. A verify job cancelled (the run's matrix stops at its first failure, so
//! a newer commit's verify is cancelled by an older one's) or that succeeded resets the count.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::contract::{
    Commit, MAIN, PRIVILEGED_WORKFLOW, RELEASE_WORKFLOW, REQUIRED_WORKFLOWS, Repo, Version,
};
use super::ports::{Job, Ports, Release, Run};
use super::{Error, RunState, answer, state, version};

/// What `release plan` is asked.
pub(crate) struct Request {
    pub(crate) repo: Repo,
    /// The tip of `main` the walk starts from.
    pub(crate) commit: Commit,
    pub(crate) version_file: String,
    /// The value of the repository variable `RELEASE_FREEZE`.
    pub(crate) freeze: String,
    /// A file the answers (`key=value` lines) are appended to.
    pub(crate) output: Option<PathBuf>,
}

/// The paths of documentation: a path under the first, or a path ending as the second says at
/// any depth.
pub(crate) const DOCS_PATHS: [&str; 2] = ["docs/", "**/*.md"];

/// The title of the one issue that tracks a hold on releases.
pub(crate) const BLOCK_TITLE: &str = "Releases are held: the privileged tests failed on main";

/// The release runs of one release that may end other than successfully in a row before the plan
/// stops releasing it again, and the release runs in a row whose verify job of a commit not yet
/// released may fail before the plan passes over that commit: a failure that every attempt meets
/// (a refusal that does not pass) then holds no more than three runs of the release group.
const RELEASE_RUNS: usize = 3;

/// Why a frozen repository releases nothing.
const FROZEN: &str = "RELEASE_FREEZE is set";

/// Whether the freeze variable `value` stops releases: anything but empty or `false` in any
/// ASCII case.
fn frozen(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty() | value.eq_ignore_ascii_case("false"))
}

/// Whether `path` is documentation: see [`DOCS_PATHS`].
pub(crate) fn is_docs_path(path: &str) -> bool {
    let [dir, any_depth] = DOCS_PATHS;
    let suffix = any_depth.trim_start_matches("**/*");
    path.starts_with(dir) | path.ends_with(suffix)
}

/// One commit judged: its version, why it is not released (none: it is released), and whether
/// a required run of it has not completed.
struct Candidate {
    commit: Commit,
    version: Version,
    reason: Option<String>,
    pending: bool,
}

/// Why a commit is not released, and whether that is a required run that has not completed.
struct Decline {
    reason: String,
    pending: bool,
}

impl Decline {
    fn settled(reason: String) -> Option<Self> {
        Some(Self {
            reason,
            pending: false,
        })
    }
}

/// An open hold: the privileged run `run` failed on `commit`, the commit at `index` of the
/// chain, and the issue `issue` tracks it.
struct Hold {
    index: usize,
    run: u64,
    commit: Commit,
    issue: u64,
}

impl Hold {
    fn reason(&self) -> String {
        format!(
            "held: privileged run {} failed on {}; tracking issue #{}",
            self.run, self.commit, self.issue
        )
    }
}

/// What judging a candidate reads.
struct Context<'a> {
    ports: Ports<'a>,
    request: &'a Request,
    /// The request's commit and then each first parent, newest first.
    chain: &'a [Commit],
    /// The published releases.
    releases: &'a [Release],
    hold: Option<Hold>,
    /// The end of the window: `chain[..end]` are the commits judged, and `chain[end]`, when
    /// there is a last release, is its commit.
    end: usize,
    /// The release runs on `main` whose head is a commit of the window or the last release's,
    /// newest first, read once ([`Context::release_runs`]).
    release_runs: OnceCell<Vec<Run>>,
    /// The jobs of each release run read so far, by its id ([`Context::jobs_of`]).
    jobs: RefCell<BTreeMap<u64, Vec<Job>>>,
}

impl Context<'_> {
    /// The runs of `release.yml` on `main` whose head is a commit of `chain[..=end]`, newest
    /// first, each once: every release run that can have released a commit of the window, or
    /// the last release (a run's head is the tip of `main` it planned from, the commit itself or
    /// a newer one).
    fn release_runs(&self) -> Result<&[Run], Error> {
        if let Some(runs) = self.release_runs.get() {
            return Ok(runs);
        }
        let heads = &self.chain[..(self.end + 1).min(self.chain.len())];
        let mut runs = Vec::new();
        for head in heads {
            let found = self
                .ports
                .runs
                .runs(&self.request.repo, RELEASE_WORKFLOW, head)?;
            runs.extend(found.into_iter().filter(|r| r.branch == MAIN));
        }
        runs.sort_by_key(|r| std::cmp::Reverse(r.id));
        runs.dedup_by_key(|r| r.id);
        Ok(self.release_runs.get_or_init(|| runs))
    }

    /// The jobs of the release run `run`, read from GitHub once per plan.
    fn jobs_of(&self, run: u64) -> Result<Vec<Job>, Error> {
        if let Some(jobs) = self.jobs.borrow().get(&run) {
            return Ok(jobs.clone());
        }
        let jobs = self.ports.runs.jobs(&self.request.repo, run)?;
        self.jobs.borrow_mut().insert(run, jobs.clone());
        Ok(jobs)
    }
}

/// Plans the release of each commit from the requested one down to the last release, oldest
/// first, and returns the printed lines followed by the answer lines, which are also appended to
/// the output file.
pub(crate) fn plan(ports: &Ports<'_>, request: &Request) -> Result<Vec<String>, Error> {
    if frozen(&request.freeze) {
        return finish(request, vec![format!("no release: {FROZEN}")], &[]);
    }
    if ports.git.is_shallow()? {
        return Err(Error::Failed(version::SHALLOW.to_owned()));
    }
    let chain = ports.git.first_parents(&request.commit)?;
    let releases: Vec<Release> = ports
        .store
        .releases(&request.repo)?
        .into_iter()
        .filter(|r| !r.draft)
        .collect();
    // The window is chain[..end]: the commits newer than the last release, or, with no release,
    // those from the commit that last changed the version file.
    let last = last_release(&releases, &chain);
    let end = match &last {
        Some((index, _)) => *index,
        None => version::base_of(ports.git, &chain, &request.version_file)? + 1,
    };
    let hold = hold(ports, request, &chain, end)?;
    let context = Context {
        ports: *ports,
        request,
        chain: &chain,
        releases: &releases,
        hold,
        end,
        release_runs: OnceCell::new(),
        jobs: RefCell::new(BTreeMap::new()),
    };
    let mut candidates: Vec<Candidate> = Vec::new();
    for index in (0..end).rev() {
        let mut candidate = judge(&context, index)?;
        let older = candidates.iter().find(|c| c.pending);
        if let (None, Some(older)) = (&candidate.reason, older) {
            candidate.reason = Some(format!(
                "waiting for {} ({}), an older commit whose runs have not completed",
                older.commit, older.version
            ));
        }
        candidates.push(candidate);
    }
    let mut printed: Vec<String> = candidates.iter().map(describe).collect();
    if let (Some((index, version)), true) = (last, candidates.iter().all(|c| c.reason.is_some())) {
        let (line, again) = resume(&context, index, version)?;
        printed.extend(line);
        candidates.extend(again);
    }
    finish(request, printed, &candidates)
}

/// The newest published release, `version` at `index` of the chain, released again when the
/// latest run of `release.yml` on `main` that released it completed other than successfully (see
/// the module): the line that says why or why not (none when no run released it), and the
/// candidate.
fn resume(
    context: &Context<'_>,
    index: usize,
    version: Version,
) -> Result<(Option<String>, Option<Candidate>), Error> {
    let commit = &context.chain[index];
    let job = format!("publish ({version}, {}, {commit})", version.tag());
    let tag = version.tag();
    // The release's runs that completed other than successfully, newest first, back to its
    // latest success.
    let mut failed: Vec<(u64, String)> = Vec::new();
    for run in context.release_runs()? {
        if !context.jobs_of(run.id)?.iter().any(|j| j.name == job) {
            continue;
        }
        let ended =
            (run.status == "completed").then(|| run.conclusion.as_deref().unwrap_or("none"));
        match (failed.is_empty(), ended) {
            (true, Some("success")) => {
                let line = format!("{tag} of {commit}: its release run {} succeeded", run.id);
                return Ok((Some(line), None));
            }
            (true, None) => {
                let line = format!(
                    "{tag} of {commit}: waiting for its release run {}, which has not completed",
                    run.id
                );
                return Ok((Some(line), None));
            }
            (_, Some(conclusion)) if conclusion != "success" => {
                failed.push((run.id, conclusion.to_owned()));
                if failed.len() == RELEASE_RUNS {
                    break;
                }
            }
            _ => break,
        }
    }
    let Some((latest, conclusion)) = failed.first() else {
        return Ok((None, None));
    };
    if failed.len() == RELEASE_RUNS {
        let ended: Vec<&str> = failed.iter().map(|(_, c)| c.as_str()).collect();
        let line = format!(
            "{tag} of {commit} is not released again: its last {RELEASE_RUNS} release runs \
             ended {} (the latest {latest}); re-run its failed jobs once the cause is fixed",
            ended.join(", ")
        );
        return Ok((Some(line), None));
    }
    let line = format!(
        "release {tag} of {commit} again: its release run {latest} ended {conclusion} ({} of \
         {RELEASE_RUNS} runs in a row that did not succeed)",
        failed.len()
    );
    let again = Candidate {
        commit: commit.clone(),
        version,
        reason: None,
        pending: false,
    };
    Ok((Some(line), Some(again)))
}

/// The index in `chain` of the published release nearest the tip whose tag is a version, and
/// that version.
fn last_release(releases: &[Release], chain: &[Commit]) -> Option<(usize, Version)> {
    releases
        .iter()
        .filter_map(|r| {
            let version = Version::of_tag(&r.tag)?;
            Some((chain.iter().position(|c| c.as_str() == r.commit)?, version))
        })
        .min_by_key(|(index, _)| *index)
}

/// The hold on releases, in one place so a different rule can replace it: the newest commit of
/// the window whose privileged run failed holds every candidate at or after it, unless a commit
/// newer than it has a successful privileged run. A later success of another workflow never
/// lifts it. An open hold is tracked by one issue, opened or updated here.
fn hold(
    ports: &Ports<'_>,
    request: &Request,
    chain: &[Commit],
    end: usize,
) -> Result<Option<Hold>, Error> {
    for (index, commit) in chain.iter().enumerate().take(end) {
        let runs = ports
            .runs
            .runs(&request.repo, PRIVILEGED_WORKFLOW, commit)?;
        match state(&runs, commit) {
            RunState::Succeeded(_) => return Ok(None),
            RunState::Failed(run) => {
                let issue = track(ports, &request.repo, &hold_body(commit, run))?;
                return Ok(Some(Hold {
                    index,
                    run,
                    commit: commit.clone(),
                    issue,
                }));
            }
            RunState::Missing | RunState::Pending(_) | RunState::Other(..) => {}
        }
    }
    Ok(None)
}

/// The body of the issue that tracks a hold.
fn hold_body(commit: &Commit, run: u64) -> String {
    format!(
        "Releases are held. The privileged run {run} failed on {commit}.\n\n\
         No commit from {commit} on is released until a later commit's privileged run succeeds. \
         A later success of another workflow does not lift the hold.\n"
    )
}

/// Opens the one issue titled [`BLOCK_TITLE`] with `body`, or brings the open one to `body`;
/// its number. An open issue that already holds `body` is not written.
fn track(ports: &Ports<'_>, repo: &Repo, body: &str) -> Result<u64, Error> {
    let issues = ports.runs.open_issues(repo)?;
    match issues.iter().find(|i| i.title == BLOCK_TITLE) {
        None => Ok(ports.runs.open_issue(repo, BLOCK_TITLE, body)?),
        Some(issue) => {
            if issue.body != body {
                ports.runs.edit_issue(repo, issue.number, body)?;
            }
            Ok(issue.number)
        }
    }
}

/// Judges the commit at `index` of the chain.
fn judge(context: &Context<'_>, index: usize) -> Result<Candidate, Error> {
    let commit = context.chain[index].clone();
    let version = version::version_in(
        context.ports.git,
        &context.chain[index..],
        &context.request.version_file,
    )?;
    let decline = decline(context, index, &commit, &version)?;
    Ok(Candidate {
        commit,
        version,
        pending: decline.as_ref().is_some_and(|d| d.pending),
        reason: decline.map(|d| d.reason),
    })
}

/// Why the commit at `index` is not released, in the order of the rules; none when it is.
fn decline(
    context: &Context<'_>,
    index: usize,
    commit: &Commit,
    version: &Version,
) -> Result<Option<Decline>, Error> {
    let tag = version.tag();
    let released = context
        .releases
        .iter()
        .find(|r| (r.tag == tag) | (r.commit == commit.as_str()));
    if let Some(release) = released {
        return Ok(Decline::settled(format!(
            "already released as {}",
            release.tag
        )));
    }
    let paths = context
        .ports
        .git
        .changed_paths(commit, context.chain.get(index + 1))?;
    if paths.iter().all(|p| is_docs_path(p)) {
        return Ok(Decline::settled("documentation only".to_owned()));
    }
    if let Some(hold) = context.hold.as_ref().filter(|h| index <= h.index) {
        return Ok(Decline::settled(hold.reason()));
    }
    if let Some(decline) = waiting(context, commit)? {
        return Ok(Some(decline));
    }
    passed_over(context, commit, version)
}

/// Why `commit`, which every other rule releases, is passed over: its verify job ended
/// `failure` in each of its latest [`RELEASE_RUNS`] release runs that ran it (see the module).
/// None when fewer did, or a run between ended that job otherwise (or has not ended it).
fn passed_over(
    context: &Context<'_>,
    commit: &Commit,
    version: &Version,
) -> Result<Option<Decline>, Error> {
    let job = format!("verify ({version}, {}, {commit})", version.tag());
    let mut failed: Vec<u64> = Vec::new();
    for run in context.release_runs()? {
        let jobs = context.jobs_of(run.id)?;
        let Some(verify) = jobs.iter().find(|j| j.name == job) else {
            continue;
        };
        if verify.conclusion.as_deref() != Some("failure") {
            break;
        }
        failed.push(run.id);
        if failed.len() == RELEASE_RUNS {
            let ids: Vec<String> = failed.iter().map(u64::to_string).collect();
            return Ok(Decline::settled(format!(
                "passed over: its verify job failed in each of its last {RELEASE_RUNS} release \
                 runs ({}, the latest first), a failure every attempt meets; the newer commits \
                 are released without it",
                ids.join(", ")
            )));
        }
    }
    Ok(None)
}

/// The first required workflow without a successful push run for `commit`, as a reason.
fn waiting(context: &Context<'_>, commit: &Commit) -> Result<Option<Decline>, Error> {
    for workflow in REQUIRED_WORKFLOWS {
        let runs = context
            .ports
            .runs
            .runs(&context.request.repo, workflow, commit)?;
        let reason = match state(&runs, commit) {
            RunState::Succeeded(_) => continue,
            RunState::Missing => {
                format!("waiting for {workflow}: no push run on main for this commit")
            }
            RunState::Pending(id) => {
                return Ok(Some(Decline {
                    reason: format!("waiting for {workflow}: run {id} has not completed"),
                    pending: true,
                }));
            }
            RunState::Failed(id) => {
                format!("{workflow} run {id} ended {}", conclusion_of(&runs, id))
            }
            RunState::Other(id, conclusion) => format!("{workflow} run {id} ended {conclusion}"),
        };
        return Ok(Decline::settled(reason));
    }
    Ok(None)
}

/// How the run `id` of `runs` ended.
fn conclusion_of(runs: &[Run], id: u64) -> String {
    runs.iter()
        .filter(|r| r.id == id)
        .filter_map(|r| r.conclusion.clone())
        .next()
        .unwrap_or_default()
}

/// The printed line of one judged candidate.
fn describe(candidate: &Candidate) -> String {
    match &candidate.reason {
        None => format!("release v{} of {}", candidate.version, candidate.commit),
        Some(reason) => format!(
            "no release of {} ({}): {reason}",
            candidate.commit, candidate.version
        ),
    }
}

/// Appends the answers for `candidates` to the output and returns `printed` followed by them.
fn finish(
    request: &Request,
    mut printed: Vec<String>,
    candidates: &[Candidate],
) -> Result<Vec<String>, Error> {
    let released: Vec<&Candidate> = candidates.iter().filter(|c| c.reason.is_none()).collect();
    let entries: Vec<String> = released
        .iter()
        .map(|c| {
            format!(
                "{{\"version\":\"{}\",\"tag\":\"{}\",\"commit\":\"{}\"}}",
                c.version,
                c.version.tag(),
                c.commit
            )
        })
        .collect();
    let lines = vec![
        format!("release={}", !released.is_empty()),
        format!("matrix={{\"include\":[{}]}}", entries.join(",")),
    ];
    printed.extend(answer(request.output.as_deref(), lines)?);
    Ok(printed)
}

#[cfg(test)]
mod passed_over_tests;
#[cfg(test)]
mod resume_tests;
#[cfg(test)]
mod tests;
