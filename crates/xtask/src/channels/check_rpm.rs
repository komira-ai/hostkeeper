//! `channels check-rpm`: the release installed the way a user installs it with `dnf`, from what
//! was published to the suite `testing`, before anything public changes.
//! `.github/workflows/release.yml` runs this verb in the job `check-rpm`, on a hosted runner of
//! each architecture and in a clean container of each distribution (pinned by digest).
//!
//! The verb runs on the runner and the system it checks is the container: every program runs in
//! it with `docker exec <container>` ([`Container`]). (The `xtask` binary of the runner is built
//! against a newer C library than these distributions have, so it cannot run in them.) A test
//! hands [`Host`] a stand-in instead.
//!
//! In the container it writes the repository file of the suite `testing` (`gpgcheck=1` and
//! `repo_gpgcheck=1`, the key at `<url>/apt/komira-archive-keyring.asc`), installs the packages
//! the spec lists as prerequisites and then `<package>-<version>` with `dnf` (which verifies the
//! signature of the repository metadata and of the package, and imports the key), and holds the
//! installed system to what the package promises ([`Package`]): the package is at the version
//! and release, `rpm -K` of the package downloaded from the pool says its digests and signatures
//! are OK, the binary says `<program> <version>+<commit>`, every file the package ships is
//! there, the user it creates exists, the unit is enabled if the spec says the install enables
//! it and is not otherwise, and no process of the program runs.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/check_rpm.rs`), with the package's name, files, user and unit made
//! data ([`super::spec`]); `docker` runs through the crate's
//! [`Exec`](crate::release::ports::Exec) port. Tierstore's conffile checks are gone: neither
//! package ships a file under `/etc`.

use std::path::{Path, PathBuf};

use super::aws::{Answer, run_program};
use super::check::{
    enabled_problems, must, outcome, running_problems, says, stamp, strings, url_flag, version_flag,
};
use super::host::Host;
use super::spec::{self, Package};
use super::{Error, rpm};
use crate::release::ports::Exec;

/// The file of the repository in the container.
pub(crate) const REPO_FILE: &str = "/etc/yum.repos.d/komira-testing.repo";

/// The key's path on the repository host (the same key signs the apt and the rpm tree).
pub(crate) const KEY_PATH: &str = "apt/komira-archive-keyring.asc";

/// A container: every program runs in it, through `docker exec`.
pub(crate) struct Container<'a> {
    pub(crate) exec: &'a dyn Exec,
    pub(crate) docker: PathBuf,
    pub(crate) name: String,
}

impl Host for Container<'_> {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        let mut all = vec![
            "exec".to_owned(),
            self.name.clone(),
            program.display().to_string(),
        ];
        all.extend(args.iter().cloned());
        run_program(self.exec, &self.docker, &all)
    }
}

/// What `check-rpm` is asked.
pub(crate) struct Rpm<'a> {
    pub(crate) package: &'a Package,
    /// The RPM architecture of the machine, `x86_64` or `aarch64`.
    pub(crate) arch: &'a str,
    pub(crate) version: &'a str,
    pub(crate) commit: &'a str,
    /// The repository host, `https://packages.komira.ai`.
    pub(crate) url: &'a str,
    /// A directory of the container to download the package to.
    pub(crate) work: &'a str,
}

/// The repository file of the suite `testing`: signed metadata and signed packages, no other.
pub(crate) fn repo_file(url: &str) -> String {
    format!(
        "[komira-testing]\nname=komira-ai packages (testing)\nbaseurl={url}/rpm/testing/$basearch\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey={url}/{KEY_PATH}\n"
    )
}

/// The lines `sh -c <script> sh <arg>` prints; the script ends in `true`, so that a miss is an
/// empty answer and not a failure.
fn lines_of(host: &dyn Host, script: &str, arg: &str) -> Result<Vec<String>, Error> {
    let said = must(host, "sh", &["-c", script, "sh", arg])?;
    Ok(said.lines().map(str::to_owned).collect())
}

/// The `check-rpm` verb: the lines to print.
///
/// # Errors
/// A flag out of its form, a program that fails (naming it and what it said), a machine of
/// another architecture, a package whose signature `rpm -K` does not accept, a binary that does
/// not say the release, or one problem per way the installed system is not what the package
/// promises.
pub(crate) fn check_rpm(host: &dyn Host, r: &Rpm) -> Result<Vec<String>, Error> {
    version_flag(r.version)?;
    let package = r.package;
    let Some(arch) = spec::arch_of_rpm(r.arch) else {
        return Err(Error::Usage(format!(
            "--arch {:?} is not x86_64 or aarch64",
            r.arch
        )));
    };
    url_flag("--url", r.url)?;
    if !r.work.starts_with('/') || r.work.ends_with('/') {
        return Err(Error::Usage(format!(
            "--work {:?} is not an absolute path without a trailing /",
            r.work
        )));
    }
    let stamp = stamp(package, r.version, r.commit)?;
    let machine = must(host, "uname", &["-m"])?;
    if machine.trim() != r.arch {
        return Err(Error::Failed(format!(
            "this machine is {}, not {}: the check would install another package",
            machine.trim(),
            r.arch
        )));
    }
    must(
        host,
        "sh",
        &[
            "-c",
            "printf '%s' \"$1\" > \"$2\"",
            "sh",
            &repo_file(r.url),
            REPO_FILE,
        ],
    )?;
    for prerequisite in package.prerequisites {
        must(host, "dnf", &["install", "-y", prerequisite])?;
    }
    let target = format!("{}-{}", package.name, r.version);
    must(host, "dnf", &["install", "-y", &target])?;
    let installed = must(
        host,
        "rpm",
        &["-q", "--qf", "%{VERSION}-%{RELEASE}.%{ARCH}", package.name],
    )?;
    let file = package.rpm_file(r.version, arch);
    let wanted = format!("{}-{}.{}", r.version, spec::RPM_RELEASE, r.arch);
    let mut problems = Vec::new();
    if installed != wanted {
        problems.push(format!(
            "rpm says {} {installed:?} is installed, not {wanted:?}",
            package.name
        ));
    }
    let key = format!("{}/{KEY_PATH}", r.url);
    must(host, "rpm", &["--import", &key])?;
    let local = format!("{}/{file}", r.work);
    must(
        host,
        "curl",
        &[
            "--proto",
            "=https",
            "--tlsv1.2",
            "--fail",
            "--silent",
            "--show-error",
            "--output",
            &local,
            &format!("{}/rpm/{}/{file}", r.url, rpm::state::POOL_DIR),
        ],
    )?;
    let checked = must(host, "rpm", &["-K", &local])?;
    if !checked.contains("signatures OK") || checked.contains("NOT OK") {
        problems.push(format!(
            "rpm -K says {:?}, not that the digests and signatures are OK",
            checked.trim()
        ));
    }
    let said = says(host, package, &stamp)?;
    for path in package.files {
        let args = ["-c", "test -f \"$1\"", "sh", path].map(str::to_owned);
        if !host.run(Path::new("sh"), &args)?.ok {
            problems.push(format!("{path} is not installed"));
        }
    }
    if let Some(user) = package.user
        && !host
            .run(Path::new("getent"), &strings(&["passwd", user]))?
            .ok
    {
        problems.push(format!("the user {user} does not exist"));
    }
    let links = lines_of(
        host,
        "ls /etc/systemd/system/*.wants/\"$1\" /etc/systemd/system/\"$1\" 2>/dev/null; true",
        package.unit,
    )?;
    problems.extend(enabled_problems(package, &links));
    let pids = lines_of(
        host,
        "grep -l -x \"$1\" /proc/[0-9]*/comm 2>/dev/null | cut -d/ -f3; true",
        package.comm(),
    )?;
    problems.extend(running_problems(&pids));
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-rpm: {} {} installed from the suite testing on {}; rpm -K says OK; it says {said:?}; its {} files are there; {}",
        package.name,
        r.version,
        r.arch,
        package.files.len(),
        outcome(package)
    )])
}

#[cfg(test)]
mod tests;
