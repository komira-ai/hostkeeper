//! The repository sets the coordinator pushes (`repositories-v1`, packages-repositories.md
//! sections 3.1 and 4.1): the daemon writes each as the host's whole repository configuration
//! and takes it into use without a restart.
//!
//! [`SharedRepositories`] is the entries the repository installer reads: each step of the
//! installer takes a snapshot of it, and a set that was applied replaces it whole.
//! [`RepositoryPush`] is the [`RepositorySink`] the heartbeat loop gives a pushed set to. A Mac
//! has no repository directory ([`crate::cli::HostFiles::repositories`] is none), so it has no
//! [`RepositoryPush`], announces no `repositories-v1` and writes nothing.
//!
//! **Applying a set.** The set is checked by the rules the coordinator checked it by
//! ([`registry::check_set`], and at least one repository), and its digest is computed again
//! ([`registry::digest`]) and must be the set's. The files it is written as are rendered for the
//! live directory ([`registry::render`]). Then, holding the installer lock without waiting for
//! it (a set that finds it held is tried again at the next heartbeat, which the coordinator
//! sends it for as long as the report does not hold it):
//!
//! 1. A live directory that holds exactly those file names with exactly those bytes, and that
//!    the loader takes, is adopted: nothing is written. It is how the files the host's setup
//!    placed are taken over.
//! 2. Otherwise the files are written (mode 0644, in a directory of mode 0755, owned by the
//!    daemon's owner) into a fresh [`STAGING`] directory beside the live one and synced, the
//!    loader judges that directory as it will be seen at the live path, and each entry is checked
//!    again against the keyring bytes the loader read, so a keyring that is not the one that was
//!    checked is refused. Then the staging directory takes the live one's place: renamed when
//!    there is no live directory, else exchanged with it (`renameat2` with `RENAME_EXCHANGE`),
//!    so no reader sees a directory half written, and the old one, now at the staging path, is
//!    removed. Everything of the old directory is gone: the set is the whole configuration.
//!
//! A set refused for its content (a rule, the digest, the loader, a fingerprint) leaves the live
//! directory and the entries in use as they were, and the report says it as `refused`, with the
//! set's generation and digest and the reason. A failure of the filesystem (or of the lock) is
//! logged and not reported as a refusal: the coordinator sends the set again. An applied or
//! adopted set is the one in force, and clears `refused`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hostkeeper_install::Announcement;
use hostkeeper_install::repository::config::{Loaded, load_with_keyrings};
use hostkeeper_install::repository::{InstallerLock, Repositories};
use hostkeeper_link::{
    Log, MAX_REFUSAL_REASON, PushedSet, RepositoriesReport, RepositoryRefusal, RepositorySink,
};
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::Installer;
use hostkeeper_release::registry::{self, HostFile};

use crate::repository::RepositoryPorts;

/// The directory a set is staged in, beside the live directory.
pub const STAGING: &str = ".repositories.next";

/// The mode of a staged file: what apt, which runs as another user, needs to read a keyring.
const FILE_MODE: u32 = 0o644;
/// The mode of the staged directory.
const DIR_MODE: u32 = 0o755;

/// The entries the repository installer reads, replaced whole by an applied set. Clones share
/// one value.
#[derive(Clone, Debug, Default)]
pub struct SharedRepositories(Arc<RwLock<Arc<Repositories>>>);

impl SharedRepositories {
    /// The handle over `repositories`.
    pub fn new(repositories: Repositories) -> SharedRepositories {
        SharedRepositories(Arc::new(RwLock::new(Arc::new(repositories))))
    }

    /// The entries now: the value a step works with from start to end, whatever is applied
    /// meanwhile.
    pub fn snapshot(&self) -> Arc<Repositories> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Makes `repositories` the entries from now on.
    pub fn replace(&self, repositories: Repositories) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(repositories);
    }
}

/// What makes a host's repository directory the coordinator's to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Managed {
    /// The live directory (`/etc/hostkeeper/repositories`).
    pub dir: PathBuf,
    /// The owner every file and the directory must have: the daemon's (root on a host).
    pub owner_uid: u32,
    /// The native installer the host could run were it given an entry, as decided at the start
    /// ([`hostkeeper_install::repository::announce::capable`]); announced once the entries in
    /// use are not none.
    pub native: Option<Installer>,
}

/// The installers the daemon announces now: those decided at its start (`base`), and the host's
/// native one once it has an entry in use, which a set applied since the start may have given it.
pub fn installers(base: &Announcement, ports: &RepositoryPorts) -> Vec<Installer> {
    let mut installers = base.installers.clone();
    let native = ports
        .managed
        .as_ref()
        .and_then(|managed| managed.native)
        .filter(|installer| !installers.contains(installer));
    if let Some(installer) = native
        && !ports.repositories.snapshot().is_empty()
    {
        installers.push(installer);
    }
    installers
}

/// How applying a set ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The live directory already held the set's files: nothing was written.
    Adopted,
    /// The set's files replaced the live directory.
    Written,
    /// The set is refused for its content, for this reason.
    Refused(String),
    /// The installer lock was held: nothing was done.
    Busy,
    /// The filesystem failed: nothing was recorded, and the live directory is as it was or
    /// holds the whole new set.
    Failed(String),
}

/// What stopped a set short.
enum Fault {
    Refused(String),
    Io(io::Error),
}

impl From<io::Error> for Fault {
    fn from(error: io::Error) -> Fault {
        Fault::Io(error)
    }
}

/// What the report says.
#[derive(Default)]
struct State {
    generation: u64,
    digest: String,
    refused: Option<RepositoryRefusal>,
}

/// The daemon's [`RepositorySink`]: writes the pushed sets into the live directory, and swaps the
/// entries the repository installer reads.
pub struct RepositoryPush {
    ports: Arc<RepositoryPorts>,
    managed: Managed,
    /// The installers decided at the start.
    base: Announcement,
    log: Arc<dyn Log>,
    state: Mutex<State>,
}

impl RepositoryPush {
    /// The sink over `ports`, which holds the live directory; none when the host has none
    /// ([`RepositoryPorts::managed`] is none: a Mac). `base` is what the daemon announced at its
    /// start.
    pub fn new(
        ports: Arc<RepositoryPorts>,
        base: Announcement,
        log: Arc<dyn Log>,
    ) -> Option<RepositoryPush> {
        let managed = ports.managed.clone()?;
        Some(RepositoryPush {
            ports,
            managed,
            base,
            log,
            state: Mutex::new(State::default()),
        })
    }

    /// Applies `set` and says how it ended.
    pub fn apply_set(&self, set: &PushedSet) -> Applied {
        self.apply_with(set, &|_| {})
    }

    /// [`Self::apply_set`], calling `staged` with the staging directory once its files are
    /// written and before the loader judges it.
    fn apply_with(&self, set: &PushedSet, staged: &dyn Fn(&Path)) -> Applied {
        let live = self.managed.dir.to_string_lossy();
        let files = match judge(set, &live) {
            Ok(files) => files,
            Err(reason) => return Applied::Refused(reason),
        };
        let lock = InstallerLock::new(&self.ports.lock);
        let _held = match lock.try_hold() {
            Ok(Some(held)) => held,
            Ok(None) => return Applied::Busy,
            Err(error) => return Applied::Failed(format!("the installer lock: {error}")),
        };
        match self.put(set, &files, staged) {
            Ok((repositories, written)) => {
                self.ports.repositories.replace(repositories);
                match written {
                    true => Applied::Written,
                    false => Applied::Adopted,
                }
            }
            Err(Fault::Refused(reason)) => Applied::Refused(reason),
            Err(Fault::Io(error)) => Applied::Failed(error.to_string()),
        }
    }

    /// The entries of the live directory once it holds `files`, and whether it was written.
    fn put(
        &self,
        set: &PushedSet,
        files: &[HostFile],
        staged_hook: &dyn Fn(&Path),
    ) -> Result<(Repositories, bool), Fault> {
        let live = self.managed.dir.as_path();
        let owner = self.managed.owner_uid;
        if let Some(loaded) = adoptable(live, files, owner) {
            verify(&set.entries, &loaded, live)?;
            return Ok((loaded.repositories, false));
        }
        let staged = live.with_file_name(STAGING);
        staged.parent().map_or(Ok(()), fs::create_dir_all)?;
        // What cannot be removed is the error of the make that follows.
        let _ = remove_path(&staged);
        fs::create_dir(&staged)?;
        fs::set_permissions(&staged, Permissions::from_mode(DIR_MODE))?;
        for file in files {
            write_file(&staged.join(&file.name), &file.bytes)?;
        }
        sync_dir(&staged)?;
        staged_hook(&staged);
        let loaded = load_with_keyrings(&staged, live, owner)
            .map_err(|error| Fault::Refused(error.to_string()))?;
        verify(&set.entries, &loaded, live)?;
        swap(&staged, live)?;
        Ok((loaded.repositories, true))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl RepositorySink for RepositoryPush {
    fn report(&self) -> RepositoriesReport {
        let state = self.state();
        RepositoriesReport {
            generation: state.generation,
            digest: state.digest.clone(),
            refused: state.refused.clone(),
        }
    }

    fn apply(&self, set: &PushedSet) {
        let applied = self.apply_set(set);
        let named = format!("repositories generation {}", set.generation);
        let mut state = self.state();
        match applied {
            Applied::Adopted | Applied::Written => {
                self.log.line(&format!(
                    "{named}: {}",
                    match applied {
                        Applied::Adopted => "the files already in place are in force",
                        _ => "written and in force",
                    }
                ));
                *state = State {
                    generation: set.generation,
                    digest: set.digest.clone(),
                    refused: None,
                };
            }
            Applied::Refused(reason) => {
                self.log
                    .line(&format!("error: {named} is refused: {reason}"));
                state.refused = Some(RepositoryRefusal {
                    generation: set.generation,
                    digest: set.digest.clone(),
                    reason: clip(&reason),
                });
            }
            Applied::Busy => self.log.line(&format!(
                "{named}: the installer lock is held; it is tried again at the next heartbeat"
            )),
            Applied::Failed(error) => self
                .log
                .line(&format!("error: {named} is not written: {error}")),
        }
    }

    fn installers(&self) -> Vec<String> {
        installers(&self.base, &self.ports)
            .iter()
            .map(|installer| installer.as_str().to_owned())
            .collect()
    }
}

/// `reason` cut at a character boundary to [`MAX_REFUSAL_REASON`] bytes.
fn clip(reason: &str) -> String {
    let mut end = reason.len().min(MAX_REFUSAL_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

/// The files of `set` as written for the directory `live`, when the set passes the coordinator's
/// rules and its digest is the one of its entries; else why not.
fn judge(set: &PushedSet, live: &str) -> Result<Vec<HostFile>, String> {
    if set.entries.is_empty() {
        return Err("the set holds no repository".to_owned());
    }
    registry::check_set(&set.entries).map_err(|error| error.to_string())?;
    let found = registry::digest(&set.entries);
    if found != set.digest {
        return Err(format!(
            "the set's digest {:?} is not {found}, the digest of its entries",
            set.digest
        ));
    }
    Ok(registry::render(&set.entries, live))
}

/// What the loader takes of `live` when it holds exactly `files`, byte for byte; none when it
/// holds anything else, or the loader refuses it.
fn adoptable(live: &Path, files: &[HostFile], owner: u32) -> Option<Loaded> {
    let held: BTreeSet<OsString> = fs::read_dir(live)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.file_name()))
        .collect();
    let wanted: BTreeSet<OsString> = files.iter().map(|file| file.name.clone().into()).collect();
    let same = held == wanted
        && files
            .iter()
            .all(|file| fs::read(live.join(&file.name)).is_ok_and(|bytes| bytes == file.bytes));
    if same {
        load_with_keyrings(live, live, owner).ok()
    } else {
        None
    }
}

/// Checks each of `entries` again against the keyring bytes the loader read of its file, so a
/// keyring that is not the one the rules judged is refused.
fn verify(entries: &[Repository], loaded: &Loaded, live: &Path) -> Result<(), Fault> {
    for entry in entries {
        let path = live.join(registry::keyring_file(&entry.name));
        let keyring = loaded.keyrings.get(&path).cloned().unwrap_or_default();
        let read = Repository {
            keyring,
            ..entry.clone()
        };
        registry::check(&read).map_err(|error| {
            Fault::Refused(format!(
                "the keyring of {:?} as written is refused: {error}",
                entry.name
            ))
        })?;
    }
    Ok(())
}

/// Removes what is at `path`, a directory with what it holds or any other file. Nothing there is
/// an error, which a caller that does not care ignores.
fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(absent) => Err(absent),
    }
}

/// Writes `bytes` to a new file at `path`, mode [`FILE_MODE`] whatever the umask, and syncs it.
fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Syncs the directory `path`.
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Puts the staged directory at `live`: a rename when nothing is there, else an exchange, after
/// which the old directory is at the staging path and is removed. The swap is done by then, so
/// the removal and the sync of the parent that makes it durable are not errors of it: a
/// leftover is removed by the next set.
fn swap(staged: &Path, live: &Path) -> io::Result<()> {
    // A live path that cannot be looked at is taken for absent: the rename says why when it is
    // not.
    if fs::symlink_metadata(live).is_err() {
        fs::rename(staged, live)?;
    } else {
        exchange(staged, live)?;
        let _ = remove_path(staged);
    }
    let _ = live.parent().map_or(Ok(()), sync_dir);
    Ok(())
}

/// Exchanges `a` and `b` in one step.
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, a, CWD, b, RenameFlags::EXCHANGE).map_err(io::Error::from)
}

/// Exchanges `a` and `b` in one step: only a Linux host has the call, and only a Linux daemon
/// has a repository directory to write.
#[cfg(not(target_os = "linux"))]
fn exchange(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
#[path = "repository_push_tests.rs"]
mod tests;
