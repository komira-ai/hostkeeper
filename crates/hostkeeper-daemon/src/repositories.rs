//! The host's repository configuration at start, and the repository installer the daemon
//! announces from it (packages-repositories.md sections 3.1 and 4.1).
//!
//! The directory of repository entries is read once, at the start, by
//! [`hostkeeper_install::repository::config::load`] as root's: a defect in it refuses the start
//! ([`StartError::Repositories`]) and a missing or empty directory is no repository. A Mac has
//! no repository entries and never reads the directory.
//!
//! [`announced`] adds `apt` to the list when the host may run it (section 4.1: a `deb` host with
//! apt's tools, apt 2.2 or later, and at least one entry), since this build registers its
//! backend ([`hostkeeper_install::REGISTERED`]) and the order path runs it
//! ([`crate::repository`]).

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

/// `announcement` with the host's native repository installer added when the host may run it
/// and the build registers it ([`hostkeeper_install::repository::announce::announced`]).
pub fn announced(
    announcement: Announcement,
    os_root: &Path,
    repositories: &Repositories,
) -> Announcement {
    let found = native(announcement.family, os_root, repositories);
    with_native(announcement, found, &REGISTERED)
}
