//! The ports of the release verbs: everything they ask of git, of GitHub and of a binary, as
//! traits, so that every verb runs in a test against fakes (`fakes`).
//!
//! The adapters that answer them for real are `git::GitCli` ([`GitHistory`]), `github::Gh`
//! ([`GitHubRuns`], [`ReleaseStore`], [`Verifier`]), `check_binary::VersionOf`
//! ([`RunVersion`]), `macos_pkg::PkgbuildCli` ([`BuildPkg`], `pkgutil` too), `code_signature::CodesignCli` ([`CodeSign`]) and `advisories::DenyCli` ([`AdvisoryCheck`]). Each runs its program through an [`Exec`], and the
//! one [`Exec`] that starts a process, `real::SystemExec`, is built in `real` only and named by
//! no test.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::contract::{Commit, Repo, Signer};

/// Why a port could not answer: what was asked and why it failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what}: {why}")]
pub(crate) struct PortError {
    pub(crate) what: String,
    pub(crate) why: String,
}

impl PortError {
    pub(crate) fn new(what: impl Into<String>, why: impl Into<String>) -> PortError {
        PortError {
            what: what.into(),
            why: why.into(),
        }
    }
}

/// The history of the repository, as git holds it.
pub(crate) trait GitHistory {
    /// Whether the clone is shallow: its history stops before the root commit.
    fn is_shallow(&self) -> Result<bool, PortError>;
    /// `commit` and then each first parent in turn, newest first, down to the root commit.
    fn first_parents(&self, commit: &Commit) -> Result<Vec<Commit>, PortError>;
    /// The text of the file `path` (from the repository root) at `commit`, none when there is
    /// no such file there.
    fn file_at(&self, commit: &Commit, path: &str) -> Result<Option<String>, PortError>;
    /// The paths `commit` changes against `parent`, or, for the root commit (no parent), every
    /// path it holds.
    fn changed_paths(
        &self,
        commit: &Commit,
        parent: Option<&Commit>,
    ) -> Result<Vec<String>, PortError>;
    /// The committer time of `commit`, in seconds since the Unix epoch.
    fn commit_time(&self, commit: &Commit) -> Result<u64, PortError>;
}

/// One workflow run, as GitHub lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) id: u64,
    /// `push`, `pull_request`, `schedule`, ...
    pub(crate) event: String,
    /// The branch the run is for (a pull request's head branch, for a pull-request run).
    pub(crate) branch: String,
    pub(crate) head_sha: String,
    /// `queued`, `in_progress`, `completed`, ...
    pub(crate) status: String,
    /// Set once the run is completed: `success`, `failure`, `cancelled`, ...
    pub(crate) conclusion: Option<String>,
}

/// One job of a workflow run, as GitHub lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Job {
    /// The job's name; a job of a matrix is named `<job> (<value>, ...)`.
    pub(crate) name: String,
    /// Set once the job is completed: `success`, `failure`, `cancelled`, `skipped`, ...
    pub(crate) conclusion: Option<String>,
}

/// One open issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Issue {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// The workflow runs and the issues of a repository on GitHub.
pub(crate) trait GitHubRuns {
    /// The runs of the workflow file `workflow` for the head commit `commit`, of any event and
    /// branch: the caller picks the ones that count.
    fn runs(&self, repo: &Repo, workflow: &str, commit: &Commit) -> Result<Vec<Run>, PortError>;
    /// The jobs of the run `run`, as its latest attempt lists them (a re-run of failed jobs
    /// lists every job of the run, the ones it did not run again too).
    fn jobs(&self, repo: &Repo, run: u64) -> Result<Vec<Job>, PortError>;
    /// Whether `commit` is on `main`: `main` is the commit or a descendant of it.
    fn on_main(&self, repo: &Repo, commit: &Commit) -> Result<bool, PortError>;
    /// The open issues.
    fn open_issues(&self, repo: &Repo) -> Result<Vec<Issue>, PortError>;
    /// Opens an issue; its number.
    fn open_issue(&self, repo: &Repo, title: &str, body: &str) -> Result<u64, PortError>;
    /// Replaces the body of issue `number`.
    fn edit_issue(&self, repo: &Repo, number: u64, body: &str) -> Result<(), PortError>;
    /// Comments `comment` on issue `number`, then closes it as completed.
    fn close_issue(&self, repo: &Repo, number: u64, comment: &str) -> Result<(), PortError>;
}

/// One asset of a release, as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredAsset {
    pub(crate) name: String,
    /// The SHA-256 the store computed, 64 lowercase hex digits; none when it gives none.
    pub(crate) sha256: Option<String>,
}

/// One release, draft or published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) tag: String,
    /// The commit the release was created at.
    pub(crate) commit: String,
    pub(crate) draft: bool,
    pub(crate) assets: Vec<StoredAsset>,
}

/// Where releases are kept: GitHub releases.
pub(crate) trait ReleaseStore {
    /// Every release, drafts included.
    fn releases(&self, repo: &Repo) -> Result<Vec<Release>, PortError>;
    /// Creates the draft release `tag` at `commit`, with no assets.
    fn create_draft(
        &self,
        repo: &Repo,
        tag: &str,
        commit: &Commit,
        notes: &str,
    ) -> Result<(), PortError>;
    /// Uploads `file` to the draft `tag`, under its file name.
    fn upload(&self, repo: &Repo, tag: &str, file: &Path) -> Result<(), PortError>;
    /// Publishes the draft `tag` (GitHub then creates the tag).
    fn publish(&self, repo: &Repo, tag: &str) -> Result<(), PortError>;
}

/// What a verified attestation says, as the verifier read it from the signing certificate and
/// the statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attestation {
    /// The workflow that signed, `<owner>/<repo>/.github/workflows/<file>`.
    pub(crate) signer_workflow: String,
    /// The ref the signing run ran on (`refs/heads/main`).
    pub(crate) source_ref: String,
    /// The commit the signing run built.
    pub(crate) source_commit: String,
    /// The SHA-256 digests of the statement's subjects.
    pub(crate) subjects: Vec<String>,
}

/// Verifies the attestations of a file: online (the verifier fetches them), or offline from a
/// bundle file.
pub(crate) trait Verifier {
    /// The attestations of `file` that verify as attestations by `signer` of the source commit
    /// `commit`, read from `bundle` when one is given. An error is a file with no attestation
    /// that verifies, or a verifier that could not run.
    fn verify(
        &self,
        signer: &Signer,
        file: &Path,
        bundle: Option<&Path>,
        commit: &Commit,
    ) -> Result<Vec<Attestation>, PortError>;

    /// The answer of [`Verifier::verify`] to each ask, in the order asked. The default verifies
    /// them one after the other; an adapter whose verifications are independent programs runs
    /// them side by side (`release check` verifies every attested asset of a release twice).
    fn verify_each(&self, asks: &[Ask<'_>]) -> Vec<Result<Vec<Attestation>, PortError>> {
        asks.iter()
            .map(|a| self.verify(a.signer, a.file, a.bundle, a.commit))
            .collect()
    }
}

/// One verification [`Verifier::verify_each`] is asked for: the arguments of
/// [`Verifier::verify`].
#[derive(Clone, Copy)]
pub(crate) struct Ask<'a> {
    pub(crate) signer: &'a Signer,
    pub(crate) file: &'a Path,
    pub(crate) bundle: Option<&'a Path>,
    pub(crate) commit: &'a Commit,
}

/// What a program printed and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Runs a binary with `--version`.
pub(crate) trait RunVersion {
    fn version_of(&self, binary: &Path) -> Result<Said, PortError>;
}

/// Checks the dependencies in `Cargo.lock` against the advisory database
/// (`cargo deny check advisories`, read by `advisories`).
pub(crate) trait AdvisoryCheck {
    /// What the check printed and whether it exited 0. cargo-deny writes its JSON records to
    /// stderr.
    fn check_advisories(&self) -> Result<Said, PortError>;
}

/// Builds a macOS installer package from a directory tree (`pkgbuild`), wraps it as a
/// distribution package (`productbuild`), and reads a package's signature (`pkgutil`).
pub(crate) trait BuildPkg {
    /// Builds the component `pkg` from the tree `root`, installed at `/`, with the package identifier
    /// `identifier` and the version text `version`, every entry owned by root and wheel, and the
    /// scripts of the directory `scripts` (none when it is `None`).
    fn build_pkg(
        &self,
        root: &Path,
        scripts: Option<&Path>,
        identifier: &str,
        version: &str,
        pkg: &Path,
    ) -> Result<(), PortError>;

    /// Wraps the component pkg `component` as the one package of the distribution pkg `product`,
    /// with the product identifier `identifier` and version `version`; `product` is not signed.
    fn build_product(
        &self,
        component: &Path,
        identifier: &str,
        version: &str,
        product: &Path,
    ) -> Result<(), PortError>;

    /// What `pkgutil --check-signature` prints of the pkg `pkg`; an error when it exits other
    /// than 0 (an unsigned pkg, or a signature the Mac does not trust).
    fn check_signature(&self, pkg: &Path) -> Result<String, PortError>;
}

/// What one `codesign` run ended with: whether it exited 0, and everything it printed (it writes
/// `--display` text to stderr and requirements to stdout, so both streams, stdout first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Codesigned {
    pub(crate) success: bool,
    pub(crate) text: String,
}

/// What `codesign` says of a built Mach-O program (`code_signature::CodesignCli` is the adapter).
pub(crate) trait CodeSign {
    /// `codesign --verify --strict --verbose=2 <binary>`.
    fn verify(&self, binary: &Path) -> Result<Codesigned, PortError>;
    /// `codesign --display --verbose=2 <binary>`.
    fn display(&self, binary: &Path) -> Result<Codesigned, PortError>;
    /// `codesign --display --requirements - <binary>`.
    fn requirements(&self, binary: &Path) -> Result<Codesigned, PortError>;
}

/// The registry the coordinator's image is pushed to (`skopeo`, read by `image_push`).
pub(crate) trait Registry {
    /// Copies every image of the OCI layout `layout`, the one its `index.json` names `tag`, to
    /// `repository:tag`, keeping every digest (`skopeo copy --all --preserve-digests`): a
    /// manifest the copy would have to change fails it.
    fn copy(&self, layout: &Path, tag: &str, repository: &str) -> Result<(), PortError>;
    /// The bytes the registry serves for `repository@digest` (`skopeo inspect --raw`).
    fn served(&self, repository: &str, digest: &str) -> Result<Vec<u8>, PortError>;
}

/// The wait between two attempts of a verb that tries again.
pub(crate) trait Wait {
    /// Waits `secs` seconds after an attempt that failed for `why`.
    fn wait(&self, secs: u64, why: &str);
}

/// How a process ended: its status was success or not, and its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    pub(crate) success: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Starts a program with arguments in the current directory and waits for it.
pub(crate) trait Exec {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran>;

    /// Each run, a program and its arguments, answered in the order given. The default runs
    /// them one after the other; the process runner runs them side by side, each with a cache
    /// home of its own ([`spawn_side_by_side`]).
    fn spawn_each(&self, runs: &[(String, Vec<String>)]) -> Vec<std::io::Result<Ran>> {
        runs.iter()
            .map(|(program, args)| self.spawn(program, args))
            .collect()
    }
}

/// How many programs [`spawn_side_by_side`] runs at once.
pub(crate) const SIDE_BY_SIDE: usize = 8;

/// The environment variable [`spawn_side_by_side`] gives each run its cache home in
/// (`gh` keeps its Sigstore TUF metadata under `$XDG_CACHE_HOME/gh`).
const CACHE_HOME: &str = "XDG_CACHE_HOME";

/// The first number [`spawn_side_by_side`] tries for the directory of its cache homes.
static NEXT_HOMES: AtomicU64 = AtomicU64::new(0);

/// How many numbers the directory of the cache homes tries before the runs fail: far more than
/// the stale directories of one process id a temporary directory holds, and few enough that a
/// fault which makes every name look taken fails at once.
const HOMES_ATTEMPTS: u64 = 1024;

/// Each run started with `start(program, args, env)`, at most `width` (at least one) at once,
/// each on a thread of its own; the answers in the order of `runs`. A run whose thread panicked
/// is an error. `env` is what `start` sets in the run's environment, on top of this process's:
/// exactly `[(CACHE_HOME, <the run's cache home>)]`.
///
/// Every run has a cache home of its own: the new, empty directory `<index>` in a new directory
/// under `parent`, which is removed when the last run has ended. No two runs share a cache:
/// `gh attestation verify` writes the Sigstore TUF metadata it fetches into its cache, every
/// run anew, and two of them writing one cache at once fail each other (go-tuf reads each file
/// back after writing it, and refuses one the other `gh` rewrote in between). When the
/// directory of the homes cannot be made, every run is answered with that error and none is
/// started; a home that cannot be made is that run's error.
pub(crate) fn spawn_side_by_side<F>(
    runs: &[(String, Vec<String>)],
    width: usize,
    parent: &Path,
    start: F,
) -> Vec<std::io::Result<Ran>>
where
    F: Fn(&str, &[String], &[(&str, &Path)]) -> std::io::Result<Ran> + Sync,
{
    let stem = format!("xtask-cache-homes-{}", std::process::id());
    let homes = match new_dir(parent, &stem, &NEXT_HOMES, HOMES_ATTEMPTS) {
        Ok(base) => Removed(base),
        Err(e) => {
            let why = format!("make the cache homes under {}: {e}", parent.display());
            return runs
                .iter()
                .map(|_| Err(std::io::Error::new(e.kind(), why.clone())))
                .collect();
        }
    };
    let start = &start;
    let mut answers = Vec::with_capacity(runs.len());
    for (first, batch) in (0..).step_by(width.max(1)).zip(runs.chunks(width.max(1))) {
        std::thread::scope(|scope| {
            let started: Vec<_> = (first..)
                .zip(batch)
                .map(|(index, (program, args))| {
                    let home = homes.0.join(format!("{index}"));
                    scope.spawn(move || {
                        std::fs::create_dir(&home)
                            .and_then(|()| start(program, args, &[(CACHE_HOME, &home)]))
                    })
                })
                .collect();
            answers.extend(started.into_iter().map(|run| {
                run.join()
                    .unwrap_or_else(|_| Err(std::io::Error::other("the run's thread panicked")))
            }));
        });
    }
    answers
}

/// A directory removed, with everything in it, when this drops.
struct Removed(std::path::PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        // One that cannot be removed is left behind; no later batch uses its name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The new directory `parent/<stem>-<n>`, `n` taken from `next`, a fresh number for each try: a
/// directory that already exists is passed over, never shared or emptied; any other error is
/// returned, and so is `attempts` numbers all taken.
fn new_dir(
    parent: &Path,
    stem: &str,
    next: &AtomicU64,
    attempts: u64,
) -> std::io::Result<std::path::PathBuf> {
    for _ in 0..attempts {
        let path = parent.join(format!("{stem}-{}", next.fetch_add(1, Ordering::Relaxed)));
        // `create_dir`, not `create_dir_all`: a directory that already exists is an error, so
        // no batch ever shares one.
        match std::fs::create_dir(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            made => return made.map(|()| path),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("no free directory {stem}-<n> in {attempts} numbers"),
    ))
}

/// One run of a program that inherits nothing: its environment is exactly `env`, and it runs in
/// `cwd`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sealed {
    /// An absolute path.
    pub(crate) program: std::path::PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) cwd: std::path::PathBuf,
}

/// Starts a [`Sealed`] run and waits for it (the coordinator updater's offline `gh`).
pub(crate) trait ExecSealed {
    fn spawn_sealed(&self, run: &Sealed) -> std::io::Result<Ran>;
}

/// The ports one verb runs against.
#[derive(Clone, Copy)]
pub(crate) struct Ports<'a> {
    pub(crate) git: &'a dyn GitHistory,
    pub(crate) runs: &'a dyn GitHubRuns,
    pub(crate) store: &'a dyn ReleaseStore,
    pub(crate) verifier: &'a dyn Verifier,
    pub(crate) run_version: &'a dyn RunVersion,
    pub(crate) pkgbuild: &'a dyn BuildPkg,
    pub(crate) codesign: &'a dyn CodeSign,
    pub(crate) advisories: &'a dyn AdvisoryCheck,
    pub(crate) registry: &'a dyn Registry,
    pub(crate) wait: &'a dyn Wait,
}

/// What owns a set of adapters and lends them as [`Ports`]: the wired adapters (`Wired`, over
/// `real::SystemExec` outside a test) or the fakes of a test.
pub(crate) trait Adapters {
    fn ports(&self) -> Ports<'_>;
}

#[cfg(test)]
mod tests;
