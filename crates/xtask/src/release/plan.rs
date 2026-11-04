//! `release plan`: whether a commit (on the scheduled path, each commit missed since the last
//! release) is released, and under which version.
//!
//! A commit is released when it is not frozen, not already released, not documentation only,
//! not held by a red privileged run, and every required workflow has a successful push run on
//! `main` for it. Versions come from the first-parent history ([`version::version_in`]), never
//! from tags, so a documentation-only commit has its number and later numbers count it.

use std::path::PathBuf;

use super::contract::{Commit, PRIVILEGED_WORKFLOW, REQUIRED_WORKFLOWS, Repo, Version};
use super::ports::{Ports, Release, Run};
use super::{Error, RunState, answer, state, version};

/// What `release plan` is asked.
pub(crate) struct Request {
    pub(crate) repo: Repo,
    /// The pushed commit, or on the scheduled path the tip of `main`.
    pub(crate) commit: Commit,
    pub(crate) version_file: String,
    /// The value of the repository variable `RELEASE_FREEZE`.
    pub(crate) freeze: String,
    /// Plan every commit since the last release, not only `commit`.
    pub(crate) scheduled: bool,
    /// A file the answers (`key=value` lines) are appended to.
    pub(crate) output: Option<PathBuf>,
}

/// The paths of documentation: a path under the first, or a path ending as the second says at
/// any depth.
pub(crate) const DOCS_PATHS: [&str; 2] = ["docs/", "**/*.md"];

/// The title of the one issue that tracks a hold on releases.
pub(crate) const BLOCK_TITLE: &str = "Releases are held: the privileged tests failed on main";

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

/// One commit judged: its version, and why it is not released (none: it is released).
struct Candidate {
    commit: Commit,
    version: Version,
    reason: Option<String>,
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
}

/// Plans the release of the requested commit (or of each missed commit) and returns the printed
/// lines followed by the answer lines, which are also appended to the output file.
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
    let end = match last_release(&releases, &chain) {
        Some(index) => index,
        None => version::base_of(ports.git, &chain, &request.version_file)? + 1,
    };
    let hold = hold(ports, request, &chain, end)?;
    let context = Context {
        ports: *ports,
        request,
        chain: &chain,
        releases: &releases,
        hold,
    };
    let indices: Vec<usize> = if request.scheduled {
        (0..end).rev().collect()
    } else {
        vec![0]
    };
    let mut candidates = Vec::new();
    for index in indices {
        candidates.push(judge(&context, index)?);
    }
    let printed = candidates.iter().map(describe).collect();
    finish(request, printed, &candidates)
}

/// The index in `chain` of the published release nearest the tip whose tag is a version.
fn last_release(releases: &[Release], chain: &[Commit]) -> Option<usize> {
    releases
        .iter()
        .filter(|r| Version::of_tag(&r.tag).is_some())
        .filter_map(|r| chain.iter().position(|c| c.as_str() == r.commit))
        .min()
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
    let reason = decline(context, index, &commit, &version)?;
    Ok(Candidate {
        commit,
        version,
        reason,
    })
}

/// Why the commit at `index` is not released, in the order of the rules; none when it is.
fn decline(
    context: &Context<'_>,
    index: usize,
    commit: &Commit,
    version: &Version,
) -> Result<Option<String>, Error> {
    let tag = version.tag();
    let released = context
        .releases
        .iter()
        .find(|r| (r.tag == tag) | (r.commit == commit.as_str()));
    if let Some(release) = released {
        return Ok(Some(format!("already released as {}", release.tag)));
    }
    let paths = context
        .ports
        .git
        .changed_paths(commit, context.chain.get(index + 1))?;
    if paths.iter().all(|p| is_docs_path(p)) {
        return Ok(Some("documentation only".to_owned()));
    }
    if let Some(hold) = context.hold.as_ref().filter(|h| index <= h.index) {
        return Ok(Some(hold.reason()));
    }
    waiting(context, commit)
}

/// The first required workflow without a successful push run for `commit`, as a reason.
fn waiting(context: &Context<'_>, commit: &Commit) -> Result<Option<String>, Error> {
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
            RunState::Pending(id) => format!("waiting for {workflow}: run {id} has not completed"),
            RunState::Failed(id) => {
                format!("{workflow} run {id} ended {}", conclusion_of(&runs, id))
            }
            RunState::Other(id, conclusion) => format!("{workflow} run {id} ended {conclusion}"),
        };
        return Ok(Some(reason));
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
    let mut lines = vec![
        format!("release={}", !released.is_empty()),
        format!("matrix={{\"include\":[{}]}}", entries.join(",")),
    ];
    if !request.scheduled {
        lines.extend(push_lines(candidates.first()));
    }
    printed.extend(answer(request.output.as_deref(), lines)?);
    Ok(printed)
}

/// The answers only the push path gives: the version, tag and commit of the released commit, or
/// the reason it is not released (none judged: the repository is frozen).
fn push_lines(candidate: Option<&Candidate>) -> Vec<String> {
    match candidate {
        None => vec![format!("reason={FROZEN}")],
        Some(Candidate {
            reason: Some(reason),
            ..
        }) => vec![format!("reason={reason}")],
        Some(released) => vec![
            format!("version={}", released.version),
            format!("tag={}", released.version.tag()),
            format!("commit={}", released.commit),
        ],
    }
}

#[cfg(test)]
mod tests;
