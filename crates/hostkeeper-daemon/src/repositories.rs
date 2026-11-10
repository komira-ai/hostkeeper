//! The host's repository configuration at start, the repository installer the daemon announces
//! from it (packages-repositories.md sections 3.1 and 4.1), and `hostkeeper check-repositories`,
//! host setup's check of a directory with the same loader before it places the directory.
//!
//! The directory of repository entries is read at the start by
//! [`hostkeeper_install::repository::config::load_with_keyrings`] as [`ROOT_UID`]'s: a defect in
//! it refuses the start ([`StartError::Repositories`]) and a missing or empty directory is no
//! repository. After the start, the repository sets the coordinator pushes replace the directory
//! and the entries in use ([`crate::repository_push`]). A Mac has no repository entries and never
//! reads the directory.
//!
//! [`check`] is that same call over another directory, judged as the host will see it at
//! [`REPOSITORIES_DIR`]: a `Signed-By` of `/etc/hostkeeper/repositories/<file>` names the file
//! `<file>` of the directory checked, so host setup can check the files it staged in a sibling
//! directory before it renames them into place. The verb accepts exactly what the start accepts.
//!
//! [`capable`] is the installer a host would announce were it given an entry, which a set of
//! repositories the coordinator pushes ([`crate::repository_push`]) may give it after the start.
//!
//! [`announced`] adds `apt` to the list when the host may run it (section 4.1: a `deb` host with
//! apt's tools, apt 2.2 or later, and at least one entry), since this build registers its
//! backend ([`hostkeeper_install::REGISTERED`]) and the order path runs it
//! ([`crate::repository`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use hostkeeper_install::Announcement;
use hostkeeper_install::REGISTERED;
use hostkeeper_install::repository::Repositories;
use hostkeeper_install::repository::announce::{
    announced as with_native, capable as capable_native, native,
};
use hostkeeper_install::repository::config::{self, Loaded, RepositoriesError};
use hostkeeper_release::Installer;
use sha2::{Digest, Sha256};

pub use hostkeeper_install::repository::config::REPOSITORIES_DIR;

use crate::cli::HostFiles;
use crate::start::StartError;

/// The owner every file of the directory, and the directory, must have on a host: root.
pub const ROOT_UID: u32 = 0;

/// The exit status of `hostkeeper check-repositories` when the loader refuses the directory, or
/// its report cannot be written.
pub const EXIT_REFUSED: u8 = 1;

/// The flags of `hostkeeper check-repositories`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct CheckFlags {
    /// The directory to check, as the daemon would take it at /etc/hostkeeper/repositories: a
    /// Signed-By of /etc/hostkeeper/repositories/<file> names <file> of this directory.
    #[arg(long, default_value = REPOSITORIES_DIR)]
    pub dir: PathBuf,
}

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

/// The entries of `dir` and the bytes of the keyrings they name, as the start takes the same
/// files at [`REPOSITORIES_DIR`] ([`load`]): the loader's rules over the files of `dir`, every
/// file owned by `owner_uid`, each `Signed-By` judged against [`REPOSITORIES_DIR`] and its
/// keyring read at `dir/<file>`. A missing or empty `dir` is no entry, as at the start.
pub fn check(dir: &Path, owner_uid: u32) -> Result<Loaded, RepositoriesError> {
    config::load_with_keyrings(dir, Path::new(REPOSITORIES_DIR), owner_uid)
}

/// What `check-repositories` prints of `loaded`, the entries of `dir`: one line per entry, in
/// name order, `<name> suites=<suite> keyring=<Signed-By> sha256=<hex>` with the SHA-256 of the
/// keyring's bytes as the loader read them; one line saying so when there is no entry.
pub fn report(dir: &Path, loaded: &Loaded) -> String {
    let repositories = &loaded.repositories;
    if repositories.is_empty() {
        return format!(
            "{}: no repository entry; the daemon would announce no repository installer\n",
            dir.display()
        );
    }
    let sha256 = |bytes: &Vec<u8>| hex::encode(Sha256::digest(bytes));
    repositories
        .names()
        .into_iter()
        .filter_map(|name| repositories.get(name))
        .map(|entry| {
            let digest = loaded.keyrings.get(&entry.keyring).map(sha256);
            format!(
                "{} suites={} keyring={} sha256={}\n",
                entry.name,
                entry.suite,
                entry.keyring.display(),
                digest.unwrap_or_default()
            )
        })
        .collect()
}

/// Runs `hostkeeper check-repositories` over `flags`' directory, every file owned by
/// `owner_uid` (the command line passes [`ROOT_UID`], as the start does): [`report`] on `out`
/// and status 0 when the loader takes the directory; the loader's refusal on `err`, after
/// `hostkeeper check-repositories: `, and [`EXIT_REFUSED`] when it does not, or when `out`
/// cannot be written.
pub fn run_check(
    flags: &CheckFlags,
    owner_uid: u32,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let failed = match check(&flags.dir, owner_uid) {
        Ok(loaded) => match out.write_all(report(&flags.dir, &loaded).as_bytes()) {
            Ok(()) => return 0,
            Err(error) => format!("the report cannot be written: {error}"),
        },
        Err(refusal) => refusal.to_string(),
    };
    // Nothing is left to tell when standard error cannot be written either: the status says it.
    let _ = writeln!(err, "hostkeeper check-repositories: {failed}");
    EXIT_REFUSED
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
