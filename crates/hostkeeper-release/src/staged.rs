//! What the daemon checks of a manifest against the tree its installer staged (packages.md
//! section 4.4, "The hook program", and packages-lifecycle.md section 6.1): the hook program is
//! a regular file of the tree, and every `place` source a regular file (a directory too, for a
//! file entry), each reached through no link. So no program and no file outside the attested
//! bytes is ever named.
//!
//! The tree is read by the caller: [`check`] asks it what each path is, through the function it
//! is given, which on a host reads the entry without following a link (`symlink_metadata`). Every
//! path asked is relative to the version's `tree/`, and each of its leading parts is asked before
//! it, so a link part way is found before anything below it is asked.

use hostkeeper_proto::hostkeeper::release::v1::Manifest;

use crate::manifest::PLACE_UNIT;

/// What a path of the staged tree is, read without following a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeEntry {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link, whatever it points at.
    Link,
    /// Anything else: a device, a fifo, a socket.
    Other,
    /// Nothing is there.
    Missing,
}

/// What is wrong with a path of the tree, at the part of it named.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeProblem {
    /// The part is not in the tree.
    #[error("is not in the tree: {0} is missing")]
    Missing(String),
    /// The part is a link.
    #[error("passes through the link {0}")]
    Link(String),
    /// A leading part is not a directory.
    #[error("passes through {0}, which is not a directory")]
    NotADirectory(String),
    /// The path is not of a kind it may be.
    #[error("is not a regular file{}", if *.0 { " or a directory" } else { "" })]
    Kind(bool),
}

/// Why a manifest is refused against the staged tree: `Failed`, `release`, `manifest_invalid`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StagedError {
    /// The hook program is not a regular file of the tree reached through no link.
    #[error("hook.command names the program {program:?}, which {why}")]
    HookProgram {
        /// The program.
        program: String,
        /// What is wrong with it.
        why: TreeProblem,
    },
    /// A `place` source is not a file of the tree reached through no link.
    #[error("the place source {entry:?} {why}")]
    PlaceSource {
        /// The source.
        entry: String,
        /// What is wrong with it.
        why: TreeProblem,
    },
}

/// Checks the hook program and every `place` source of `manifest`, one already passed by
/// [`crate::manifest::check`] and, for a program, [`crate::manifest::check_hook_of`] for an entry that
/// is not native (so each is a relative path of plain names), against the tree
/// `entry` reads. A unit source and the program must be regular files; a file source may also be
/// a directory, as a `[[files]]` source today.
pub fn check(
    manifest: &Manifest,
    mut entry: impl FnMut(&str) -> TreeEntry,
) -> Result<(), StagedError> {
    let program = manifest.hook.as_ref().and_then(|hook| hook.command.first());
    if let Some(program) = program {
        walk(program, false, &mut entry).map_err(|why| StagedError::HookProgram {
            program: program.clone(),
            why,
        })?;
    }
    for place in &manifest.place {
        let directory = place.kind != PLACE_UNIT;
        walk(&place.source, directory, &mut entry).map_err(|why| StagedError::PlaceSource {
            entry: place.source.clone(),
            why,
        })?;
    }
    Ok(())
}

/// Asks each leading part of `path`, then `path` itself: every leading part a directory, and
/// `path` a regular file, or a directory when `directory` allows one.
fn walk(
    path: &str,
    directory: bool,
    entry: &mut impl FnMut(&str) -> TreeEntry,
) -> Result<(), TreeProblem> {
    let reached = |part: &str, found: TreeEntry| match found {
        TreeEntry::Missing => Err(TreeProblem::Missing(part.to_owned())),
        TreeEntry::Link => Err(TreeProblem::Link(part.to_owned())),
        other => Ok(other),
    };
    for (at, _) in path.match_indices('/') {
        let part = &path[..at];
        if reached(part, entry(part))? != TreeEntry::Directory {
            return Err(TreeProblem::NotADirectory(part.to_owned()));
        }
    }
    match (reached(path, entry(path))?, directory) {
        (TreeEntry::File, _) | (TreeEntry::Directory, true) => Ok(()),
        _ => Err(TreeProblem::Kind(directory)),
    }
}
