//! What this crate's integration tests need of its internals. It is built only with the feature
//! `test-support`, which only those tests turn on (the crate's dev-dependency on itself).

use std::path::Path;

use clap::ValueEnum;

use super::contract::{Commit, Platform, Service, Version};
use super::package::linux_check;
use super::package::nfpm_stage::{self, Arch};
use super::package::{Request, package_dated};

/// The command line of `cargo xtask`, every verb with its flags, as the binary parses it: the
/// workflow lint holds every `cargo xtask` call of a workflow to it.
pub fn cli() -> clap::Command {
    <crate::Cli as clap::CommandFactory>::command()
}

/// The file names `release nfpm-stage` writes into its `--out`: `gh`, the trusted root, then
/// the daemon and the boot server.
pub const NFPM_STAGED: [&str; 4] = [
    nfpm_stage::GH_FILE,
    nfpm_stage::TRUSTED_ROOT_FILE,
    nfpm_stage::HOSTKEEPER_FILE,
    nfpm_stage::PXE_FILE,
];

/// Where `release check-linux-pkg` requires a deb and an rpm to install `gh`, then the trusted
/// root.
pub const LINUX_PKG_INSTALLED: [&str; 2] = [
    linux_check::INSTALLED_GH,
    linux_check::INSTALLED_TRUSTED_ROOT,
];

/// The programs `release check-linux-pkg --staged` requires a deb, an rpm and an Arch package to
/// install, each where it is installed and the file name `release nfpm-stage` stages it as.
pub const LINUX_PKG_PROGRAMS: [(&str, &str); 2] = [
    (
        linux_check::INSTALLED_HOSTKEEPER,
        nfpm_stage::HOSTKEEPER_FILE,
    ),
    (linux_check::INSTALLED_PXE, nfpm_stage::PXE_FILE),
];

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

/// Builds the linux-x86_64 hostkeeper package of `version` as `cargo xtask release package`
/// does, from `binary`, `unit` and `trusted_root`, into `out`, with every time in the archive
/// `mtime` (the verb takes it from the commit in git). Returns the verb's lines, or why it
/// refused.
pub fn linux_package(
    version: &str,
    binary: &Path,
    unit: &Path,
    trusted_root: &Path,
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
        trusted_root: Some(trusted_root.to_path_buf()),
        archives: None,
        out: out.to_path_buf(),
    };
    package_dated(&request, || Ok(mtime)).map_err(|e| e.to_string())
}

/// The hook `release deb` and `release macos-pkg --service hello` put in hello's packages, by
/// the absolute path each installs it at, with the program path it runs: the `.deb`'s and the
/// `.pkg`'s.
pub fn hello_hooks() -> Vec<(String, String, String)> {
    use super::native::hello::{self, File};
    let pick = |files: Vec<File>, program: &str| {
        let hook = files
            .iter()
            .find(|f| f.path.ends_with(super::native::HOOK))
            .map(|f| {
                (
                    f.path.clone(),
                    String::from_utf8_lossy(&f.data).into_owned(),
                )
            });
        hook.map(|(path, text)| (path, text, program.to_owned()))
    };
    let mut out = Vec::new();
    out.extend(pick(
        hello::deb_files(Vec::new(), false),
        hello::DEB_PROGRAM,
    ));
    if let Ok(files) = hello::pkg_files(Vec::new(), false) {
        out.extend(pick(files, hello::PKG_PROGRAM));
    }
    out
}
