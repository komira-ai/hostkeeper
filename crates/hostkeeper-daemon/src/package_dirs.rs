//! The start's setting of the modes of the directories that keep packages to the store's layout
//! ([`hostkeeper_install::version`] gives it and the reason for each mode): the packages
//! directory and each service directory in it [`PACKAGES_MODE`] and [`SERVICE_MODE`] (0711,
//! passed through by every user, listed by root alone), each version an installer published
//! [`VERSION_MODE`] (0700, root's alone: it holds the package file and a copy of its tree), and
//! each version an earlier release unpacked from hostkeeper's own archive, which its service runs
//! from as its own user, [`ARCHIVE_VERSION_MODE`] (0555, as that release made it).
//!
//! Earlier releases left these directories otherwise: one left everything open (the packages
//! and service directories 0755, every version 0555), and the next closed everything to root
//! (0700), the archive versions too, so that a service run as its own user from one could not
//! start again. At each start, before anything is staged, the daemon gives each directory whose
//! mode is not its layout's that mode:
//!
//! - the packages directory, then each directory in it (a service directory);
//! - in each service directory, each directory not named with a leading `.`: an archive version
//!   when it holds a regular file `manifest.toml` and nothing named `target.json` (what the
//!   archive's unpack made, and what no installer makes), otherwise a published version;
//! - a directory named with a leading `.` (a publish or a prune under way) is closed (0700) when
//!   its group or others can reach it, and otherwise left as it is;
//! - a regular file in the packages directory or in a service directory (a seal, a seal being
//!   written, a download) is closed to its owner, its group's and others' bits cleared, when
//!   they have any: an earlier release wrote each seal 0444, and a service directory every user
//!   passes through must hold nothing another user can read;
//! - then the apt work directories' parent and the mirrors' parent beside the packages directory
//!   are closed (0700) the same way.
//!
//! Nothing below a version directory is changed: the modes there are part of the tree digest its
//! seal holds. The mode of a version directory itself is not part of the digest, so every seal
//! still holds. A link (`current`, `previous`), anything that is neither a directory nor a regular
//! file, and an entry that is not there are left as they are; a version whose top cannot be looked at is left as it
//! is and is a failure.
//!
//! What it set is said in one log line, and what it could not set is the alert [`ALERT_START`];
//! the start goes on either way. The services whose archive version `current` names it gave
//! [`ARCHIVE_VERSION_MODE`] are in its answer: their run units could not start while the version
//! was closed, so the start starts them again ([`crate::run_units`]). An archive version that
//! only `previous` names, or none, is opened too, and starts nothing.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_install::version::{
    ARCHIVE_VERSION_MODE, PACKAGES_MODE, SERVICE_MODE, VERSION_MODE,
};

use crate::boot::ALERT_START;
use crate::executor::Ctx;

/// The mode the start gives a directory beside the packages directory, or a directory under way
/// in a service directory, that its group or others can reach.
pub const CLOSED: u32 = 0o700;

/// The file at the top of an archive version: the archive's own manifest.
pub const ARCHIVE_MANIFEST: &str = "manifest.toml";

/// The link in a service directory to the version the host runs.
const CURRENT: &str = "current";

/// The file at the top of a version an installer published ([`crate::target`]).
const TARGET_FILE: &str = "target.json";

/// What [`set_dirs`] did.
#[derive(Debug, Default)]
pub struct Set {
    /// The directories and files whose mode was set, outermost first, each with the mode it was
    /// given.
    pub set: Vec<(PathBuf, u32)>,
    /// The services, by the name of their directory, whose archive version `current` names was
    /// given [`ARCHIVE_VERSION_MODE`], in name order, each once.
    pub opened: Vec<String>,
    /// The directories that could not be looked at, listed or set, and why.
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// What the start did to the package directories: [`Set::set`] and [`Set::opened`] of
/// [`set_dirs`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageDirs {
    /// The directories and files whose mode was set, outermost first, each with the mode it was
    /// given.
    pub set: Vec<(PathBuf, u32)>,
    /// The services whose archive version `current` names was opened to their users.
    pub opened: Vec<String>,
}

/// Sets the modes of the daemon's package directories and of the apt work directories' and
/// mirrors' parents ([the module](self)), says so, and answers what it set.
pub fn set(ctx: &Ctx) -> PackageDirs {
    let dirs = &ctx.repository_ports.dirs;
    let done = set_dirs(&ctx.dirs.packages, &[&dirs.work, &dirs.mirror]);
    if !done.set.is_empty() {
        let paths: Vec<String> = done
            .set
            .iter()
            .map(|(path, mode)| format!("{}={mode:04o}", path.display()))
            .collect();
        ctx.log(&format!(
            "level=info set the package directories an earlier release left otherwise to the \
             modes of the store's layout: {}",
            paths.join(" ")
        ));
    }
    if !done.failed.is_empty() {
        let failed: Vec<String> = done
            .failed
            .iter()
            .map(|(path, error)| format!("{}: {error}", path.display()))
            .collect();
        ctx.alert(
            ALERT_START,
            &format!(
                "a package directory was not given the mode of the store's layout: {}",
                failed.join("; ")
            ),
        );
    }
    PackageDirs {
        set: done.set,
        opened: done.opened,
    }
}

/// Sets `packages`, each directory in it and each directory in those to their layout's modes,
/// then closes each of `beside` ([the module](self)), each one before what is in it.
pub fn set_dirs(packages: &Path, beside: &[&Path]) -> Set {
    let mut done = Set::default();
    if give(packages, PACKAGES_MODE, &mut done) {
        for service in subdirs(packages, &mut done) {
            if give(&service, SERVICE_MODE, &mut done) {
                for version in subdirs(&service, &mut done) {
                    set_version(&service, &version, &mut done);
                }
            }
        }
    }
    for dir in beside {
        close(dir, &mut done);
    }
    done
}

/// Gives the entry `version` of `service`, when it is a directory, the mode of its kind, or
/// closes it when it is under way (a leading `.`) or a regular file (a seal, a download). A link
/// (`current`, `previous`) and an entry gone since the listing are left as they are.
fn set_version(service: &Path, version: &Path, done: &mut Set) {
    match fs::symlink_metadata(version) {
        Ok(meta) if meta.is_dir() => {}
        Ok(meta) if meta.is_file() => {
            close(version, done);
            return;
        }
        Ok(_) => return,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => return done.failed.push((version.to_path_buf(), e)),
    }
    let under_way = version
        .file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
    if under_way {
        close(version, done);
        return;
    }
    match is_archive(version) {
        Ok(true) => {
            let before = done.set.len();
            give(version, ARCHIVE_VERSION_MODE, done);
            let opened = (done.set.len() > before) & is_current(service, version);
            let name = service
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            if let Some(name) = name.filter(|name| opened & !done.opened.contains(name)) {
                done.opened.push(name);
            }
        }
        Ok(false) => {
            give(version, VERSION_MODE, done);
        }
        Err(e) => done.failed.push((version.to_path_buf(), e)),
    }
}

/// Whether `service`'s `current` link names `version` (the version its run units start from).
fn is_current(service: &Path, version: &Path) -> bool {
    let current = fs::read_link(service.join(CURRENT));
    current.is_ok_and(|target| Some(target.as_os_str()) == version.file_name())
}

/// Whether the directory `version` is an archive version: a regular file [`ARCHIVE_MANIFEST`] at
/// its top and nothing named [`TARGET_FILE`]. Anything else is not one; a top that cannot be
/// looked at is an error.
fn is_archive(version: &Path) -> io::Result<bool> {
    let found = |name: &str| match fs::symlink_metadata(version.join(name)) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    let manifest = found(ARCHIVE_MANIFEST)?.is_some_and(|meta| meta.is_file());
    Ok(manifest && found(TARGET_FILE)?.is_none())
}

/// Sets `dir` to `mode` when it is a directory of another mode, and answers whether it is a
/// directory (to look into). A failure is kept in `done`.
fn give(dir: &Path, mode: u32, done: &mut Set) -> bool {
    set_when(dir, done, |found| (found != mode).then_some(mode))
}

/// Sets `dir` to [`CLOSED`] when it is a directory its group or others can reach, and answers
/// whether it is a directory. A failure is kept in `done`.
fn close(dir: &Path, done: &mut Set) -> bool {
    set_when(dir, done, |found| (found & 0o077 != 0).then_some(CLOSED))
}

/// Sets `dir` to the mode `wanted` answers for its present mode, if any, when it is a directory,
/// and answers whether it is one. A regular file at `dir` is closed to its owner instead (its
/// group's and others' bits cleared, and any set-id or sticky bit with them, when it has any of
/// the first two), and is not a directory; anything else is left as it is. A directory that is
/// not there is not one; a failure to look or to set is kept in `done`, and is not one.
fn set_when(dir: &Path, done: &mut Set, wanted: impl Fn(u32) -> Option<u32>) -> bool {
    let looked = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return false,
        looked => looked,
    };
    let set = looked.and_then(|meta| {
        let found = meta.permissions().mode() & 0o7777;
        let mode = match (meta.is_dir(), meta.is_file()) {
            (true, _) => wanted(found),
            (false, true) => (found & 0o077 != 0).then_some(found & 0o700),
            (false, false) => None,
        };
        if let Some(mode) = mode {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode))?;
            done.set.push((dir.to_path_buf(), mode));
        }
        Ok(meta.is_dir())
    });
    set.unwrap_or_else(|e| {
        done.failed.push((dir.to_path_buf(), e));
        false
    })
}

/// The entries of `dir`, in name order (each looked at by the caller); one that cannot be listed
/// is kept in `done` and has none.
fn subdirs(dir: &Path, done: &mut Set) -> Vec<PathBuf> {
    let listed = fs::read_dir(dir).and_then(|entries| {
        entries
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()
    });
    match listed {
        Ok(mut paths) => {
            paths.sort();
            paths
        }
        Err(e) => {
            done.failed.push((dir.to_path_buf(), e));
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "package_dirs_tests.rs"]
mod tests;
