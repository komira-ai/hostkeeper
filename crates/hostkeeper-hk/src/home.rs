//! `$HOME/.hostkeeper`, where `hk` keeps the files that are its own: the settings file
//! (`hk.toml`, [`crate::settings`]) and the `xcode` directory of the records and the lock.
//!
//! Everything `hk` reads from there steers a call, so it must be the user's own and nobody
//! else's to change: [`Home::check_dir`] and [`Home::check_file`] refuse a link, a path of
//! another owner and one that group or others may write, naming the path. The user is
//! [`World::uid`], so a test can have another owner without root.
//!
//! - [`Home::ensure`] makes `~/.hostkeeper` and `~/.hostkeeper/xcode` with mode 0700 when they
//!   are missing, whatever the umask (the mode is set after the directory is made), and checks
//!   each that exists; a directory that exists keeps the mode it has.
//! - [`Home::create_file`] makes a new file with mode 0600, whatever the umask, and refuses to
//!   replace one.
//! - [`Home::replace_file`] makes the file the same way and puts it in the place of one that is
//!   there (a record that is kept up to date), through a new file renamed over the old, so a
//!   reader never sees half of one.
//!
//! Any verb that writes under `~/.hostkeeper` goes through `ensure` first. This is the one place
//! that makes these directories and files; it is not used by `hk ssh`, whose state is in
//! `~/.hostkeeper/ssh` and is made by the `ssh` module.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use crate::{Flow, Stop, World};

/// The directory under `$HOME`.
const DIR: &str = ".hostkeeper";

/// The settings file in it.
const SETTINGS: &str = "hk.toml";

/// The directory of the Xcode records and the lock, in it.
const XCODE: &str = "xcode";

/// The mode of a directory `hk` makes.
const DIR_MODE: u32 = 0o700;

/// The mode of a file `hk` makes.
const FILE_MODE: u32 = 0o600;

/// The bits of a mode that let group or others change what it is the mode of.
const OTHERS_WRITE: u32 = 0o022;

/// `$HOME/.hostkeeper` of the user running `hk`.
pub(crate) struct Home {
    dir: PathBuf,
    uid: u32,
}

impl Home {
    /// The directory under the world's home; `None` when it has no home or an empty one.
    pub(crate) fn of(w: &World<'_>) -> Option<Home> {
        let home = w
            .home
            .as_ref()
            .filter(|home| !home.as_os_str().is_empty())?;
        Some(Home {
            dir: home.join(DIR),
            uid: w.uid,
        })
    }

    /// `~/.hostkeeper/hk.toml`.
    pub(crate) fn settings(&self) -> PathBuf {
        self.dir.join(SETTINGS)
    }

    /// Whether `path` is the directory `~/.hostkeeper` or a file in it that exists, after
    /// checking it is the user's own: `false` when it is missing. A link, a path of another
    /// owner, one that group or others may write, and one that is not a directory (when
    /// `directory`) or not a regular file (when not) are usage errors naming `path`.
    fn look(&self, path: &Path, directory: bool) -> Flow<bool> {
        let shown = path.display();
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(Stop::usage(format!("cannot look at {shown}: {error}")));
            }
        };
        if meta.file_type().is_symlink() {
            return Err(Stop::usage(format!(
                "{shown} is a link; hk does not follow it"
            )));
        }
        if !(if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        }) {
            let kind = if directory {
                "a directory"
            } else {
                "a regular file"
            };
            return Err(Stop::usage(format!("{shown} is not {kind}")));
        }
        if meta.uid() != self.uid {
            return Err(Stop::usage(format!(
                "{shown} is not owned by the user running hk (user id {}; its owner is {})",
                self.uid,
                meta.uid()
            )));
        }
        if meta.mode() & OTHERS_WRITE != 0 {
            return Err(Stop::usage(format!(
                "{shown} may be changed by its group or others; run chmod go-w on it"
            )));
        }
        Ok(true)
    }

    /// Whether the directory `path` (`~/.hostkeeper` or one in it) exists, after the checks of
    /// [`Home::look`].
    pub(crate) fn check_dir(&self, path: &Path) -> Flow<bool> {
        self.look(path, true)
    }

    /// Whether the file `path` (in `~/.hostkeeper`) exists, after the checks of [`Home::look`].
    pub(crate) fn check_file(&self, path: &Path) -> Flow<bool> {
        self.look(path, false)
    }

    /// Whether `~/.hostkeeper` exists, after checking it.
    pub(crate) fn exists(&self) -> Flow<bool> {
        self.check_dir(&self.dir)
    }

    /// Makes `~/.hostkeeper` and `~/.hostkeeper/xcode` with mode 0700 when they are missing, and
    /// checks each that exists; the `xcode` directory.
    pub(crate) fn ensure(&self) -> Flow<PathBuf> {
        let xcode = self.dir.join(XCODE);
        for dir in [&self.dir, &xcode] {
            if self.check_dir(dir)? {
                continue;
            }
            let fail = |error: std::io::Error| {
                Stop::fail(format!("cannot make {}: {error}", dir.display()))
            };
            DirBuilder::new().mode(DIR_MODE).create(dir).map_err(fail)?;
            // The umask only takes bits away; the mode is what it is said to be.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
                .map_err(fail)?;
        }
        Ok(xcode)
    }

    /// Makes the new file `path` with mode 0600 holding `bytes`; a file that is there already, a
    /// link included, is never replaced.
    pub(crate) fn create_file(&self, path: &Path, bytes: &[u8]) -> Flow<()> {
        let shown = path.display();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Stop::usage(format!("{shown} exists; it is not replaced"))
                } else {
                    Stop::fail(format!("cannot make {shown}: {error}"))
                }
            })?;
        let written = file
            .set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
            .and_then(|()| file.write_all(bytes))
            .and_then(|()| file.sync_all());
        if let Err(error) = written {
            // What was made is not left half written.
            let _ = std::fs::remove_file(path);
            return Err(Stop::fail(format!("cannot write {shown}: {error}")));
        }
        Ok(())
    }

    /// Makes the file `path` with mode 0600 holding `bytes`, in the place of what is there: the
    /// bytes are written to a new file beside it, which is renamed over `path`. A leftover of an
    /// earlier try is removed first, and nothing is left behind when the rename fails.
    pub(crate) fn replace_file(&self, path: &Path, bytes: &[u8]) -> Flow<()> {
        let mut name = path.as_os_str().to_owned();
        name.push(".new");
        let new = PathBuf::from(name);
        // A leftover that is not there is not a failure; one that cannot be removed is said by
        // the creation below.
        let _ = std::fs::remove_file(&new);
        self.create_file(&new, bytes)?;
        std::fs::rename(&new, path).map_err(|error| {
            let _ = std::fs::remove_file(&new);
            Stop::fail(format!("cannot replace {}: {error}", path.display()))
        })
    }
}
