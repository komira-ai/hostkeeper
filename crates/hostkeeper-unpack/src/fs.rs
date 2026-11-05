//! The file-system port of the unpack ([`PackageFs`]) and its production implementation
//! ([`HostFs`]).
//!
//! Every path the port takes is relative to the packages directory
//! (`/var/lib/hostkeeper/packages`) and made of plain names: `<service>/.stage-<random>/bin/x`.
//! [`HostFs`] opens the packages directory once and reaches every path from that handle, one
//! component at a time, never following a link (`openat` with `O_NOFOLLOW`), and creates
//! every file and directory afresh (`O_EXCL`, `mkdirat`). So no entry of an archive can make it
//! write outside the packages directory, or through a link inside it, even if a name check
//! before it had a bug. Tests put a fake in front of it to record calls and to inject a full
//! disk or a crash at any call.

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Permissions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Gid, Mode, OFlags, Uid};
use rustix::io::Errno;

/// The calls an unpack makes on the disk. Paths are relative to the packages directory.
pub trait PackageFs {
    /// The names in the directory `dir` (not `.` and `..`).
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    /// Whether something (of any kind, a link included) is at `path`, looked at without
    /// following it. Only a missing entry, or a missing parent, is `false`.
    fn exists(&self, path: &Path) -> io::Result<bool>;
    /// Makes the service directory `<service>` ready: creates it when it is missing, then, on
    /// every call, opens it without following a link (a link or a file there is an error),
    /// gives it to the owner with mode 0755 (service users pass through it to their version)
    /// and makes its entry in the packages directory durable. So a call stopped half-way, by a
    /// crash or a failed fsync, is finished by the next one.
    fn create_service(&self, service: &Path) -> io::Result<()>;
    /// Takes the exclusive unpack lock of the service directory `<service>`, without waiting:
    /// [`io::ErrorKind::WouldBlock`] while another holder has it. Held until the lock is dropped.
    fn lock(&self, service: &Path) -> io::Result<ServiceLock>;
    /// Creates a new, empty directory `<service>/.stage-<random>` with mode 0700 in the
    /// existing service directory, and returns its path.
    fn create_stage(&self, service: &Path) -> io::Result<PathBuf>;
    /// Creates the new directory `path`, mode 0700.
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Creates the new, empty file `path`, mode 0600, open for writing.
    fn create_file(&self, path: &Path) -> io::Result<Box<dyn StagedFile>>;
    /// Sets the permission bits of `path` (a directory, in an unpack).
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// Makes the entries of the directory `path` durable.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    /// Renames `from` to `to`, replacing a file at `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Removes `path` and everything below it, never following a link. Nothing there is not an
    /// error.
    fn remove_tree(&self, path: &Path) -> io::Result<()>;
    /// Where `path` is, for the readers that take a path: the manifest loader and the digest of
    /// the staged tree.
    fn real_path(&self, path: &Path) -> PathBuf;
}

/// A file being written by an unpack.
pub trait StagedFile {
    /// Writes all of `bytes`.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Sets the file's permission bits.
    fn set_mode(&mut self, mode: u32) -> io::Result<()>;
    /// Makes the file's contents durable.
    fn sync(&mut self) -> io::Result<()>;
}

/// The exclusive unpack lock of one service directory (`flock` on the directory), released when
/// it is dropped. Only [`HostFs::lock`] makes one.
#[derive(Debug)]
pub struct ServiceLock {
    _held: OwnedFd,
}

/// The owner of everything [`HostFs`] creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl Owner {
    /// Root: the owner of every package on a host.
    pub const ROOT: Owner = Owner { uid: 0, gid: 0 };
}

/// The packages directory of the host.
#[derive(Debug)]
pub struct HostFs {
    root: PathBuf,
    fd: OwnedFd,
    owner: Owner,
}

impl HostFs {
    /// Opens the packages directory `root`, which must be a directory (not a link). Everything
    /// created through it is given to `owner`: [`Owner::ROOT`] in the daemon.
    pub fn open(root: &Path, owner: Owner) -> io::Result<HostFs> {
        let fd = rustix::fs::open(root, dir_flags(), Mode::empty())?;
        Ok(HostFs {
            root: root.to_path_buf(),
            fd,
            owner,
        })
    }

    /// The directory that holds the last component of `path`, reached without following a
    /// link, and that component.
    fn parent<'p>(&self, path: &'p Path) -> io::Result<(OwnedFd, &'p OsStr)> {
        let mut names = Vec::new();
        for part in path.components() {
            match part {
                Component::Normal(name) => names.push(name),
                _ => return Err(not_plain()),
            }
        }
        let Some((last, dirs)) = names.split_last() else {
            return Err(not_plain());
        };
        let mut at = rustix::fs::openat(&self.fd, ".", dir_flags(), Mode::empty())?;
        for dir in dirs {
            at = rustix::fs::openat(&at, *dir, dir_flags(), Mode::empty())?;
        }
        Ok((at, last))
    }

    /// Opens `path` without following it: a directory with `O_DIRECTORY` when `dir`.
    fn open_at(&self, path: &Path, dir: bool) -> io::Result<OwnedFd> {
        let (at, name) = self.parent(path)?;
        let flags = if dir {
            dir_flags()
        } else {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        Ok(rustix::fs::openat(&at, name, flags, Mode::empty())?)
    }

    fn give(&self, fd: impl AsFd) -> io::Result<()> {
        let uid = Uid::from_raw(self.owner.uid);
        let gid = Gid::from_raw(self.owner.gid);
        Ok(rustix::fs::fchown(fd, Some(uid), Some(gid))?)
    }

    /// `mkdirat`, then the new directory given to the owner with `mode` (not narrowed by the
    /// umask).
    fn make_dir(&self, at: &OwnedFd, name: &OsStr, mode: u32) -> io::Result<()> {
        rustix::fs::mkdirat(at, name, Mode::from_raw_mode(0o700))?;
        let fd = rustix::fs::openat(at, name, dir_flags(), Mode::empty())?;
        self.give(&fd)?;
        set_mode_of(fd, mode)
    }
}

/// Sets the mode of the open `fd` to `mode`, as `fchmod` does. Through std, which converts the
/// mode to the platform's `mode_t` (32 bits on Linux, 16 on macOS), so no cast is written here.
fn set_mode_of(fd: OwnedFd, mode: u32) -> io::Result<()> {
    File::from(fd).set_permissions(Permissions::from_mode(mode))
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn not_plain() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "a package path is relative and made of plain names",
    )
}

impl PackageFs for HostFs {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        let fd = self.open_at(dir, true)?;
        names(&fd)
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        let found = self
            .parent(path)
            .and_then(|(at, name)| Ok(rustix::fs::statat(&at, name, AtFlags::SYMLINK_NOFOLLOW)?));
        match found {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn create_service(&self, service: &Path) -> io::Result<()> {
        let (at, name) = self.parent(service)?;
        match rustix::fs::mkdirat(&at, name, Mode::from_raw_mode(0o700)) {
            Err(Errno::EXIST) => {}
            made => made?,
        }
        let fd = rustix::fs::openat(&at, name, dir_flags(), Mode::empty())?;
        self.give(&fd)?;
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o755))?;
        Ok(rustix::fs::fsync(&at)?)
    }

    fn lock(&self, service: &Path) -> io::Result<ServiceLock> {
        use rustix::fs::FlockOperation;
        let fd = self.open_at(service, true)?;
        rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive)?;
        Ok(ServiceLock { _held: fd })
    }

    fn create_stage(&self, service: &Path) -> io::Result<PathBuf> {
        let random = RandomState::new().build_hasher().finish();
        let stage = service.join(format!(".stage-{random:016x}"));
        let (at, name) = self.parent(&stage)?;
        self.make_dir(&at, name, 0o700)?;
        Ok(stage)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let (at, name) = self.parent(path)?;
        self.make_dir(&at, name, 0o700)
    }

    fn create_file(&self, path: &Path) -> io::Result<Box<dyn StagedFile>> {
        let (at, name) = self.parent(path)?;
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let fd = rustix::fs::openat(
            &at,
            name,
            flags | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?;
        self.give(&fd)?;
        Ok(Box::new(HostFile(File::from(fd))))
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        let fd = self.open_at(path, false)?;
        set_mode_of(fd, mode)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let fd = self.open_at(path, true)?;
        Ok(rustix::fs::fsync(&fd)?)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (from_at, from_name) = self.parent(from)?;
        let (to_at, to_name) = self.parent(to)?;
        Ok(rustix::fs::renameat(&from_at, from_name, &to_at, to_name)?)
    }

    fn remove_tree(&self, path: &Path) -> io::Result<()> {
        let (at, name) = self.parent(path)?;
        remove_at(&at, name)
    }

    fn real_path(&self, path: &Path) -> PathBuf {
        self.root.join(path)
    }
}

/// The names in the open directory `fd`.
fn names(fd: &OwnedFd) -> io::Result<Vec<OsString>> {
    let mut out = Vec::new();
    for entry in Dir::read_from(fd)? {
        let name = entry?.file_name().to_bytes().to_vec();
        if (name != b".") & (name != b"..") {
            out.push(OsStr::from_bytes(&name).to_os_string());
        }
    }
    Ok(out)
}

/// Removes `name` in the directory `at` and everything below it, never following a link. A
/// directory is opened to its owner first, so a read-only tree can be emptied.
fn remove_at(at: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let stat = match rustix::fs::statat(at, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(()),
        other => other?,
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Ok(rustix::fs::unlinkat(at, name, AtFlags::empty())?);
    }
    let fd = rustix::fs::openat(at, name, dir_flags(), Mode::empty())?;
    rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o700))?;
    for child in names(&fd)? {
        remove_at(&fd, &child)?;
    }
    Ok(rustix::fs::unlinkat(at, name, AtFlags::REMOVEDIR)?)
}

/// A file [`HostFs`] created.
struct HostFile(File);

impl StagedFile for HostFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }

    fn set_mode(&mut self, mode: u32) -> io::Result<()> {
        self.0.set_permissions(Permissions::from_mode(mode))
    }

    fn sync(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}
