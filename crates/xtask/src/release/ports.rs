//! The ports of the release verbs: everything they ask of git, of GitHub and of a binary, as
//! traits, so that every verb runs in a test against fakes (`fakes`).
//!
//! The adapters that answer them for real are `git::GitCli` ([`GitHistory`]), `github::Gh`
//! ([`GitHubRuns`], [`ReleaseStore`], [`Verifier`]), `check_binary::VersionOf`
//! ([`RunVersion`]), `macos_pkg::PkgbuildCli` ([`BuildPkg`], `pkgutil` too), `mdm_fetch::CurlCli`
//! ([`Download`]) and `advisories::DenyCli` ([`AdvisoryCheck`]). Each runs its program through an [`Exec`], and the
//! one [`Exec`] that starts a process, `real::SystemExec`, is built in `real` only and named by
//! no test.

use std::path::Path;

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

/// Downloads a file over HTTPS (`release mdm-fetch`).
pub(crate) trait Download {
    /// Writes what `url` (an `https://` URL) serves to `file`, replacing it; an error leaves
    /// nothing at `file` that the caller trusts (it checks the bytes after).
    fn download(&self, url: &str, file: &Path) -> Result<(), PortError>;
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
    pub(crate) advisories: &'a dyn AdvisoryCheck,
    pub(crate) download: &'a dyn Download,
}

/// What owns a set of adapters and lends them as [`Ports`]: the wired adapters (`Wired`, over
/// `real::SystemExec` outside a test) or the fakes of a test.
pub(crate) trait Adapters {
    fn ports(&self) -> Ports<'_>;
}
