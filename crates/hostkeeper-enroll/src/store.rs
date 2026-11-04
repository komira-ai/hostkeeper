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
//! created so that a key is never readable by others, even for a moment.
//!
//! Opening the store takes the lock, checks `SCHEMA` and then finishes or discards what a crash
//! left: a generation without `COMPLETE` is removed, `current` is pointed at the newest complete
//! generation (or removed when there is none), and a pending key whose enrollment finished (the
//! newest generation holds the same key) is removed. Any other pending key belongs to a request
//! still open and is kept: the enroller polls for it.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

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
}

/// The store lock. The pki directory is the holder's until it is dropped.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
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

/// The port to the pki directory.
pub trait Store: Send {
    /// Takes the store lock (refused, not waited for, when another holder has it), checks the
    /// schema and finishes or discards what a crash left.
    fn open(&self) -> Result<StoreLock, StoreError>;
    /// The pending key, when there is one.
    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError>;
    /// Writes the pending key, durably, mode 0600. Nothing is posted before this returns.
    fn write_pending_key(&self, pkcs8_der: &[u8]) -> Result<(), StoreError>;
    /// The generation `current` points to.
    fn current(&self) -> Result<Option<Stored>, StoreError>;
    /// Writes generation `n + 1`, marks it complete and points `current` at it.
    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError>;
    /// Removes the generations older than `current`, once `current` has been used.
    fn prune(&self) -> Result<(), StoreError>;
}

/// The pki directory on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkiDir {
    root: PathBuf,
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
    pub fn new(root: &Path) -> PkiDir {
        PkiDir {
            root: root.to_path_buf(),
        }
    }

    /// The host's pki directory, `/etc/updater/pki`.
    pub fn host() -> PkiDir {
        PkiDir::new(Path::new("/etc/updater/pki"))
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

    fn generation_dir(&self, number: u64) -> PathBuf {
        self.root.join(generation_name(number))
    }

    fn pending_dir(&self) -> PathBuf {
        self.root.join(PENDING)
    }

    /// Reads `SCHEMA`, and writes it in a directory that has none.
    fn check_schema(&self) -> Result<(), StoreError> {
        let text = match fs::read_to_string(self.root.join(SCHEMA_FILE)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let text = format!("{SCHEMA}\n");
                return Ok(write_durable(
                    &self.root,
                    SCHEMA_FILE,
                    text.as_bytes(),
                    0o644,
                )?);
            }
            read => read?,
        };
        match text.trim().parse::<u64>() {
            Ok(SCHEMA) => Ok(()),
            Ok(found) if found > SCHEMA => Err(StoreError::NewerSchema { found }),
            _ => Err(StoreError::BadSchema(text)),
        }
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
            }
            None => remove_file_if_present(&link)?,
        }
        sync_dir(&self.root)
    }

    /// Finishes or discards what a crash left; see the module documentation.
    fn recover(&self) -> Result<(), StoreError> {
        remove_file_if_present(&self.root.join(CURRENT_TMP))?;
        remove_file_if_present(&self.pending_dir().join(tmp_name(KEY)))?;
        let mut newest = None;
        for number in self.generations()? {
            let dir = self.generation_dir(number);
            if dir.join(COMPLETE).is_file() {
                newest = Some(number);
            } else {
                fs::remove_dir_all(&dir)?;
            }
        }
        sync_dir(&self.root)?;
        self.point_current(newest)?;
        // A pending key is finished when the newest generation holds the same key; any other
        // pending key belongs to a request still open and is kept.
        let pending = self.pending_dir().join(KEY);
        if let Some(newest) = newest
            && fs::read(self.generation_dir(newest).join(KEY)).ok() == fs::read(&pending).ok()
        {
            remove_file_if_present(&pending)?;
        }
        Ok(())
    }
}

impl Store for PkiDir {
    fn open(&self) -> Result<StoreLock, StoreError> {
        make_private_dir(&self.root)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.root.join(LOCK))?;
        flock(&file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|errno| StoreError::Locked(errno.into()))?;
        self.check_schema()?;
        self.recover()?;
        Ok(StoreLock { _file: file })
    }

    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match fs::read(self.pending_dir().join(KEY)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            read => Ok(Some(read?)),
        }
    }

    fn write_pending_key(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        let dir = self.pending_dir();
        make_private_dir(&dir)?;
        sync_dir(&self.root)?;
        Ok(write_durable(&dir, KEY, pkcs8_der, 0o600)?)
    }

    fn current(&self) -> Result<Option<Stored>, StoreError> {
        let Some(number) = self.current_number()? else {
            return Ok(None);
        };
        let dir = self.generation_dir(number);
        Ok(Some(Stored {
            number,
            cert_pem: fs::read_to_string(dir.join(CERT))?,
            key_der: fs::read(dir.join(KEY))?,
            dir,
        }))
    }

    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError> {
        let number = self
            .generations()?
            .last()
            .map_or(1, |newest| newest.saturating_add(1));
        let dir = self.generation_dir(number);
        DirBuilder::new().mode(0o700).create(&dir)?;
        write_durable(&dir, CERT, cert_pem.as_bytes(), 0o644)?;
        write_durable(&dir, CA, ca_pem.as_bytes(), 0o644)?;
        let source = match key {
            KeyFrom::Pending => self.pending_dir().join(KEY),
            KeyFrom::Generation(from) => self.generation_dir(from).join(KEY),
        };
        fs::hard_link(&source, dir.join(KEY))?;
        sync_dir(&dir)?;
        write_durable(&dir, COMPLETE, b"", 0o644)?;
        sync_dir(&self.root)?;
        self.point_current(Some(number))?;
        // The generation is current from here on; should removing the pending key fail, the
        // next open removes it (see `recover`).
        if key == KeyFrom::Pending {
            fs::remove_file(&source)?;
            sync_dir(&self.pending_dir())?;
        }
        Ok(Stored {
            number,
            cert_pem: cert_pem.to_owned(),
            key_der: fs::read(dir.join(KEY))?,
            dir,
        })
    }

    fn prune(&self) -> Result<(), StoreError> {
        let Some(current) = self.current_number()? else {
            return Ok(());
        };
        for number in self.generations()? {
            if number < current {
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

/// Creates `dir` when it is missing and closes it to group and others.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, Permissions::from_mode(0o700))
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

/// Temporary file created with `mode`, fsync, rename, directory fsync.
fn write_durable(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> io::Result<()> {
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
    sync_dir(dir)
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
}
