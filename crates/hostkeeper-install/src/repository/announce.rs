//! When a daemon may announce a repository installer (packages-repositories.md section 4.1,
//! packages.md section 5.4), decided once at its start: `apt` on a host of the `deb` family
//! whose apt tools are present, whose installed `apt` is 2.2 or later (the first with
//! `apt-get update --error-on=any`, without which an update that fails its signature check
//! still exits 0), and which has at least one repository entry (every entry passed the checks
//! of [`super::config`], or the daemon did not start). A host announces at most one native
//! installer, the one of its own package database.
//!
//! The daemon announces it because its build registers the installer
//! ([`crate::REGISTERED`]) and its order path runs it: [`announced`] adds the host's native
//! installer only when `registered` holds it, so the coordinator never orders an entry the host
//! would defer.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use hostkeeper_release::Installer;

use super::apt::version::compare;
use super::config::Repositories;
use crate::announce::{Announcement, Family};

/// The oldest apt announced.
pub const APT_FLOOR: &str = "2.2";

/// The tools `apt` needs, relative to the host's root.
pub const APT_TOOLS: [&str; 6] = [
    "usr/bin/apt-get",
    "usr/bin/apt-cache",
    "usr/bin/apt-mark",
    "usr/bin/dpkg",
    "usr/bin/dpkg-deb",
    "usr/bin/dpkg-query",
];

/// The dpkg database's status file, relative to the host's root.
pub const DPKG_STATUS: &str = "var/lib/dpkg/status";

/// The version of the installed package `apt` in the dpkg status file at `path`, read a line
/// at a time; `None` when the file cannot be read or holds no installed `apt`.
pub fn installed_apt(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    let (mut package, mut status, mut version) = (false, false, None);
    for line in reader.lines() {
        let line = line.ok()?;
        if line.is_empty() {
            if package & status {
                return version;
            }
            (package, status, version) = (false, false, None);
            continue;
        }
        match line.split_once(": ") {
            Some(("Package", name)) => package = name == "apt",
            Some(("Status", value)) => status = value == "install ok installed",
            Some(("Version", value)) => version = Some(value.to_owned()),
            _ => {}
        }
    }
    (package & status).then_some(version).flatten()
}

/// The native repository installer the host under `root` may run, if any.
pub fn native(
    family: Option<Family>,
    root: &Path,
    repositories: &Repositories,
) -> Option<Installer> {
    if repositories.is_empty() {
        return None;
    }
    capable(family, root)
}

/// The native repository installer the host under `root` could run were it given an entry: the
/// one [`native`] names for a host that has one. The daemon decides it once at its start, and
/// announces it once a set of repositories it was sent leaves it an entry.
pub fn capable(family: Option<Family>, root: &Path) -> Option<Installer> {
    if family != Some(Family::Deb) {
        return None;
    }
    let tools = APT_TOOLS.iter().all(|tool| root.join(tool).is_file());
    let recent = installed_apt(&root.join(DPKG_STATUS))
        .and_then(|found| compare(&found, APT_FLOOR))
        .is_some_and(|order| order.is_ge());
    (tools & recent).then_some(Installer::Apt)
}

/// `announcement` with the native installer `native` added, when `registered` (the
/// installers the build has the steps of) holds it.
pub fn announced(
    mut announcement: Announcement,
    native: Option<Installer>,
    registered: &[Installer],
) -> Announcement {
    if let Some(installer) = native.filter(|i| registered.contains(i)) {
        announcement.installers.push(installer);
    }
    announcement
}
