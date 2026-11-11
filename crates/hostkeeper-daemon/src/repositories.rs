//! The host's repository configuration at start, the repository installer the daemon announces
//! from it (packages-repositories.md sections 3.1 and 4.1).
//!
//! The directory of repository entries is read at the start by
//! [`hostkeeper_install::repository::config::load_with_keyrings`] as [`ROOT_UID`]'s: a defect in
//! it refuses the start ([`StartError::Repositories`]) and a missing or empty directory is no
//! repository. After the start, the repository sets the coordinator pushes replace the directory
//! and the entries in use ([`crate::repository_push`]). A Mac has no repository entries and never
//! reads the directory.
//!
//! [`capable`] is the installer a host would announce were it given an entry, which a set of
//! repositories the coordinator pushes ([`crate::repository_push`]) may give it after the start.
//!
//! [`announced`] adds `apt` to the list when the host may run it (section 4.1: a `deb` host with
//! apt's tools, apt 2.2 or later, and at least one entry), since this build registers its
//! backend ([`hostkeeper_install::REGISTERED`]) and the order path runs it
//! ([`crate::repository`]).

use std::path::Path;

use hostkeeper_install::Announcement;
use hostkeeper_install::REGISTERED;
use hostkeeper_install::repository::Repositories;
use hostkeeper_install::repository::announce::{
    announced as with_native, capable as capable_native, native,
};
use hostkeeper_install::repository::config;
use hostkeeper_release::Installer;

pub use hostkeeper_install::repository::config::REPOSITORIES_DIR;

use crate::cli::HostFiles;
use crate::start::StartError;

/// The owner every file of the directory, and the directory, must have on a host: root.
pub const ROOT_UID: u32 = 0;

/// The entries of the repositories directory of `files`, every file owned by `owner_uid` (the
/// start passes [`ROOT_UID`]); none when `files` has no such directory (a Mac) or the directory
/// is not there. `Err` names the file that breaks a rule.
pub fn load(files: &HostFiles, owner_uid: u32) -> Result<Repositories, StartError> {
    match &files.repositories {
        None => Ok(Repositories::default()),
        Some(path) => config::load_with_keyrings(path, path, owner_uid)
            .map(|loaded| loaded.repositories)
            .map_err(|source| StartError::Repositories {
                path: path.clone(),
                source,
            }),
    }
}

/// The native repository installer the host could run once it has an entry, when the build
/// registers it: what [`crate::repository_push`] announces after the first set the coordinator
/// pushes ([`hostkeeper_install::repository::announce::capable`]).
pub fn capable(announcement: &Announcement, os_root: &Path) -> Option<Installer> {
    capable_native(announcement.family, os_root).filter(|installer| REGISTERED.contains(installer))
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
