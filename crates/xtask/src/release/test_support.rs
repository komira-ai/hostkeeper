//! What this crate's integration tests need of its internals. It is built only with the feature
//! `test-support`, which only those tests turn on (the crate's dev-dependency on itself).

use std::path::Path;

use clap::ValueEnum;

use super::contract::{Commit, Platform, Service, Version};
use super::package::linux_check;
use super::package::nfpm_stage::{self, Arch};
use super::package::{Request, package_dated};

/// The file names `release nfpm-stage` writes into its `--out`: `gh`, then the trusted root.
pub const NFPM_STAGED: [&str; 2] = [nfpm_stage::GH_FILE, nfpm_stage::TRUSTED_ROOT_FILE];

/// Where `release check-linux-pkg` requires a deb and an rpm to install `gh`, then the trusted
/// root.
pub const LINUX_PKG_INSTALLED: [&str; 2] = [
    linux_check::INSTALLED_GH,
    linux_check::INSTALLED_TRUSTED_ROOT,
];

/// The path of `gh` in its release archive of `gh_version` for `arch` (`amd64` or `arm64`).
pub fn gh_member(gh_version: &str, arch: &str) -> Result<String, String> {
    let version = gh_version.parse::<Version>().map_err(|e| e.to_string())?;
    let arch = Arch::from_str(arch, false)?;
    Ok(nfpm_stage::gh_member(&version, arch).display().to_string())
}

/// Stages `gh` and the trusted root as `cargo xtask release nfpm-stage` does, with its flags'
/// values. Returns the verb's lines, or why it refused.
pub fn nfpm_stage(
    arch: &str,
    gh_version: &str,
    gh_archive: &Path,
    gh_sha256: &str,
    trusted_root: &Path,
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
        out: out.to_path_buf(),
    };
    nfpm_stage::stage(&request).map_err(|e| e.to_string())
}

/// Builds the linux-x86_64 hostkeeper package of `version` as `cargo xtask release package`
/// does, from `binary` and `unit`, into `out`, with every time in the archive `mtime` (the
/// verb takes it from the commit in git). Returns the verb's lines, or why it refused.
pub fn linux_package(
    version: &str,
    binary: &Path,
    unit: &Path,
    out: &Path,
    mtime: u64,
) -> Result<Vec<String>, String> {
    let request = Request {
        service: Service::Hostkeeper,
        platform: Platform::LinuxX86_64,
        version: version.parse::<Version>().map_err(|e| e.to_string())?,
        commit: "0"
            .repeat(40)
            .parse::<Commit>()
            .map_err(|e| e.to_string())?,
        binary: binary.to_path_buf(),
        unit: Some(unit.to_path_buf()),
        fail_ready: false,
        out: out.to_path_buf(),
    };
    package_dated(&request, || Ok(mtime)).map_err(|e| e.to_string())
}
