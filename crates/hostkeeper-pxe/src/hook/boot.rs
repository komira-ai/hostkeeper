//! The files of the boot directory: the symlink `current` and the version directories.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;

/// The symlink to the version served.
pub(super) const CURRENT: &str = "current";
/// The name the new link is made under, then renamed over `current`.
const NEXT: &str = "current-next";

/// Points `<boot_dir>/current` at `version` (a relative target), atomically: a new link is made
/// under another name and renamed over the old.
pub(super) fn point_current(boot_dir: &Path, version: &str) -> io::Result<()> {
    let next = boot_dir.join(NEXT);
    remove_file(&next)?;
    symlink(version, &next)?;
    fs::rename(&next, boot_dir.join(CURRENT))
}

/// Whether `path` is a directory (not a link to one).
pub(super) fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Removes the file or link `path`; one that is not there is not an error. (The boot directory
/// is locked, so nothing else removes it between the look and the removal.)
pub(super) fn remove_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(_) => Ok(()),
        Ok(_) => fs::remove_file(path),
    }
}

/// Removes the directory `path` and all it holds; one that is not there is not an error.
pub(super) fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(_) => Ok(()),
        Ok(_) => fs::remove_dir_all(path),
    }
}
