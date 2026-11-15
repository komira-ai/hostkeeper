//! A config file written only when it is absent: the rule of `hostkeeper_daemon::mac_config`
//! (docs/design/mac-bootstrap.md, section 6), "a reinstall never changes a Mac's trust root".
//!
//! A file that is there as a regular file is kept as it is. Anything else at the path (a link, a
//! directory) is refused and left. Otherwise a new temporary file in the same directory, with the
//! file's mode and synced, is hard-linked to the path, which fails when the file appeared
//! meanwhile (that file is kept), and the temporary file is removed, with no copy left behind on
//! any failure.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// What [`write_if_absent`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    /// The file was made.
    Made,
    /// A file was there already, and is unchanged.
    Kept,
}

/// Why a file was not written.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// Something other than a regular file is at the path.
    #[error("{} is there but is not a regular file; it is not replaced", path.display())]
    NotAFile { path: PathBuf },
    /// A file that is there is writable by its group or others, so it is not trusted as it is.
    #[error("{} is writable by its group or others; it is not trusted", path.display())]
    Writable { path: PathBuf },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> FileError {
    let path = path.to_path_buf();
    |source| FileError::Io { path, source }
}

/// What is at `path`: `None` when nothing, [`Written::Kept`] for a regular file.
fn existing(path: &Path) -> Result<Option<Written>, FileError> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(path)(e)),
        Ok(meta) if meta.file_type().is_file() && meta.mode() & 0o022 != 0 => {
            Err(FileError::Writable {
                path: path.to_path_buf(),
            })
        }
        Ok(meta) if meta.file_type().is_file() => Ok(Some(Written::Kept)),
        Ok(_) => Err(FileError::NotAFile {
            path: path.to_path_buf(),
        }),
    }
}

/// Writes `text` to `path` with `mode` unless a regular file is there.
pub fn write_if_absent(path: &Path, text: &str, mode: u32) -> Result<Written, FileError> {
    write_if_absent_with(path, text, mode, &|| {})
}

/// [`write_if_absent`], with `between` run after the temporary file is written and before it is
/// linked to `path`, where a file can appear.
pub fn write_if_absent_with(
    path: &Path,
    text: &str,
    mode: u32,
    between: &dyn Fn(),
) -> Result<Written, FileError> {
    if let Some(kept) = existing(path)? {
        return Ok(kept);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .map_err(io_at(dir))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = dir.join(format!(".{name}.bootstrap-{}", std::process::id()));
    drop(fs::remove_file(&temporary));
    let made = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .map_err(io_at(&temporary))
        .and_then(|()| {
            between();
            match fs::hard_link(&temporary, path) {
                Ok(()) => Ok(Written::Made),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    Ok(existing(path)?.unwrap_or(Written::Kept))
                }
                Err(e) => Err(io_at(path)(e)),
            }
        });
    drop(fs::remove_file(&temporary));
    made
}
