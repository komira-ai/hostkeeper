//! The pki directory, `/etc/updater/pki` (design section 10): the host's key and certificates.
//!
//! ```text
//! /etc/updater/pki/             0700
//!   .lock                       the store lock (flock), taken for as long as an enroller lives
//!   SCHEMA                      "1": the layout below; a newer number is refused
//!   pending/key                 0600, the key of a host not yet enrolled (PKCS#8 DER)
//!   gen-<n>/                    0700, one certificate generation
//!     cert.pem                  the host certificate
//!     ca.pem                    the CA bundle it came with
//!     key                       0600, a hard link of the key it was issued for
//!     COMPLETE                  written last: the generation is whole
//!   current -> gen-<n>          the newest complete generation, switched by rename
//! ```
//!
//! Every file is written by temporary file, fsync, rename and directory fsync, the order of the
//! step record in `hostkeeper-steps` (`record.rs`), here with the file's mode set when it is
//! created so that a key is never readable by others, even for a moment. A pki directory made
//! here is fsynced into its parent.
//!
//! The root, `pending/` and each generation must be directories (not links) owned by the daemon's
//! user, and every key a regular file owned by it and closed to group and others; anything else
//! is refused ([`StoreError::NotPrivate`]). The directories are closed to group and others when
//! they are used.
//!
//! Opening the store first reads `SCHEMA`, so a newer layout is refused before anything in it is
//! touched. It then takes the lock, checks `pending/` and every generation directory as above
//! before it looks inside any of them, and finishes or discards what a crash left: a generation
//! without `COMPLETE` is removed, `current` is pointed at the newest complete generation (or
//! removed when there is none), and a pending key is removed only when it reads back equal to the
//! newest generation's key (its enrollment finished). Any other pending key, and any key that
//! cannot be read, is kept.
//!
//! A new generation is written in two parts. Its files and `COMPLETE` come first; a failure
//! there removes the partial directory and leaves `current` and every key as they were. Then
//! `current` is switched: from that rename on the generation is published. Removing the pending
//! key after publication is cleanup: a failure there is not reported, and the next open finishes
//! it.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{FlockOperation, flock};

/// The layout this release writes and reads.
pub const SCHEMA: u64 = 1;

const LOCK: &str = ".lock";
const SCHEMA_FILE: &str = "SCHEMA";
const PENDING: &str = "pending";
const KEY: &str = "key";
const CERT: &str = "cert.pem";
const CA: &str = "ca.pem";
const COMPLETE: &str = "COMPLETE";
const CURRENT: &str = "current";
const CURRENT_TMP: &str = "current.tmp";

/// Why the pki directory could not be used.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot use the pki directory: {0}")]
    Io(#[from] io::Error),
    #[error("the pki directory is locked by another enroller: {0}")]
    Locked(io::Error),
    #[error(
        "the pki directory has schema {found}, newer than schema {SCHEMA} this release reads; it is left as it is"
    )]
    NewerSchema { found: u64 },
    #[error("the pki directory's SCHEMA file does not hold a schema number: {0:?}")]
    BadSchema(String),
    #[error("the pki directory's current link points to {0:?}, which is not a generation")]
    BadLink(PathBuf),
    #[error(
        "{0:?} is not the daemon's own: a directory or key must be owned by it, not a link, and closed to group and others"
    )]
    NotPrivate(PathBuf),
    #[error("generation {0} of the pki directory has no COMPLETE marker")]
    Incomplete(u64),
    #[error("the pki directory has used every generation number")]
    GenerationsExhausted,
}

/// The store lock. The pki directory is the holder's until it is dropped.
pub struct StoreLock {
    _guard: Box<dyn Send>,
}

impl StoreLock {
    /// A lock held for as long as `guard` lives: the open lock file of [`PkiDir`], or whatever
    /// another [`Store`] holds.
    pub fn new(guard: Box<dyn Send>) -> StoreLock {
        StoreLock { _guard: guard }
    }
}

impl std::fmt::Debug for StoreLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreLock")
    }
}

/// Where the key of a new generation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyFrom {
    /// The pending key: the first certificate of a host.
    Pending,
    /// The key of generation `n`: a renewal, or a return through `renewal_of`.
    Generation(u64),
}

/// A complete generation, as read from the directory. Its `Debug` leaves the key out.
#[derive(Clone, PartialEq, Eq)]
pub struct Stored {
    pub number: u64,
    pub dir: PathBuf,
    pub cert_pem: String,
    pub ca_pem: String,
    pub key_der: Vec<u8>,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("number", &self.number)
            .field("dir", &self.dir)
            .field("key_der", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Stored {
    pub fn cert_path(&self) -> PathBuf {
        self.dir.join(CERT)
    }

    pub fn ca_path(&self) -> PathBuf {
        self.dir.join(CA)
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join(KEY)
    }
}

/// The port to the pki directory. Only the holder of the [`StoreLock`] that [`Store::open`]
/// returned calls the other methods.
pub trait Store: Send {
    /// Refuses a newer schema, takes the store lock (refused, not waited for, when another
    /// holder has it) and finishes or discards what a crash left.
    fn open(&self) -> Result<StoreLock, StoreError>;
    /// The pending key, when there is one, made durable before it is returned: a key whose
    /// earlier write failed after its rename is fsynced here, so it is never posted before it is
    /// on disk.
    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError>;
    /// Writes the pending key, durably, mode 0600. Nothing is posted before this returns.
    fn write_pending_key(&self, pkcs8_der: &[u8]) -> Result<(), StoreError>;
    /// The generation `current` points to.
    fn current(&self) -> Result<Option<Stored>, StoreError>;
    /// Writes generation `n + 1`, marks it complete and points `current` at it. An error after
    /// the switch of `current` started may leave the generation published: the caller reads
    /// [`Store::current`] to know.
    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError>;
    /// Removes the generations older than generation `used`, when `used` is still current; a
    /// confirmation for a generation that is no longer current removes nothing.
    fn prune(&self, used: u64) -> Result<(), StoreError>;
}

/// A point in the writes of [`PkiDir`] where a [`FaultGate`] can make it fail, as a failed
/// fsync or a crash would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The pending key was renamed into place; its directory is not fsynced.
    PendingKeyRenamed,
    /// A reused pending key is about to be fsynced.
    PendingKeyReused,
    /// The files and key of a new generation are written; `COMPLETE` is not.
    GenerationFiles,
    /// `current` was renamed to the new generation; the root directory is not fsynced.
    CurrentRenamed,
    /// `current` is durable; the pending key is not removed.
    Published,
}

/// Answers `true` at the points where [`PkiDir`] is to fail.
pub type FaultGate = Arc<dyn Fn(Fault) -> bool + Send + Sync>;

/// The pki directory on disk.
#[derive(Clone)]
pub struct PkiDir {
    root: PathBuf,
    owner: u32,
    gate: Option<FaultGate>,
}

impl std::fmt::Debug for PkiDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkiDir")
            .field("root", &self.root)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The paths of the generation `current` points to, through the link: what the transport
/// presents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentPaths {
    pub cert: PathBuf,
    pub ca: PathBuf,
    pub key: PathBuf,
}

impl PkiDir {
    /// The pki directory at `root`, owned by the effective user.
    pub fn new(root: &Path) -> PkiDir {
        PkiDir {
            root: root.to_path_buf(),
            owner: rustix::process::geteuid().as_raw(),
            gate: None,
        }
    }

    /// The host's pki directory, `/etc/updater/pki`.
    pub fn host() -> PkiDir {
        PkiDir::new(Path::new("/etc/updater/pki"))
    }

    /// The same directory, expected to be owned by `uid`.
    pub fn with_owner(mut self, uid: u32) -> PkiDir {
        self.owner = uid;
        self
    }

    /// The same directory, failing wherever `gate` says: the seam of the fault tests.
    pub fn with_faults(mut self, gate: FaultGate) -> PkiDir {
        self.gate = Some(gate);
        self
    }

    /// The certificate, CA bundle and key of the current generation, through `current`.
    pub fn current_paths(&self) -> CurrentPaths {
        let current = self.root.join(CURRENT);
        CurrentPaths {
            cert: current.join(CERT),
            ca: current.join(CA),
            key: current.join(KEY),
        }
    }

    fn cross(&self, fault: Fault) -> io::Result<()> {
        match &self.gate {
            Some(gate) if gate(fault) => Err(io::Error::other(format!("{fault:?} failed"))),
            _ => Ok(()),
        }
    }

    fn generation_dir(&self, number: u64) -> PathBuf {
        self.root.join(generation_name(number))
    }

    fn pending_dir(&self) -> PathBuf {
        self.root.join(PENDING)
    }

    /// Refuses `path` unless it is a directory (or, with `dir` false, a regular file) owned by
    /// the daemon's user; a key must also be closed to group and others.
    fn check_own(&self, path: &Path, dir: bool) -> Result<(), StoreError> {
        let meta = fs::symlink_metadata(path)?;
        let kind = if dir { meta.is_dir() } else { meta.is_file() };
        let closed = dir | (meta.mode() & 0o077 == 0);
        if (kind, meta.uid() == self.owner, closed) != (true, true, true) {
            return Err(StoreError::NotPrivate(path.to_path_buf()));
        }
        Ok(())
    }

    /// Checks that `dir` is the daemon's own directory and closes it to group and others.
    fn own_dir(&self, dir: &Path) -> Result<(), StoreError> {
        self.check_own(dir, true)?;
        Ok(fs::set_permissions(dir, Permissions::from_mode(0o700))?)
    }

    /// Creates `dir` (mode 0700) when it is missing, fsyncing its parent, then [`Self::own_dir`].
    fn make_dir(&self, dir: &Path) -> Result<(), StoreError> {
        if !fs::exists(dir)? {
            let parent = dir.parent().unwrap_or(Path::new("/"));
            fs::create_dir_all(parent)?;
            DirBuilder::new().mode(0o700).create(dir)?;
            sync_dir(parent)?;
        }
        self.own_dir(dir)
    }

    /// The schema of the directory: `None` when it has no `SCHEMA` yet. Reads only.
    fn read_schema(&self) -> Result<Option<u64>, StoreError> {
        let text = match fs::read_to_string(self.root.join(SCHEMA_FILE)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            read => read?,
        };
        match text.trim().parse::<u64>() {
            Ok(SCHEMA) => Ok(Some(SCHEMA)),
            Ok(found) if found > SCHEMA => Err(StoreError::NewerSchema { found }),
            _ => Err(StoreError::BadSchema(text)),
        }
    }

    /// Reads the key file `path`, which must be the daemon's own.
    fn read_key(&self, path: &Path) -> Result<Vec<u8>, StoreError> {
        self.check_own(path, false)?;
        Ok(fs::read(path)?)
    }

    /// The numbers of the `gen-<n>` directories, ascending; other names are not generations.
    fn generations(&self) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let name = entry?.file_name();
            out.extend(name.to_str().and_then(generation_number));
        }
        out.sort_unstable();
        Ok(out)
    }

    /// The generation `current` points to, without reading it.
    fn current_number(&self) -> Result<Option<u64>, StoreError> {
        let target = match fs::read_link(self.root.join(CURRENT)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            read => read?,
        };
        let number = target.to_str().and_then(generation_number);
        number.map(Some).ok_or(StoreError::BadLink(target))
    }

    /// Points `current` at generation `newest`, or removes it for `None`.
    fn point_current(&self, newest: Option<u64>) -> io::Result<()> {
        let link = self.root.join(CURRENT);
        let wanted = newest.map(|n| PathBuf::from(generation_name(n)));
        if fs::read_link(&link).ok() == wanted {
            return Ok(());
        }
        match wanted {
            Some(target) => {
                let tmp = self.root.join(CURRENT_TMP);
                remove_file_if_present(&tmp)?;
                symlink(&target, &tmp)?;
                fs::rename(&tmp, &link)?;
                self.cross(Fault::CurrentRenamed)?;
            }
            None => remove_file_if_present(&link)?,
        }
        sync_dir(&self.root)
    }

    /// Finishes or discards what a crash left; see the module documentation.
    fn recover(&self) -> Result<(), StoreError> {
        // Nothing inside `pending/` or a generation is touched before it is checked to be the
        // daemon's own directory, so a link can never lead a removal out of the tree.
        if fs::symlink_metadata(self.pending_dir()).is_ok() {
            self.own_dir(&self.pending_dir())?;
        }
        remove_file_if_present(&self.root.join(CURRENT_TMP))?;
        remove_file_if_present(&self.pending_dir().join(tmp_name(KEY)))?;
        let mut newest = None;
        for number in self.generations()? {
            let dir = self.generation_dir(number);
            self.check_own(&dir, true)?;
            if dir.join(COMPLETE).is_file() {
                newest = Some(number);
            } else {
                fs::remove_dir_all(&dir)?;
            }
        }
        sync_dir(&self.root)?;
        self.point_current(newest)?;
        // A pending key is finished when it reads back equal to the newest generation's key;
        // any other pending key, or one that cannot be read, belongs to a request still open.
        let pending = self.pending_dir().join(KEY);
        if let Some(newest) = newest
            && let (Ok(held), Ok(waiting)) = (
                self.read_key(&self.generation_dir(newest).join(KEY)),
                self.read_key(&pending),
            )
            && held == waiting
        {
            fs::remove_file(&pending)?;
            sync_dir(&self.pending_dir())?;
        }
        Ok(())
    }

    /// Temporary file created with `mode`, fsync, rename, `after_rename`, directory fsync.
    fn write_durable(
        &self,
        dir: &Path,
        name: &str,
        bytes: &[u8],
        mode: u32,
        after_rename: Option<Fault>,
    ) -> io::Result<()> {
        let tmp = dir.join(tmp_name(name));
        remove_file_if_present(&tmp)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(name))?;
        if let Some(fault) = after_rename {
            self.cross(fault)?;
        }
        sync_dir(dir)
    }

    /// The files of generation directory `dir`, then `COMPLETE`.
    fn fill_generation(
        &self,
        dir: &Path,
        cert_pem: &str,
        ca_pem: &str,
        source: &Path,
    ) -> Result<(), StoreError> {
        self.write_durable(dir, CERT, cert_pem.as_bytes(), 0o644, None)?;
        self.write_durable(dir, CA, ca_pem.as_bytes(), 0o644, None)?;
        self.check_own(source, false)?;
        fs::hard_link(source, dir.join(KEY))?;
        sync_dir(dir)?;
        self.cross(Fault::GenerationFiles)?;
        self.write_durable(dir, COMPLETE, b"", 0o644, None)?;
        Ok(sync_dir(&self.root)?)
    }
}

impl Store for PkiDir {
    fn open(&self) -> Result<StoreLock, StoreError> {
        let schema = self.read_schema()?;
        self.make_dir(&self.root)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.root.join(LOCK))?;
        flock(&file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|errno| StoreError::Locked(errno.into()))?;
        if schema.is_none() {
            let text = format!("{SCHEMA}\n");
            self.write_durable(&self.root, SCHEMA_FILE, text.as_bytes(), 0o644, None)?;
        }
        self.recover()?;
        Ok(StoreLock::new(Box::new(file)))
    }

    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.pending_dir().join(KEY);
        if !fs::exists(&path)? {
            return Ok(None);
        }
        self.own_dir(&self.pending_dir())?;
        let key = self.read_key(&path)?;
        self.cross(Fault::PendingKeyReused)?;
        File::open(&path)?.sync_all()?;
        sync_dir(&self.pending_dir())?;
        sync_dir(&self.root)?;
        Ok(Some(key))
    }

    fn write_pending_key(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        let dir = self.pending_dir();
        self.make_dir(&dir)?;
        Ok(self.write_durable(&dir, KEY, pkcs8_der, 0o600, Some(Fault::PendingKeyRenamed))?)
    }

    fn current(&self) -> Result<Option<Stored>, StoreError> {
        let Some(number) = self.current_number()? else {
            return Ok(None);
        };
        let dir = self.generation_dir(number);
        self.check_own(&dir, true)?;
        if !dir.join(COMPLETE).is_file() {
            return Err(StoreError::Incomplete(number));
        }
        Ok(Some(Stored {
            number,
            cert_pem: fs::read_to_string(dir.join(CERT))?,
            ca_pem: fs::read_to_string(dir.join(CA))?,
            key_der: self.read_key(&dir.join(KEY))?,
            dir,
        }))
    }

    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError> {
        let number = match self.generations()?.last() {
            None => 1,
            Some(newest) => newest
                .checked_add(1)
                .ok_or(StoreError::GenerationsExhausted)?,
        };
        let dir = self.generation_dir(number);
        let source = match key {
            KeyFrom::Pending => self.pending_dir().join(KEY),
            KeyFrom::Generation(from) => self.generation_dir(from).join(KEY),
        };
        DirBuilder::new().mode(0o700).create(&dir)?;
        if let Err(error) = self.fill_generation(&dir, cert_pem, ca_pem, &source) {
            // Not published: the partial generation goes, and a later attempt starts afresh.
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        self.point_current(Some(number))?;
        if key == KeyFrom::Pending {
            // Published; removing the pending key is cleanup, which the next open finishes.
            let _ = self
                .cross(Fault::Published)
                .and_then(|()| fs::remove_file(&source))
                .and_then(|()| sync_dir(&self.pending_dir()));
        }
        Ok(Stored {
            number,
            cert_pem: cert_pem.to_owned(),
            ca_pem: ca_pem.to_owned(),
            key_der: fs::read(dir.join(KEY))?,
            dir,
        })
    }

    fn prune(&self, used: u64) -> Result<(), StoreError> {
        if self.current_number()? != Some(used) {
            return Ok(());
        }
        for number in self.generations()? {
            if number < used {
                fs::remove_dir_all(self.generation_dir(number))?;
            }
        }
        Ok(sync_dir(&self.root)?)
    }
}

fn generation_name(number: u64) -> String {
    format!("gen-{number}")
}

/// The `n` of a directory named exactly `gen-<n>` (no sign, no leading zero).
fn generation_number(name: &str) -> Option<u64> {
    let number: u64 = name.strip_prefix("gen-")?.parse().ok()?;
    (generation_name(number) == name).then_some(number)
}

fn tmp_name(name: &str) -> String {
    format!("{name}.tmp")
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a directory that only looks like a generation being taken for one, so that
    /// `current` could point at `gen-01` while the next generation is written as `gen-1`.
    #[test]
    fn only_canonical_generation_names_count() {
        for (name, number) in [("gen-1", Some(1)), ("gen-42", Some(42)), ("gen-0", Some(0))] {
            assert_eq!(generation_number(name), number, "{name}");
        }
        for name in [
            "gen-", "gen-01", "gen-+1", "gen-x", "gen-1a", "pending", "gen1",
        ] {
            assert_eq!(generation_number(name), None, "{name}");
        }
    }

    /// Catches a lock that another store cannot make, or whose `Debug` shows what it holds.
    #[test]
    fn any_store_can_make_a_lock() {
        assert_eq!(format!("{:?}", StoreLock::new(Box::new(()))), "StoreLock");
    }
}
