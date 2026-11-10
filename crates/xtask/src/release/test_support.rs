//! What this crate's integration tests need of its internals. It is built only with the feature
//! `test-support`, which only those tests turn on (the crate's dev-dependency on itself).

use std::path::Path;

use clap::ValueEnum;

use super::contract::Version;
use super::package::linux_check;
use super::package::nfpm_stage::{self, Arch};

/// The command line of `cargo xtask`, every verb with its flags, as the binary parses it: the
/// workflow lint holds every `cargo xtask` call of a workflow to it.
pub fn cli() -> clap::Command {
    <crate::Cli as clap::CommandFactory>::command()
}

/// The command line of `hostkeeper-coordinator-update`, as the binary parses it: the committed
/// unit's command is held to it.
pub fn coordinator_update_cli() -> clap::Command {
    <crate::UpdateCli as clap::CommandFactory>::command()
}

/// The file names `release nfpm-stage` writes into its `--out`: `gh`, the trusted root, then
/// the daemon.
pub const NFPM_STAGED: [&str; 3] = [
    nfpm_stage::GH_FILE,
    nfpm_stage::TRUSTED_ROOT_FILE,
    nfpm_stage::HOSTKEEPER_FILE,
];

/// Where `release check-linux-pkg` requires a deb and an rpm to install `gh`, then the trusted
/// root.
pub const LINUX_PKG_INSTALLED: [&str; 2] = [
    linux_check::INSTALLED_GH,
    linux_check::INSTALLED_TRUSTED_ROOT,
];

/// The program `release check-linux-pkg --staged` requires a deb and an rpm to
/// install, where it is installed and the file name `release nfpm-stage` stages it as.
pub const LINUX_PKG_PROGRAMS: [(&str, &str); 1] = [(
    linux_check::INSTALLED_HOSTKEEPER,
    nfpm_stage::HOSTKEEPER_FILE,
)];

/// The console's issue link `release check-linux-pkg` requires a deb and an rpm to install, and
/// what it must point at.
pub const LINUX_PKG_LINK: (&str, &str) =
    (linux_check::INSTALLED_ISSUE_LINK, linux_check::ISSUE_TARGET);

/// The path of `gh` in its release archive of `gh_version` for `arch` (`amd64` or `arm64`).
pub fn gh_member(gh_version: &str, arch: &str) -> Result<String, String> {
    let version = gh_version.parse::<Version>().map_err(|e| e.to_string())?;
    let arch = Arch::from_str(arch, false)?;
    Ok(nfpm_stage::gh_member(&version, arch).display().to_string())
}

/// Stages `gh`, the trusted root and the two programs of `binaries` as `cargo xtask release
/// nfpm-stage` does, with its flags' values. Returns the verb's lines, or why it refused.
pub fn nfpm_stage(
    arch: &str,
    gh_version: &str,
    gh_archive: &Path,
    gh_sha256: &str,
    trusted_root: &Path,
    binaries: &Path,
    out: &Path,
) -> Result<Vec<String>, String> {
    let request = nfpm_stage::Request {
        arch: Arch::from_str(arch, false)?,
        gh_version: gh_version.parse::<Version>().map_err(|e| e.to_string())?,
        gh_archive: gh_archive.to_path_buf(),
        gh_sha256: gh_sha256
            .parse()
            .map_err(|e: crate::install::NotASha256| e.to_string())?,
        trusted_root: trusted_root.to_path_buf(),
        binaries: binaries.to_path_buf(),
        out: out.to_path_buf(),
    };
    nfpm_stage::stage(&request).map_err(|e| e.to_string())
}
