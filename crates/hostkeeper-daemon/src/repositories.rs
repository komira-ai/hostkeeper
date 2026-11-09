//! The host's repository configuration at start, and the repository installer the daemon
//! announces from it (packages-repositories.md sections 3.1 and 4.1).
//!
//! The directory of repository entries is read once, at the start, by
//! [`hostkeeper_install::repository::config::load`] as root's: a defect in it refuses the start
//! ([`StartError::Repositories`]) and a missing or empty directory is no repository. A Mac has
//! no repository entries and never reads the directory.
//!
//! This build registers no repository installer ([`hostkeeper_install::REGISTERED`]), so
//! [`announced`] never adds one to the list: a coordinator never orders an entry this daemon
//! would defer. It says on standard error once when the host could run one.

use std::path::Path;

use hostkeeper_install::Announcement;
use hostkeeper_install::REGISTERED;
use hostkeeper_install::repository::Repositories;
use hostkeeper_install::repository::announce::{announced as with_native, native};
use hostkeeper_install::repository::config;

use crate::cli::HostFiles;
use crate::start::StartError;

/// The entries of the repositories directory of `files`; none when `files` has no such directory
/// (a Mac) or the directory is not there. `Err` names the file that breaks a rule.
pub fn load(files: &HostFiles) -> Result<Repositories, StartError> {
    match &files.repositories {
        None => Ok(Repositories::default()),
        Some(path) => config::load(path, 0).map_err(|source| StartError::Repositories {
            path: path.clone(),
            source,
        }),
    }
}

/// `announcement` with the host's native repository installer added when the build registers
/// its steps ([`hostkeeper_install::repository::announce::announced`]). When the host could run
/// one that this build does not register, a line on standard error says so.
pub fn announced(
    announcement: Announcement,
    os_root: &Path,
    repositories: &Repositories,
) -> Announcement {
    let found = native(announcement.family, os_root, repositories);
    if let Some(installer) = found.filter(|installer| !REGISTERED.contains(installer)) {
        eprintln!(
            "hostkeeper daemon: this host could run the installer {}, and this build does not announce it yet",
            installer.as_str()
        );
    }
    with_native(announcement, found, &REGISTERED)
}
