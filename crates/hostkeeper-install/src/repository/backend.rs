//! What differs per package manager (packages-repositories.md section 4): the commands that read
//! the index, simulate, download, install, hold, query and purge, the layout of the work
//! directory and of the version directory's copy, and how the commands' output maps onto
//! section 4.5's reasons. The installer kind ([`super::RepositoryInstaller`]) runs them in its
//! order, with its records, its lock, its tree and its enablement, the same for every
//! manager; [`super::apt::Apt`] is the backend designed here, and a `dnf` backend (A8) is the
//! next.

use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_release::Installer;

use super::config::Repository;
use super::outcome::Failure;
use super::tools::Tools;

/// The stanza of one package version in the repository's signed index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stanza {
    /// The SHA-256 of the package file, lower-case hex.
    pub sha256: String,
    /// The package file's size in bytes.
    pub size: u64,
}

/// A package file's identity, as its control fields give it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub package: String,
    pub version: String,
    pub architecture: String,
}

/// A package as the host's package database shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The version the database holds.
    pub version: String,
    /// Whether it is fully installed (`install ok installed`), not half-installed or
    /// half-configured.
    pub complete: bool,
}

/// How a purge ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Purged {
    /// The package is gone, or was never there.
    Done,
    /// The package manager's lock is held; the purge may be tried again.
    Busy(String),
}

/// One package manager's side of the repository installer.
pub trait Backend: Send + Sync {
    /// The installer a host announces for this backend.
    fn installer(&self) -> Installer;

    /// The manager's own grammars of a package's name and version; `Err` names the field.
    fn check_names(&self, package: &str, version: &str) -> Result<(), String>;

    /// The host's architecture, as the manager names it (`amd64`).
    fn architecture(&self, tools: &dyn Tools, bound: u64) -> Result<String, Failure>;

    /// Makes the work directory `work` (empty and owned by the daemon) see `repository` alone.
    fn prepare(&self, work: &Path, repository: &Repository) -> io::Result<()>;

    /// Fetches and verifies the repository's index into `work`. One of the two fetch commands.
    fn update(&self, tools: &dyn Tools, work: &Path, bound: u64) -> Result<(), Failure>;

    /// The index's stanza of `package` at `version` for `architecture` (or the manager's
    /// architecture-free one); `Ok(None)` when it lists none.
    fn stanza(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        architecture: &str,
        bound: u64,
    ) -> Result<Option<Stanza>, Failure>;

    /// The dependency check before anything is downloaded: the install of `package` at
    /// `version` from `work` must change no other package.
    fn simulate(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
    ) -> Result<(), Failure>;

    /// Downloads the package file into `work`, checked against the index; answers its path. The
    /// other fetch command.
    fn download(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
    ) -> Result<PathBuf, Failure>;

    /// The control fields of the package file `file`.
    fn fields(&self, tools: &dyn Tools, file: &Path, bound: u64) -> Result<Fields, Failure>;

    /// Extracts the package's files into the empty directory `tree`, running no script.
    fn extract(
        &self,
        tools: &dyn Tools,
        file: &Path,
        tree: &Path,
        bound: u64,
    ) -> Result<(), Failure>;

    /// The directory of the version directory the backend keeps its copy in (`apt`).
    fn meta_dir(&self) -> &'static str;

    /// Copies what a later `activate` needs (the entry, the verified index and the package
    /// `file`) from `work` into the version directory's `meta`.
    fn keep(&self, work: &Path, file: &Path, meta: &Path, repository: &str) -> io::Result<()>;

    /// Fills the empty work directory `work` from a version directory's `meta`, so the install
    /// finds the index and the package with no fetch.
    fn restore(&self, meta: &Path, work: &Path) -> io::Result<()>;

    /// Installs `package` at `version` from `work`, with no download: an upgrade, a downgrade
    /// or the same version again.
    fn install(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
    ) -> Result<(), Failure>;

    /// `package` as the host's database shows it; `None` when it is not installed.
    fn installed(
        &self,
        tools: &dyn Tools,
        package: &str,
        bound: u64,
    ) -> Result<Option<Installed>, Failure>;

    /// Holds (`true`) or releases (`false`) `package`, so nothing else upgrades it.
    fn hold(&self, tools: &dyn Tools, package: &str, hold: bool, bound: u64)
    -> Result<(), Failure>;

    /// Purges `package`, its configuration files with it, whatever version is installed.
    fn purge(&self, tools: &dyn Tools, package: &str, bound: u64) -> Result<Purged, Failure>;
}
