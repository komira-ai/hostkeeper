//! The filesystem port: every read and write of this crate goes through an [`Fs`], with paths
//! spelt as the host names them. [`RealFs`] reads and writes them under a root directory (`/` on
//! a host, a scratch directory in a test); [`crate::testing::FaultFs`] wraps it to record each
//! write and to fail the n-th one, which is how a crash between any two writes is simulated.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

/// What is at a path, the path itself and not what a link at it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    /// A socket, a pipe or a device.
    Other,
}

/// The filesystem operations of this crate. Reads: [`Fs::read`], [`Fs::kind`],
/// [`Fs::read_link`], [`Fs::list`]. Every other method writes.
pub trait Fs {
    /// Where `path` is on the disk this process sees: the path a loader that opens files itself
    /// (`Manifest::load`) is given.
    fn host_path(&self, path: &Path) -> PathBuf;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// What is at `path`, not following a link; `None` when nothing is.
    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>>;
    /// The target of the link at `path`, as it was written.
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;
    /// The names in the directory `dir`, sorted.
    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    /// Creates or truncates the file `path`, not through a link, and writes `bytes` to it.
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// `fsync` of the file `path`.
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    /// `fsync` of the directory `dir`.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Makes a link at `link` whose target is `target`, written as it is.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Makes `dir` and its missing parents, each mode 0755 whatever the creation mask.
    fn create_dirs(&self, dir: &Path) -> io::Result<()>;
}

/// The filesystem, every path read under `root`.
#[derive(Clone, Debug)]
pub struct RealFs {
    root: PathBuf,
}

impl RealFs {
    /// Reads and writes every path under `root`: `/` on a host.
    pub fn new(root: &Path) -> RealFs {
        RealFs {
            root: root.to_path_buf(),
        }
    }
}

/// The kind of a file type: the first of link, directory and regular file it is, or `Other`.
fn kind_of(file_type: fs::FileType) -> FileKind {
    [
        (file_type.is_symlink(), FileKind::Symlink),
        (file_type.is_dir(), FileKind::Dir),
        (file_type.is_file(), FileKind::File),
    ]
    .into_iter()
    .find_map(|(is, kind)| is.then_some(kind))
    .unwrap_or(FileKind::Other)
}

impl Fs for RealFs {
    /// `path` under the root, with only its plain components: `/`, `.` and `..` are dropped,
    /// so no spelling of a path reaches above the root. Links on the way are the host's own and
    /// are followed as the host follows them.
    fn host_path(&self, path: &Path) -> PathBuf {
        let plain: PathBuf = path
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .collect();
        self.root.join(plain)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(self.host_path(path))
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        match fs::symlink_metadata(self.host_path(path)) {
            Ok(meta) => Ok(Some(kind_of(meta.file_type()))),
            Err(e) => (e.kind() == io::ErrorKind::NotFound)
                .then_some(None)
                .ok_or(e),
        }
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        fs::read_link(self.host_path(path))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        let mut names = fs::read_dir(self.host_path(dir))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.host_path(path))?
            .write_all(bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        File::open(self.host_path(path))?.sync_all()
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(self.host_path(dir))?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(self.host_path(from), self.host_path(to))
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        symlink(target, self.host_path(link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(self.host_path(path))
    }

    /// Each directory made gets mode 0755 whatever the creation mask; directories that were
    /// there are left as they are.
    fn create_dirs(&self, dir: &Path) -> io::Result<()> {
        let host = self.host_path(dir);
        let missing: Vec<PathBuf> = host
            .ancestors()
            .take_while(|at| fs::symlink_metadata(at).is_err())
            .map(Path::to_path_buf)
            .collect();
        DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&host)?;
        missing
            .iter()
            .try_for_each(|made| fs::set_permissions(made, fs::Permissions::from_mode(0o755)))
    }
}
