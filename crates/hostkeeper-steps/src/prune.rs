//! Pruning a package directory to two versions (docs/design/hostkeeper-design.md, section 4.7).
//!
//! A host keeps two versions of a service: the one `current` names and the one `previous`
//! names, so that a rollback to `previous` is a local switch with no fetch. After a confirmed
//! switch every other version directory of the service is deleted, with its seal, except the
//! versions the caller protects ([`Protected`]).
//!
//! The rule is a pure function, [`prune_plan`], of what the directory holds ([`PruneInputs`]);
//! [`Layout::prune`] reads the directory, plans, and deletes. It takes no lock: the caller holds
//! the service's lock for the whole call.
//!
//! # Deleting a version
//!
//! Each version is deleted in three steps, each durable before the next:
//!
//! 1. `<version>/` is opened to its owner (0700) and renamed to `.stage-prune-<16 hex>` beside
//!    it, so the version is gone in one step: no reader ever sees a half-deleted `<version>/`.
//!    The version is read-only (0555) as an unpack leaves it, and macOS refuses to rename a
//!    directory its caller cannot write, even to a new name in the same parent (it rewrites the
//!    directory's `..` entry); Linux makes that check only when the parent changes. The mode of
//!    the top directory is not part of the tree digest;
//! 2. `<version>.seal` is removed, so no seal outlives its tree;
//! 3. the renamed tree is removed bottom-up. Each directory is opened to its owner first, so a
//!    read-only tree (0555) can be emptied, and no link is followed: a link inside the tree is
//!    removed itself, never what it points at.
//!
//! A crash leaves only a `.stage-*` directory, a seal with no tree beside it, or (a crash between
//! the opening and the rename) a surplus version still whole, its top directory 0700. The unpack's
//! own orphan pass removes the first two (`hostkeeper_unpack::remove_orphans`), and so does the
//! next prune, which deletes all three.
//! Every step is idempotent.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::selfupdate::Layout;

/// The prefix of the name a version directory is renamed to before it is removed. It begins
/// with `.stage-`, which no version can (a version does not begin with `.`), so the unpack's
/// orphan pass removes what a crashed prune leaves.
pub const PRUNE_PREFIX: &str = ".stage-prune-";

/// The versions a prune keeps whatever `current` and `previous` name. The caller builds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Protected {
    /// The release an unfinished step record replaces (`updater.previous_release`): an update
    /// that is not over may still return to it.
    pub previous_releases: BTreeSet<String>,
    /// The target of a switch under way: the target of an unfinished step record or force
    /// record, or the version of a `placed.json` entry still `pending` for the service.
    pub switch_targets: BTreeSet<String>,
    /// A version staged and not yet switched to that the caller keeps.
    pub staged: BTreeSet<String>,
}

impl Protected {
    /// Whether `version` is protected.
    pub fn contains(&self, version: &str) -> bool {
        [&self.previous_releases, &self.switch_targets, &self.staged]
            .iter()
            .any(|set| set.contains(version))
    }
}

/// What a package directory holds, as a prune reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneInputs {
    /// The version directories: real directories (not links) named as a version.
    pub versions: BTreeSet<String>,
    /// The versions with a seal `<version>.seal`, with or without a directory.
    pub seals: BTreeSet<String>,
    /// The directories a crashed prune left (`.stage-prune-*`).
    pub leftovers: BTreeSet<String>,
    /// What the link `current` holds when it is a link to one plain name; `None` when it is
    /// missing or holds anything else.
    pub current: Option<String>,
    /// What the link `previous` holds, the same way.
    pub previous: Option<String>,
    pub protected: Protected,
}

/// What a prune deletes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrunePlan {
    /// The versions kept: every version with a directory or a seal that is not deleted.
    pub keep: BTreeSet<String>,
    /// The versions deleted, each with its directory and its seal, whichever is there.
    pub delete: BTreeSet<String>,
    /// The leftover directories of a crashed prune, removed.
    pub leftovers: BTreeSet<String>,
    /// `current` or `previous` is missing or does not name a version directory: nothing can be
    /// proved surplus, and nothing is deleted.
    pub unresolved: bool,
}

/// What a prune did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// As [`PrunePlan::unresolved`]: nothing was deleted.
    pub unresolved: bool,
    /// The versions kept.
    pub kept: BTreeSet<String>,
    /// The versions deleted, directory and seal.
    pub deleted: BTreeSet<String>,
    /// What could not be removed, a version or a leftover directory, with the error. The next
    /// prune tries it again.
    pub failed: BTreeMap<String, String>,
}

/// A point of a prune's deletion, for a test that stops it there ([`Layout::prune_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneStep<'a> {
    /// `<version>/` was renamed out of the way.
    Moved { version: &'a str },
    /// `<version>.seal` was removed.
    SealRemoved { version: &'a str },
    /// The entry at `path` of a renamed tree is about to be removed.
    Removing { path: &'a Path },
}

/// The rule of design section 4.7, in order: keep the versions `current` and `previous` name;
/// keep every protected version; delete every other version, its directory and its seal,
/// whichever of the two is there (a seal with no directory and a directory with no seal are
/// deleted alike); remove the leftovers of a crashed prune. When `current` or `previous` is
/// missing or names no version directory, nothing is deleted and the plan is `unresolved`.
pub fn prune_plan(inputs: &PruneInputs) -> PrunePlan {
    let present: BTreeSet<String> = inputs.versions.union(&inputs.seals).cloned().collect();
    let resolved = |link: &Option<String>| {
        link.as_ref()
            .filter(|version| inputs.versions.contains(*version))
            .cloned()
    };
    let (Some(current), Some(previous)) = (resolved(&inputs.current), resolved(&inputs.previous))
    else {
        return PrunePlan {
            keep: present,
            unresolved: true,
            ..PrunePlan::default()
        };
    };
    let (keep, delete): (BTreeSet<String>, BTreeSet<String>) =
        present.into_iter().partition(|version| {
            (*version == current) | (*version == previous) | inputs.protected.contains(version)
        });
    PrunePlan {
        keep,
        delete,
        leftovers: inputs.leftovers.clone(),
        unresolved: false,
    }
}

impl Layout {
    /// Deletes every version of this package directory but those `current` and `previous` name
    /// and those `protect` names ([`prune_plan`]). A version that cannot be removed is in the
    /// report's `failed`, and the others are still deleted; an error is only a directory that
    /// cannot be listed, with nothing deleted. For a package directory only
    /// ([`io::ErrorKind::InvalidInput`] otherwise). The caller holds the service's lock.
    pub fn prune(&self, protect: &Protected) -> io::Result<PruneReport> {
        self.prune_with(protect, &mut |_| Ok(()))
    }

    /// [`Layout::prune`], calling `at` at each point of the deletion: an error it returns stops
    /// the deletion of that version there, as a crash or a failed removal would, and is
    /// reported in `failed`.
    pub fn prune_with(
        &self,
        protect: &Protected,
        at: &mut dyn FnMut(PruneStep<'_>) -> io::Result<()>,
    ) -> io::Result<PruneReport> {
        let plan = prune_plan(&self.prune_inputs(protect)?);
        let mut report = PruneReport {
            unresolved: plan.unresolved,
            kept: plan.keep,
            ..PruneReport::default()
        };
        for name in plan.leftovers {
            if let Err(error) = remove_tree(&self.dir().join(&name), at) {
                report.failed.insert(name, error.to_string());
            }
        }
        for version in plan.delete {
            match self.delete_version_with(&version, at) {
                Ok(()) => {
                    report.deleted.insert(version);
                }
                Err(error) => {
                    report.failed.insert(version, error.to_string());
                }
            }
        }
        Ok(report)
    }

    /// What this package directory holds, with `protect`, as [`prune_plan`] reads it. Entries
    /// of any other name (`current.tmp`, an unpack's `.stage-*` or `.<version>.seal.tmp`, a link
    /// or a file named as a version) are not read and never touched.
    pub fn prune_inputs(&self, protect: &Protected) -> io::Result<PruneInputs> {
        self.expect_package()?;
        let mut inputs = PruneInputs {
            current: link_name(&self.current_path()),
            previous: link_name(&self.previous_path()),
            protected: protect.clone(),
            ..PruneInputs::default()
        };
        for entry in fs::read_dir(self.dir())? {
            let entry = entry?;
            let is_dir = entry.file_type()?.is_dir();
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with(PRUNE_PREFIX) {
                if is_dir {
                    inputs.leftovers.insert(name);
                }
                continue;
            }
            if let Some(version) = name.strip_suffix(".seal") {
                if !is_dir & self.check_name(version).is_ok() {
                    inputs.seals.insert(version.to_owned());
                }
                continue;
            }
            if is_dir & self.check_name(&name).is_ok() {
                inputs.versions.insert(name);
            }
        }
        Ok(inputs)
    }

    /// Deletes version `version` of this package directory, whoever names it: its directory
    /// renamed out of the way, its seal removed, the renamed tree removed, each durable before
    /// the next ([`Layout::prune`] does the same for every surplus version). Idempotent: a version
    /// already gone, or gone but its seal, is finished. The caller holds the service's lock and
    /// has made sure no link names `version`. For a package directory only
    /// ([`io::ErrorKind::InvalidInput`] otherwise); a `version` that is not a name of one is
    /// refused before anything is touched.
    pub fn delete_version(&self, version: &str) -> io::Result<()> {
        self.expect_package()?;
        self.check_name(version)?;
        self.delete_version_with(version, &mut |_| Ok(()))
    }

    /// The three steps of deleting `version`: its directory renamed out of the way, its seal
    /// removed, the renamed tree removed; each durable before the next.
    fn delete_version_with(
        &self,
        version: &str,
        at: &mut dyn FnMut(PruneStep<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let dir = self.version_path(version);
        let moved = self.dir().join(prune_name(version));
        if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            remove_tree(&moved, at)?;
            fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
            fs::rename(dir, &moved)?;
            self.sync()?;
            at(PruneStep::Moved { version })?;
        }
        match fs::remove_file(self.seal_path(version)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        self.sync()?;
        at(PruneStep::SealRemoved { version })?;
        remove_tree(&moved, at)?;
        self.sync()
    }

    fn sync(&self) -> io::Result<()> {
        File::open(self.dir())?.sync_all()
    }
}

/// The name `version`'s directory is renamed to: [`PRUNE_PREFIX`] and 16 hex digits of the
/// SHA-256 of the version, short enough for any version a name can hold.
fn prune_name(version: &str) -> String {
    let digest = Sha256::digest(version.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{PRUNE_PREFIX}{hex}")
}

/// What the link at `path` holds when it is a link to one plain name, `None` otherwise: missing,
/// not a link, or a target with a directory part.
fn link_name(path: &Path) -> Option<String> {
    let target = fs::read_link(path).ok()?;
    let mut parts = target.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) => name.to_str().map(str::to_owned),
        _ => None,
    }
}

/// Removes `path` and everything below it, never following a link; nothing there is fine. A
/// directory is opened to its owner before it is listed, so a read-only tree can be emptied.
fn remove_tree(path: &Path, at: &mut dyn FnMut(PruneStep<'_>) -> io::Result<()>) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        meta => meta?,
    };
    if meta.is_dir() {
        fs::set_permissions(path, Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            remove_tree(&entry?.path(), at)?;
        }
        at(PruneStep::Removing { path })?;
        return fs::remove_dir(path);
    }
    at(PruneStep::Removing { path })?;
    fs::remove_file(path)
}
