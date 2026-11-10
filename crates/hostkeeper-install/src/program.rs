//! The hook program of a native package's entry (packages-native.md section 10.2): the manifest's
//! `hook.command` names it by the absolute path the package installs it at (`/usr/bin/tierstore`
//! in `["/usr/bin/tierstore", "update-hook"]`), and the daemon runs that file's copy in the
//! version's extracted tree, `tree/usr/bin/tierstore`, which the seal covers.
//!
//! [`resolve`] finds the file the path names in a tree as the installed system would: each link
//! met on the way is followed inside the tree, an absolute target from the tree's root and a
//! relative one from the link's directory, and `..` never climbs above the root, as in a
//! `chroot`. So `/usr/bin/tierstore`, a link to `/opt/tierstore/bin/tierstore`, resolves to
//! `opt/tierstore/bin/tierstore`. [`check`] also asks that the file be a regular file with an
//! execute bit: the one rule on a native hook, checked at `stage`, against the bytes the
//! package installs.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// The most links followed while one path is resolved, as Linux's `MAXSYMLINKS`.
pub const MAX_LINKS: usize = 40;

/// Why a hook program is not a file the package installs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProgramError {
    #[error("{0} is not an absolute path")]
    NotAbsolute(String),
    #[error("{0} is not a file the package installs")]
    Missing(String),
    #[error("{0} passes through more than {MAX_LINKS} links in the package")]
    Loop(String),
    #[error("{0} is not a regular file in the package")]
    NotFile(String),
    #[error("{0} is not executable in the package")]
    NotExecutable(String),
}

/// The path, relative to `tree` and holding no link, of the file the absolute `program` names
/// in it; `program` itself when no link is on its way.
pub fn resolve(tree: &Path, program: &str) -> Result<PathBuf, ProgramError> {
    if !program.starts_with('/') {
        return Err(ProgramError::NotAbsolute(program.to_owned()));
    }
    let missing = || ProgramError::Missing(program.to_owned());
    // A path still to split into components; the components still to walk, the next one last;
    // and the real directories walked so far.
    let mut todo: Vec<PathBuf> = vec![PathBuf::from(program)];
    let mut pending: Vec<std::ffi::OsString> = Vec::new();
    let mut at = PathBuf::new();
    let mut links = 0;
    loop {
        if let Some(path) = todo.pop() {
            let mut parts: Vec<std::ffi::OsString> = Vec::new();
            for part in path.components() {
                match part {
                    Component::RootDir => {
                        at = PathBuf::new();
                        pending.clear();
                    }
                    Component::Normal(name) => parts.push(name.to_owned()),
                    Component::ParentDir => parts.push("..".into()),
                    Component::CurDir | Component::Prefix(_) => {}
                }
            }
            parts.reverse();
            pending.extend(parts);
        }
        let Some(name) = pending.pop() else {
            return Ok(at);
        };
        if name == ".." {
            at.pop();
            continue;
        }
        let next = at.join(&name);
        let meta = fs::symlink_metadata(tree.join(&next)).map_err(|_| missing())?;
        if !meta.file_type().is_symlink() {
            if !pending.is_empty() && !meta.is_dir() {
                return Err(missing());
            }
            at = next;
            continue;
        }
        links += 1;
        if links > MAX_LINKS {
            return Err(ProgramError::Loop(program.to_owned()));
        }
        let target = fs::read_link(tree.join(&next)).map_err(|_| missing())?;
        // The rest of the path is walked after the link's target.
        pending.reverse();
        let rest: PathBuf = pending.drain(..).collect();
        // An absolute target starts again at the tree's root (its `RootDir` resets `at`); a
        // relative one from the link's directory, `at` as it is.
        todo.push(target.join(rest));
    }
}

/// [`resolve`], then the rule of a hook program: a regular file with an execute bit. Answers the
/// path relative to `tree`.
pub fn check(tree: &Path, program: &str) -> Result<PathBuf, ProgramError> {
    let rel = resolve(tree, program)?;
    let meta = fs::symlink_metadata(tree.join(&rel))
        .map_err(|_| ProgramError::Missing(program.to_owned()))?;
    if !meta.is_file() {
        return Err(ProgramError::NotFile(program.to_owned()));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(ProgramError::NotExecutable(program.to_owned()));
    }
    Ok(rel)
}

#[cfg(test)]
#[path = "program_tests.rs"]
mod tests;
