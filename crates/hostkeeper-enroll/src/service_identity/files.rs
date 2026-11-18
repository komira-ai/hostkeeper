//! The identity directory on disk: `<root>/<service>/{key.pem,cert.pem,ca.pem}` ([`IdentityDir`],
//! the production [`IdentityFiles`]).
//!
//! - The root is made `root:root 0755` when it is absent, and refused when it is a link, not a
//!   directory, not owned by root or writable by group or others.
//! - A service's directory is owned by root with the service account's group, mode `0750`: the
//!   account reads its files but cannot create, rename or replace any. It is made and repaired
//!   when a file is written, and refused when it is a link, not a directory or not owned by root.
//! - A file is written to a new temporary file in the service's directory, which is owned by the
//!   account and moded `0600` before any byte is written, synced, renamed over the old file, and
//!   the directory is synced. A reader sees the old file or the new one.
//! - A file is read through `openat` with `O_NOFOLLOW` and `O_NONBLOCK`, and refused unless it is
//!   a regular file no larger than [`MAX_FILE_BYTES`]. A link is never followed. A file owned by
//!   another user than the account is absent as far as a read goes: it is not read and not
//!   trusted (a service whose `identityAccount` changed has such files), and the write that
//!   follows replaces it by rename with a file owned by the account.
//! - Removing a service removes its directory and what is in it without following any link;
//!   [`IdentityFiles::services`] names the directories there are.
//!
//! "Root" is the [`Owner`] the directory is built with: uid 0 and gid 0 on a host
//! ([`IdentityDir::host`]), the effective user in a test that is not root.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use hostkeeper_config::names::is_service_name;
use rustix::fs::{AtFlags, CWD, Dir, FileType, Gid, Mode, OFlags, Uid};
use rustix::io::Errno;
use rustix::path::Arg;

/// The most bytes of a file this module reads: a certificate chain or a key is far smaller.
pub const MAX_FILE_BYTES: u64 = 16 * 1024;

/// The mode of the identity root.
const ROOT_MODE: u32 = 0o755;
/// The mode of a service's directory.
const DIR_MODE: u32 = 0o750;
/// The mode of every file.
const FILE_MODE: u32 = 0o600;

/// A user and a group, by number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

/// The files of a service's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityFile {
    /// The service's key, PKCS#8 PEM.
    Key,
    /// The service's certificate, PEM.
    Cert,
    /// The CA bundle the host trusts, PEM.
    Ca,
}

impl IdentityFile {
    /// The file's name in the service's directory.
    pub const fn name(self) -> &'static str {
        match self {
            IdentityFile::Key => "key.pem",
            IdentityFile::Cert => "cert.pem",
            IdentityFile::Ca => "ca.pem",
        }
    }
}

/// Why a file or directory of the identity directory was refused or could not be used.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{0:?} is not a service name")]
    ServiceName(String),
    #[error("{0} is not a directory (a link or a file is not followed)")]
    NotDirectory(PathBuf),
    #[error("{path} is owned by user {found}, not by {expected}")]
    OtherOwner {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
    #[error("{0} is writable by group or others")]
    Writable(PathBuf),
    #[error("{0} is a symbolic link, which is not followed")]
    Symlink(PathBuf),
    #[error("{0} is not a regular file")]
    NotRegular(PathBuf),
    #[error("{0} is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge(PathBuf),
}

/// `found` owns `path` when `expected` should.
fn check_owner(path: &Path, found: u32, expected: u32) -> Result<(), FilesError> {
    if found == expected {
        return Ok(());
    }
    Err(FilesError::OtherOwner {
        path: path.to_path_buf(),
        found,
        expected,
    })
}

/// The conversion of an I/O error (or an `Errno`) of `path` into a [`FilesError`].
fn at<E: Into<io::Error>>(path: &Path) -> impl Fn(E) -> FilesError + '_ {
    move |source| io_error(path, source)
}

/// An I/O error `source` (or an `Errno`) of `path`.
fn io_error(path: &Path, source: impl Into<io::Error>) -> FilesError {
    FilesError::Io {
        path: path.to_path_buf(),
        source: source.into(),
    }
}

/// The port to a host's identity directory; see the module documentation.
pub trait IdentityFiles: Send {
    /// Makes the service's directory (and the root) when absent and sets its owner and mode.
    ///
    /// # Errors
    /// [`FilesError`]: a name, link, owner or mode that is refused, or an I/O error.
    fn prepare(&self, service: &str, owner: Owner) -> Result<(), FilesError>;

    /// The bytes of `file` of `service`, owned by `owner`'s user; none when the service's
    /// directory or the file is absent, or the file is owned by another user.
    ///
    /// # Errors
    /// [`FilesError`]: a link, a file that is not regular or too large, or an I/O error.
    fn read(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
    ) -> Result<Option<Vec<u8>>, FilesError>;

    /// Writes `file` of `service` durably, owned by `owner`, mode `0600`.
    ///
    /// # Errors
    /// [`FilesError`], as [`IdentityFiles::prepare`].
    fn write(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
        bytes: &[u8],
    ) -> Result<(), FilesError>;

    /// Removes the service's directory and what it holds; an absent one is fine.
    ///
    /// # Errors
    /// [`FilesError`]: a name that is refused, a directory inside it, or an I/O error.
    fn remove(&self, service: &str) -> Result<(), FilesError>;

    /// The names of the services that have an entry in the root, whatever the entry is; an entry
    /// whose name is not a service name is not listed. Empty when the root is absent.
    ///
    /// # Errors
    /// [`FilesError`]: a root that is refused, or an I/O error.
    fn services(&self) -> Result<Vec<String>, FilesError>;
}

/// The identity directory at a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityDir {
    root: PathBuf,
    trusted: Owner,
}

impl IdentityDir {
    /// The identity directory of a host: `root`, owned by root.
    pub fn host(root: &Path) -> IdentityDir {
        IdentityDir::owned_by(root, Owner { uid: 0, gid: 0 })
    }

    /// The identity directory at `root`, whose "root" is `trusted`.
    pub fn owned_by(root: &Path, trusted: Owner) -> IdentityDir {
        IdentityDir {
            root: root.to_path_buf(),
            trusted,
        }
    }

    fn service_path(&self, service: &str) -> Result<PathBuf, FilesError> {
        if !is_service_name(service) {
            return Err(FilesError::ServiceName(service.to_owned()));
        }
        Ok(self.root.join(service))
    }

    /// The root, opened and checked; made first when `create` is set. None when it is absent and
    /// `create` is not.
    fn open_root(&self, create: bool) -> Result<Option<OwnedFd>, FilesError> {
        let mut opened = open_dir(CWD, &self.root);
        if create && opened.as_ref().is_err_and(|errno| *errno == Errno::NOENT) {
            self.make_root()?;
            opened = open_dir(CWD, &self.root);
        }
        let fd = match opened {
            Err(Errno::NOENT) => return Ok(None),
            other => other.map_err(|errno| not_directory(&self.root, errno))?,
        };
        let stat = rustix::fs::fstat(&fd).map_err(at(&self.root))?;
        check_owner(&self.root, stat.st_uid, self.trusted.uid)?;
        if stat.st_mode & 0o022 != 0 {
            return Err(FilesError::Writable(self.root.clone()));
        }
        Ok(Some(fd))
    }

    fn make_root(&self) -> Result<(), FilesError> {
        let parent = self.root.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(parent).map_err(at(parent))?;
        rustix::fs::mkdirat(CWD, &self.root, Mode::from_raw_mode(ROOT_MODE))
            .map_err(at(&self.root))?;
        let fd = open_dir(CWD, &self.root).map_err(|errno| not_directory(&self.root, errno))?;
        settle(&fd, self.trusted, ROOT_MODE).map_err(at(&self.root))
    }

    /// A service's directory, opened without following a link and checked to be root's; made
    /// first and set to `owner`'s group and mode `0750` when `owner` is given. None when it is
    /// absent and not made.
    fn open_service(
        &self,
        root: &OwnedFd,
        service: &str,
        owner: Option<Owner>,
    ) -> Result<Option<OwnedFd>, FilesError> {
        let path = self.service_path(service)?;
        if owner.is_some() {
            let made = rustix::fs::mkdirat(root, service, Mode::from_raw_mode(DIR_MODE));
            made.or_else(|errno| {
                if errno == Errno::EXIST {
                    Ok(())
                } else {
                    Err(errno)
                }
            })
            .map_err(at(&path))?;
        }
        let fd = match open_dir(root, service) {
            Err(Errno::NOENT) => return Ok(None),
            other => other.map_err(|errno| not_directory(&path, errno))?,
        };
        let stat = rustix::fs::fstat(&fd).map_err(at(&path))?;
        check_owner(&path, stat.st_uid, self.trusted.uid)?;
        if let Some(owner) = owner {
            let group = Owner {
                uid: self.trusted.uid,
                gid: owner.gid,
            };
            settle(&fd, group, DIR_MODE).map_err(at(&path))?;
        }
        Ok(Some(fd))
    }

    /// The service's directory, made when absent.
    fn made_service(&self, service: &str, owner: Owner) -> Result<OwnedFd, FilesError> {
        let path = self.service_path(service)?;
        let root = self.open_root(true)?;
        let root = root.ok_or_else(|| FilesError::NotDirectory(self.root.clone()))?;
        let found = self.open_service(&root, service, Some(owner))?;
        // The root was just made and the directory with it, or an error was returned: the
        // directory is absent only if it vanished between the two calls.
        found.ok_or(FilesError::NotDirectory(path))
    }
}

/// `path` as a directory under `at`, without following a link.
fn open_dir(at: impl AsFd, path: impl Arg) -> Result<OwnedFd, Errno> {
    rustix::fs::openat(
        at,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

/// What opening a directory without following a link says of `path`: not a directory when it is
/// a link or a file, an I/O error otherwise.
fn not_directory(path: &Path, errno: Errno) -> FilesError {
    match errno {
        Errno::NOTDIR | Errno::LOOP => FilesError::NotDirectory(path.to_path_buf()),
        other => io_error(path, other),
    }
}

/// Gives `fd` to `owner` and sets its mode.
fn settle(fd: &OwnedFd, owner: Owner, mode: u32) -> Result<(), Errno> {
    rustix::fs::fchown(
        fd,
        Some(Uid::from_raw(owner.uid)),
        Some(Gid::from_raw(owner.gid)),
    )?;
    rustix::fs::fchmod(fd, Mode::from_raw_mode(mode))
}

impl IdentityFiles for IdentityDir {
    fn prepare(&self, service: &str, owner: Owner) -> Result<(), FilesError> {
        self.made_service(service, owner).map(drop)
    }

    fn read(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
    ) -> Result<Option<Vec<u8>>, FilesError> {
        let path = self.service_path(service)?.join(file.name());
        let Some(root) = self.open_root(false)? else {
            return Ok(None);
        };
        let Some(dir) = self.open_service(&root, service, None)? else {
            return Ok(None);
        };
        let opened = rustix::fs::openat(
            &dir,
            file.name(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        );
        let fd = match opened {
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP) => return Err(FilesError::Symlink(path)),
            other => other.map_err(at(&path))?,
        };
        let stat = rustix::fs::fstat(&fd).map_err(at(&path))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(FilesError::NotRegular(path));
        }
        if stat.st_uid != owner.uid {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(at(&path))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(FilesError::TooLarge(path));
        }
        Ok(Some(bytes))
    }

    fn write(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        let path = self.service_path(service)?.join(file.name());
        let dir = self.made_service(service, owner)?;
        let tmp = format!(".{}.tmp", file.name());
        let stale = rustix::fs::unlinkat(&dir, &tmp, AtFlags::empty());
        stale
            .or_else(|errno| {
                if errno == Errno::NOENT {
                    Ok(())
                } else {
                    Err(errno)
                }
            })
            .map_err(at(&path))?;
        let fd = rustix::fs::openat(
            &dir,
            &tmp,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(FILE_MODE),
        )
        .map_err(at(&path))?;
        settle(&fd, owner, FILE_MODE).map_err(at(&path))?;
        let mut out = File::from(fd);
        out.write_all(bytes).map_err(at(&path))?;
        out.sync_all().map_err(at(&path))?;
        rustix::fs::renameat(&dir, &tmp, &dir, file.name()).map_err(at(&path))?;
        rustix::fs::fsync(&dir).map_err(at(&path))
    }

    fn remove(&self, service: &str) -> Result<(), FilesError> {
        let path = self.service_path(service)?;
        let Some(root) = self.open_root(false)? else {
            return Ok(());
        };
        match rustix::fs::unlinkat(&root, service, AtFlags::empty()) {
            Err(Errno::ISDIR | Errno::PERM) => {}
            Err(Errno::NOENT) => return Ok(()),
            other => {
                return other
                    .map_err(at(&path))
                    .and_then(|()| rustix::fs::fsync(&root).map_err(at(&path)));
            }
        }
        let dir = open_dir(&root, service).map_err(|errno| not_directory(&path, errno))?;
        for entry in Dir::read_from(&dir).map_err(at(&path))? {
            let entry = entry.map_err(at(&path))?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            rustix::fs::unlinkat(&dir, name, AtFlags::empty()).map_err(at(&path))?;
        }
        rustix::fs::unlinkat(&root, service, AtFlags::REMOVEDIR).map_err(at(&path))?;
        rustix::fs::fsync(&root).map_err(at(&path))
    }

    fn services(&self) -> Result<Vec<String>, FilesError> {
        let Some(root) = self.open_root(false)? else {
            return Ok(Vec::new());
        };
        let mut found = Vec::new();
        for entry in Dir::read_from(&root).map_err(at(&self.root))? {
            let entry = entry.map_err(at(&self.root))?;
            if let Ok(name) = entry.file_name().to_str()
                && is_service_name(name)
            {
                found.push(name.to_owned());
            }
        }
        found.sort();
        Ok(found)
    }
}
