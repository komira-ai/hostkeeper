//! The start's closing of the directories that keep packages: what hostkeeper keeps of a package
//! (the package file itself, which holds every file it installs whatever mode that file is given
//! on the host, and the unpacked copy beside it) is read by root alone, and an earlier release
//! left it open to every user (the packages directory and each service directory 0755, each
//! version directory 0555, the apt work directories and the private repositories' mirror 0755).
//!
//! At each start, before anything is staged, the daemon sets mode 0700 on each of these
//! directories that its group or others can reach: the packages directory, each service directory
//! in it, each version directory in those, and the apt work directories' parent and the mirrors'
//! parent beside it. Nothing below a version directory is changed: the modes there are part of
//! the tree digest its seal holds, and a closed parent keeps every other user out of all of it.
//! The mode of a version directory itself is not part of the digest, so every seal still holds.
//! A directory already closed, a link (`current`, `previous`), anything that is not a directory
//! and a directory that is not there are left as they are.
//!
//! What it closed is said in one log line, and what it could not close is the alert
//! [`ALERT_START`]; the start goes on either way.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::boot::ALERT_START;
use crate::executor::Ctx;

/// The mode the start gives a directory that keeps packages.
pub const CLOSED: u32 = 0o700;

/// What [`close_dirs`] did.
#[derive(Debug, Default)]
pub struct Closed {
    /// The directories set to [`CLOSED`], outermost first.
    pub closed: Vec<PathBuf>,
    /// The directories that could not be looked at, listed or set, and why.
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// Closes the directories of the daemon's packages directory and of the apt work directories'
/// and mirrors' parents ([the module](self)), says so, and answers what it closed.
pub fn close(ctx: &Ctx) -> Vec<PathBuf> {
    let dirs = &ctx.repository_ports.dirs;
    let done = close_dirs(&ctx.dirs.packages, &[&dirs.work, &dirs.mirror]);
    if !done.closed.is_empty() {
        let paths: Vec<String> = done
            .closed
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        ctx.log(&format!(
            "level=info closed to every user but root the package directories an earlier \
             release left open: {}",
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
                "a package directory was not closed to other users: {}",
                failed.join("; ")
            ),
        );
    }
    done.closed
}

/// Closes `packages`, each directory in it and each directory in those, then each of `beside`
/// ([the module](self)), each one before what is in it.
pub fn close_dirs(packages: &Path, beside: &[&Path]) -> Closed {
    let mut done = Closed::default();
    if close_one(packages, &mut done) {
        for service in subdirs(packages, &mut done) {
            if close_one(&service, &mut done) {
                for version in subdirs(&service, &mut done) {
                    close_one(&version, &mut done);
                }
            }
        }
    }
    for dir in beside {
        close_one(dir, &mut done);
    }
    done
}

/// Sets `dir` to [`CLOSED`] when it is a directory its group or others can reach, and answers
/// whether it is a directory (to look into). A failure is kept in `done`.
fn close_one(dir: &Path, done: &mut Closed) -> bool {
    let looked = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return false,
        looked => looked,
    };
    let closed = looked.and_then(|meta| {
        let open = meta.is_dir() && meta.permissions().mode() & 0o077 != 0;
        if open {
            fs::set_permissions(dir, fs::Permissions::from_mode(CLOSED))?;
            done.closed.push(dir.to_path_buf());
        }
        Ok(meta.is_dir())
    });
    closed.unwrap_or_else(|e| {
        done.failed.push((dir.to_path_buf(), e));
        false
    })
}

/// The entries of `dir`, in name order (each looked at by [`close_one`]); one that cannot be
/// listed is kept in `done` and has none.
fn subdirs(dir: &Path, done: &mut Closed) -> Vec<PathBuf> {
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
