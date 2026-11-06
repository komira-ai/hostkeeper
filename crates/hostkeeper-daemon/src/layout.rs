//! The state directories the daemon owns. On Linux `hostkeeper.tmpfiles.conf` makes them at
//! boot; a Mac has no tmpfiles, so the daemon makes the ones it is missing at each start, with
//! the modes that file gives them.
//!
//! The other two directories of that file are made elsewhere: the state directory (and its
//! lock) by [`crate::start`], and the admin socket's directory by [`crate::socket::bind`]. The
//! config directory is the installer's, and a missing config is an error of its own.

use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::start::{Settings, StartError};

/// The name of the directory of the update hooks' records, beside the state directory
/// (`/var/lib/updater/hooks` beside `/var/lib/updater/state`).
pub const HOOKS_DIR: &str = "hooks";

/// Makes the packages directory (mode 0755) and the hooks directory (0700) when they are
/// missing, and the parents they need (0755). A directory that exists is left as it is, its
/// mode and contents included. Something at either path that is not a directory, a link
/// included, stops the start.
pub fn ensure_state_dirs(settings: &Settings) -> Result<(), StartError> {
    for (dir, mode) in dirs(settings) {
        ensure_dir(&dir, mode).map_err(|source| StartError::Io { path: dir, source })?;
    }
    Ok(())
}

fn dirs(settings: &Settings) -> [(PathBuf, u32); 2] {
    let hooks = settings
        .state_dir
        .parent()
        .unwrap_or(&settings.state_dir)
        .join(HOOKS_DIR);
    [(settings.packages_dir.clone(), 0o755), (hooks, 0o700)]
}

fn ensure_dir(dir: &Path, mode: u32) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir.parent().unwrap_or(dir))?;
    let made = match DirBuilder::new().mode(mode).create(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e),
    };
    if !fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory (a file or a link)", dir.display()),
        ));
    }
    if made {
        return fs::set_permissions(dir, Permissions::from_mode(mode));
    }
    Ok(())
}
