//! The file-system port the store and the CA key file write through.
//!
//! Every file is written whole by [`write_atomic`]: a temporary file beside it, fsync, rename
//! over the name, fsync of the directory (design 4.3; the same steps as the step record of
//! `hostkeeper-steps`). [`RealFs`] does the system calls. [`FaultFs`] does them too, records
//! each operation, and fails the one a test chooses, so a test can stop the store between any
//! two operations as a crash would.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// What `fstat` says of a file read with [`Fs::read_private`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStat {
    /// A regular file (not a directory, device or pipe).
    pub is_file: bool,
    /// The owner's uid.
    pub uid: u32,
    /// The permission bits (`mode & 0o7777`).
    pub mode: u32,
}

/// An exclusive `flock` on a lock file, released when this is dropped.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

/// The file operations the store and the key file use. Every path is absolute or relative to
/// the process's directory; nothing here follows a configuration.
pub trait Fs: Send + Sync {
    /// The bytes of `path`; `None` when it does not exist.
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;
    /// The bytes of `path` and what `fstat` says of it, opened without following a symbolic
    /// link (a link is an error); `None` when it does not exist.
    fn read_private(&self, path: &Path) -> io::Result<Option<(Vec<u8>, FileStat)>>;
    /// Creates or truncates `path`, sets its permission bits to `mode`, and writes `bytes`.
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;
    /// Flushes `path`'s data and metadata to the disk.
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    /// Renames `from` over `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Flushes the directory `dir`, so a rename in it is durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Creates `dir` and its missing parents, each with mode 0700; an existing one is fine.
    fn create_dir(&self, dir: &Path) -> io::Result<()>;
    /// The names in `dir`, sorted.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// Opens (creating, mode 0600) `path` and takes an exclusive `flock` on it, waiting for it.
    fn lock(&self, path: &Path) -> io::Result<FileLock>;
}

/// The temporary file [`write_atomic`] writes `name` through.
pub fn temp_name(name: &str) -> String {
    format!("{name}.tmp")
}

/// Writes `bytes` to `dir/name` whole: `dir/<name>.tmp` with permission bits `mode`, fsync,
/// rename over `dir/name`, fsync of `dir`. A crash leaves either the old file or the new one,
/// and at most a stray temporary file, which the next write replaces.
pub fn write_atomic(
    fs: &dyn Fs,
    dir: &Path,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> io::Result<()> {
    let tmp = dir.join(temp_name(name));
    fs.write_new(&tmp, bytes, mode)?;
    fs.sync_file(&tmp)?;
    fs.rename(&tmp, &dir.join(name))?;
    fs.sync_dir(dir)
}

/// The file system itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl Fs for RealFs {
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_private(&self, path: &Path) -> io::Result<Option<(Vec<u8>, FileStat)>> {
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path);
        let mut file = match opened {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        let stat = FileStat {
            is_file: meta.is_file(),
            uid: meta.uid(),
            mode: meta.mode() & 0o7777,
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some((bytes, stat)))
    }

    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(path)?;
        // `mode` applies only to a file this call created; a stale temporary file keeps its
        // own bits unless they are set here.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        OpenOptions::new().write(true).open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }

    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = std::fs::read_dir(dir)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<String>>>()?;
        names.sort();
        Ok(names)
    }

    fn lock(&self, path: &Path) -> io::Result<FileLock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(FileLock { _file: file })
    }
}

/// One operation [`FaultFs`] was asked to do, with its paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Read(PathBuf),
    ReadPrivate(PathBuf),
    WriteNew(PathBuf),
    SyncFile(PathBuf),
    Rename(PathBuf, PathBuf),
    SyncDir(PathBuf),
    CreateDir(PathBuf),
    List(PathBuf),
    Lock(PathBuf),
}

/// [`RealFs`] that records every operation and fails the one numbered [`FaultFs::fail_at`].
///
/// Operations are numbered from 1 in the order they are asked for. The failing one does
/// nothing and returns an error of kind `Other` naming its number; the ones after it run again.
#[derive(Debug, Default)]
pub struct FaultFs {
    count: AtomicU64,
    fail_at: AtomicU64,
    ops: Mutex<Vec<Op>>,
}

impl FaultFs {
    /// A file system that fails nothing until told to.
    pub fn new() -> FaultFs {
        FaultFs::default()
    }

    /// Fails the operation numbered `n` (counted from 1 since this was made); 0 fails nothing.
    pub fn fail_at(&self, n: u64) {
        self.fail_at.store(n, Ordering::SeqCst);
    }

    /// How many operations were asked for so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    /// Every operation asked for so far, in order, the failed one included.
    pub fn ops(&self) -> Vec<Op> {
        self.ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn step(&self, op: Op) -> io::Result<()> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        self.ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(op);
        if n == self.fail_at.load(Ordering::SeqCst) {
            return Err(io::Error::other(format!(
                "injected failure at operation {n}"
            )));
        }
        Ok(())
    }
}

impl Fs for FaultFs {
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        self.step(Op::Read(path.to_path_buf()))?;
        RealFs.read(path)
    }

    fn read_private(&self, path: &Path) -> io::Result<Option<(Vec<u8>, FileStat)>> {
        self.step(Op::ReadPrivate(path.to_path_buf()))?;
        RealFs.read_private(path)
    }

    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        self.step(Op::WriteNew(path.to_path_buf()))?;
        RealFs.write_new(path, bytes, mode)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.step(Op::SyncFile(path.to_path_buf()))?;
        RealFs.sync_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.step(Op::Rename(from.to_path_buf(), to.to_path_buf()))?;
        RealFs.rename(from, to)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.step(Op::SyncDir(dir.to_path_buf()))?;
        RealFs.sync_dir(dir)
    }

    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        self.step(Op::CreateDir(dir.to_path_buf()))?;
        RealFs.create_dir(dir)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.step(Op::List(dir.to_path_buf()))?;
        RealFs.list(dir)
    }

    fn lock(&self, path: &Path) -> io::Result<FileLock> {
        self.step(Op::Lock(path.to_path_buf()))?;
        RealFs.lock(path)
    }
}
