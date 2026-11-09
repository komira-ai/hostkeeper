//! `channels check-apt`: the release installed the way a user installs it, from what was published
//! to the suite `testing`, before the release is promoted to `stable`. `.github/workflows/release.yml` runs
//! this verb in its install-check jobs, on hosted runners and at release time only.
//!
//! The verb runs in a clean Ubuntu container (the image is pinned by digest by the job). It adds
//! `packages.komira.ai` with the published keyring (a binary `.gpg`, referred to by a deb822
//! `Signed-By`) and the suite `testing`, installs the packages the spec lists as prerequisites
//! and then `<package>=<version>`, and holds the installed system to what the package promises
//! ([`spec::Package`]): the package is at the version, the binary says `<program>
//! <version>+<commit>`, every file the package ships is there, the user it creates exists, the
//! unit is enabled if the spec says the install enables it and is not otherwise, and no process
//! of the program runs.
//!
//! The programs (`apt-get`, `dpkg`, `dpkg-query`, `getent`, `curl`, the installed binary) run behind
//! [`Host`], so a test answers in place of them; `--root` is where the checked system is (`/`),
//! so a test points it at a scratch directory.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/check.rs`: `check_apt`), with the package's name, files, user and
//! unit made data ([`spec`]); the programs run through the crate's
//! [`Exec`](crate::release::ports::Exec) port.

use std::path::Path;

use super::aws::{Answer, run_program};
use super::host::Host;
use super::spec::{self, Package};
use super::{Error, is_plain_version, read};
use crate::release::contract::Commit;
use crate::release::ports::Exec;

/// Where the keyring is put (below the root) and the name the source file refers to it by.
pub(crate) const KEYRING: &str = "usr/share/keyrings/komira-archive-keyring.gpg";
const KEYRING_PATH: &str = "/usr/share/keyrings/komira-archive-keyring.gpg";

/// The source file (below the root).
pub(crate) const SOURCES: &str = "etc/apt/sources.list.d/komira.sources";

/// The keyring's file name in the repository.
pub(crate) const KEYRING_NAME: &str = "komira-archive-keyring.gpg";

/// The programs of the checked system, run through an [`Exec`] with the frontend of `debconf`
/// set to `noninteractive`, as a package install in a container needs.
pub(crate) struct Noninteractive<'a>(pub(crate) &'a dyn Exec);

impl Host for Noninteractive<'_> {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        let mut all = vec![
            "DEBIAN_FRONTEND=noninteractive".to_owned(),
            program.display().to_string(),
        ];
        all.extend(args.iter().cloned());
        run_program(self.0, Path::new("env"), &all)
    }
}

pub(crate) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// Runs `program <args>`; its standard output, or why it failed.
pub(super) fn must(host: &dyn Host, program: &str, args: &[&str]) -> Result<String, Error> {
    let answer = host.run(Path::new(program), &strings(args))?;
    if answer.ok {
        return Ok(answer.stdout);
    }
    Err(Error::Failed(format!(
        "{program} {} failed: {}",
        args.join(" "),
        answer.stderr.trim_end()
    )))
}

pub(super) fn version_flag(version: &str) -> Result<(), Error> {
    if is_plain_version(version) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "--version {version:?} is not <major>.<minor>.<patch>"
        )))
    }
}

/// The package called `name` of the product, or a usage error naming the packages there are.
pub(super) fn package_flag(name: &str) -> Result<&'static Package, Error> {
    spec::HOSTKEEPER.package(name).ok_or_else(|| {
        let names: Vec<&str> = spec::HOSTKEEPER.packages.iter().map(|p| p.name).collect();
        Error::Usage(format!("--package {name:?} is none of {names:?}"))
    })
}

/// An `https` URL without a trailing `/`.
pub(super) fn url_flag(flag: &str, url: &str) -> Result<(), Error> {
    if url.starts_with("https://") && !url.ends_with('/') {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{flag} {url:?} is not an https URL without a trailing /"
        )))
    }
}

/// The text the program of `package` prints for `--version`.
pub(super) fn stamp(package: &Package, version: &str, commit: &str) -> Result<String, Error> {
    let commit: Commit = commit
        .parse()
        .map_err(|e| Error::Usage(format!("--commit {commit:?}: {e}")))?;
    Ok(format!(
        "{} {version}+{}",
        package.program(),
        commit.short()
    ))
}

/// The binary of `package` says the release it is.
pub(super) fn says(host: &dyn Host, package: &Package, stamp: &str) -> Result<String, Error> {
    let said = must(host, package.binary, &["--version"])?;
    if said.strip_suffix('\n').unwrap_or(&said) != stamp {
        return Err(Error::Failed(format!(
            "the binary says {said:?}, not {stamp:?}"
        )));
    }
    Ok(stamp.to_owned())
}

/// The problem, if the install did not leave the unit as the spec says: the links that enable
/// it, as found.
pub(super) fn enabled_problems(package: &Package, links: &[String]) -> Vec<String> {
    if package.enabled_after_install {
        if links.is_empty() {
            return vec![format!(
                "the service is not enabled: no link to {} is in etc/systemd/system, and the package enables it on a first install",
                package.unit
            )];
        }
        return Vec::new();
    }
    links
        .iter()
        .map(|link| {
            format!("the service is enabled: {link} exists, and the package enables nothing")
        })
        .collect()
}

/// The problems of a process of the program running: the processes, as found.
pub(super) fn running_problems(pids: &[String]) -> Vec<String> {
    pids.iter()
        .map(|pid| {
            format!("the service is running (process {pid}), and the package starts nothing")
        })
        .collect()
}

/// What `check-apt` is asked.
pub(crate) struct Apt<'a> {
    /// The system to check: `/`, or a scratch directory in a test.
    pub(crate) root: &'a Path,
    pub(crate) package: &'a Package,
    /// The Debian architecture of the machine, `amd64` or `arm64`.
    pub(crate) arch: &'a str,
    pub(crate) version: &'a str,
    pub(crate) commit: &'a str,
    /// The repository, `https://packages.komira.ai/apt`.
    pub(crate) apt_url: &'a str,
}

/// The deb822 source of the suite `testing`, signed by the keyring alone.
pub(crate) fn source(apt_url: &str, arch: &str) -> String {
    format!(
        "Types: deb\nURIs: {apt_url}\nSuites: testing\nComponents: main\nArchitectures: {arch}\nSigned-By: {KEYRING_PATH}\n"
    )
}

/// The links below `etc/systemd/system` that enable the unit: the unit as the administrator's
/// own, and the unit in a `wants` or `requires` directory.
fn enabled_links(root: &Path, unit: &str) -> Vec<String> {
    let base = root.join("etc/systemd/system");
    let mut found = Vec::new();
    if base.join(unit).symlink_metadata().is_ok() {
        found.push(base.join(unit));
    }
    for dir in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let link = dir.path().join(unit);
        if dir.path().is_dir() && link.symlink_metadata().is_ok() {
            found.push(link);
        }
    }
    found.sort();
    found
        .iter()
        .map(|link| link.display().to_string())
        .collect()
}

/// The processes (by `comm`) named `comm` below `root/proc`.
fn running(root: &Path, comm: &str) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root.join("proc"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if name.trim_end() == comm {
            found.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    found.sort();
    found
}

/// The checks of an installed system, as problems.
fn installed_problems(r: &Apt, installed: &str, user_exists: Option<bool>) -> Vec<String> {
    let package = r.package;
    let mut problems = Vec::new();
    if installed != r.version {
        problems.push(format!(
            "dpkg says {} {installed:?} is installed, not {:?}",
            package.name, r.version
        ));
    }
    for file in package.files {
        if !r.root.join(file.trim_start_matches('/')).is_file() {
            problems.push(format!("{file} is not installed"));
        }
    }
    if let (Some(user), Some(false)) = (package.user, user_exists) {
        problems.push(format!("the user {user} does not exist"));
    }
    problems.extend(enabled_problems(
        package,
        &enabled_links(r.root, package.unit),
    ));
    problems.extend(running_problems(&running(r.root, package.comm())));
    problems
}

/// The `check-apt` verb: the lines to print.
///
/// # Errors
/// A flag out of its form, a program that fails (naming it and what it said), a keyring that is
/// not a binary keyring, a machine of another architecture, a binary that does not say the
/// release, or one problem per way the installed system is not what the package promises.
pub(crate) fn check_apt(host: &dyn Host, r: &Apt) -> Result<Vec<String>, Error> {
    version_flag(r.version)?;
    if spec::arch_of_deb(r.arch).is_none() {
        return Err(Error::Usage(format!(
            "--arch {:?} is not amd64 or arm64",
            r.arch
        )));
    }
    url_flag("--apt-url", r.apt_url)?;
    let stamp = stamp(r.package, r.version, r.commit)?;
    let machine = must(host, "dpkg", &["--print-architecture"])?;
    if machine.trim() != r.arch {
        return Err(Error::Failed(format!(
            "this machine is {}, not {}: the check would install another package",
            machine.trim(),
            r.arch
        )));
    }
    // A clean image has no certificates: the first update goes over the mirrors' plain HTTP.
    must(host, "apt-get", &["update"])?;
    must(
        host,
        "apt-get",
        &[
            "install",
            "-y",
            "--no-install-recommends",
            "ca-certificates",
            "curl",
        ],
    )?;
    let keyring = r.root.join(KEYRING);
    let keyring_arg = keyring.display().to_string();
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
            "--create-dirs",
            "--output",
            &keyring_arg,
            &format!("{}/{KEYRING_NAME}", r.apt_url),
        ],
    )?;
    let bytes = read(&keyring)?;
    // A binary OpenPGP keyring starts with a packet header (bit 7 set); an armored one is text
    // that a deb822 `Signed-By` path does not accept.
    if bytes.first().is_none_or(|b| b & 0x80 == 0) {
        return Err(Error::Failed(format!(
            "{KEYRING_NAME} is not a binary keyring: apt takes a .gpg file in Signed-By, not armor"
        )));
    }
    let sources = r.root.join(SOURCES);
    let dir = sources.parent().unwrap_or(&sources);
    std::fs::create_dir_all(dir).map_err(Error::spawn(format!("create {}", dir.display())))?;
    std::fs::write(&sources, source(r.apt_url, r.arch))
        .map_err(Error::spawn(format!("write {}", sources.display())))?;
    must(host, "apt-get", &["update"])?;
    for prerequisite in r.package.prerequisites {
        must(
            host,
            "apt-get",
            &["install", "-y", "--no-install-recommends", prerequisite],
        )?;
    }
    let target = format!("{}={}", r.package.name, r.version);
    must(
        host,
        "apt-get",
        &["install", "-y", "--no-install-recommends", &target],
    )?;
    let installed = must(
        host,
        "dpkg-query",
        &["--show", "--showformat=${Version}", r.package.name],
    )?;
    let said = says(host, r.package, &stamp)?;
    let user_exists = match r.package.user {
        Some(user) => Some(
            host.run(Path::new("getent"), &strings(&["passwd", user]))?
                .ok,
        ),
        None => None,
    };
    let problems = installed_problems(r, &installed, user_exists);
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-apt: {} {} installed from the suite testing on {}; it says {said:?}; its {} files are there; {}",
        r.package.name,
        r.version,
        r.arch,
        r.package.files.len(),
        outcome(r.package)
    )])
}

/// What the check found the install to have done to the service, for the line it prints.
pub(super) fn outcome(package: &Package) -> String {
    let user = package
        .user
        .map_or(String::new(), |u| format!("the user {u} exists; "));
    let enabled = if package.enabled_after_install {
        "the service is enabled"
    } else {
        "the service is not enabled"
    };
    format!("{user}{enabled} and not running")
}

#[cfg(test)]
mod tests;
