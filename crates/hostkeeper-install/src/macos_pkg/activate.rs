//! `activate` (packages-repositories.md section 4.7): makes a staged version the installed one,
//! and is also how a rollback goes back (the `activate` of the kept previous version, whose
//! `.pkg` the core re-hashed before the step; its signature is not checked again). It fetches
//! nothing, and a crash inside it resumes by running it again: `installer` installs the same
//! bytes again, and the deletions and the enablement find nothing left to do.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::Label;

use super::payload::labels_of;
use super::record::PkgRecord;
use super::{ARTIFACT, MacosPkg, Order, TREE, arg, io, receipt};
use crate::host::{self, GID_ADMIN, Kind, Meta, file_digest, link_target, parents};
use crate::outcome::{Failure, Reason};
use crate::tools::{Tool, last_line, may_be_cut};

/// What `installer` prints when macOS's installer is busy with another install, in any case:
/// the order is deferred, `package_manager_busy`.
pub const BUSY: [&str; 2] = ["already in progress", "installer is busy"];

/// What an `activate` that succeeded found and corrected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activated {
    /// `Some` when what was installed was not what the record said (`drift_repaired`): the
    /// version the receipt showed, `None` when there was no receipt.
    pub drift: Option<Option<String>>,
}

impl MacosPkg<'_> {
    /// Activates the version staged at `dir`. `from` is the version directory of the version
    /// the record names, when it is on the host and is not `dir`: the version this one replaces,
    /// or rolls back from.
    pub fn activate(
        &self,
        order: &Order,
        dir: &Path,
        from: Option<&Path>,
    ) -> Result<Activated, Failure> {
        let deadline = order.bounds.activate;
        let ids = *self.host.ids();
        let tree = host::walk(&dir.join(TREE), &ids).map_err(|e| io("the staged tree", e))?;
        let old_tree = match from {
            Some(old) => host::walk(&old.join(TREE), &ids).map_err(|e| io("the kept tree", e))?,
            None => BTreeMap::new(),
        };
        let _held = self.lock.hold().map_err(|e| io("the installer lock", e))?;

        let found = self.receipt(order, deadline)?;
        let record = self
            .records
            .load(&order.service)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))?;
        let drift = self.drift(&record, found.as_ref(), from, &old_tree)?;
        let replaced: BTreeSet<String> = match (&drift, &found) {
            (Some(_), Some(receipt)) => self.listed(order, receipt, deadline)?,
            _ => files_of(&old_tree),
        };

        self.check_parents(&tree)?;
        self.check_owners(order, &tree, deadline)?;
        let mut created = record
            .as_ref()
            .map(|r| r.created_dirs.clone())
            .unwrap_or_default();
        for (path, meta) in &tree {
            let absent = self.host.meta(path).map_err(|e| io(path, e))?.is_none();
            if meta.kind == Kind::Dir && absent {
                created.insert(path.clone());
            }
        }
        let mut pending = PkgRecord::new(&order.identifier, order.version.as_str());
        pending.created_dirs = created;
        pending.pending = true;
        self.save(order, &pending)?;

        self.install(dir, deadline)?;
        self.prove(order, dir, &tree, deadline)?;

        for path in replaced.iter().filter(|path| !tree.contains_key(*path)) {
            if !self.host.through_link(path).map_err(|e| io(path, e))? {
                self.host.delete_file(path).map_err(|e| io(path, e))?;
            }
        }
        let mut done = pending;
        for dir in done.created_dirs.clone().iter().rev() {
            let gone = !tree.contains_key(dir)
                && (self.host.delete_empty_dir(dir).map_err(|e| io(dir, e))?
                    || self.host.meta(dir).map_err(|e| io(dir, e))?.is_none());
            if gone {
                done.created_dirs.remove(dir);
            }
        }
        let dropped: Vec<Label> = labels_of(&old_tree)
            .into_iter()
            .filter(|label| !order.run.contains(label))
            .collect();
        for (verb, label) in order
            .run
            .iter()
            .map(|l| ("enable", l))
            .chain(dropped.iter().map(|l| ("disable", l)))
        {
            self.launchctl(verb, label, deadline)?;
        }
        done.pending = false;
        self.save(order, &done)?;
        Ok(Activated { drift })
    }

    /// The drift the host shows against the record: a receipt at another version, a receipt with
    /// no record, a record with no receipt, or, at the recorded version, an installed file that
    /// differs from the recorded version's tree. A pending record is an `activate` cut short,
    /// not drift.
    fn drift(
        &self,
        record: &Option<PkgRecord>,
        found: Option<&receipt::Receipt>,
        from: Option<&Path>,
        old_tree: &BTreeMap<String, Meta>,
    ) -> Result<Option<Option<String>>, Failure> {
        let version = |r: Option<&receipt::Receipt>| r.map(|r| r.version.clone());
        match (record, found) {
            (Some(r), _) if r.pending => Ok(None),
            (None, None) => Ok(None),
            (None, Some(_)) | (Some(_), None) => Ok(Some(version(found))),
            (Some(r), Some(receipt)) if receipt.version != r.version => Ok(Some(version(found))),
            (Some(_), Some(_)) => match from {
                Some(old) => match self.differs(&old.join(TREE), old_tree)? {
                    true => Ok(Some(version(found))),
                    false => Ok(None),
                },
                None => Ok(None),
            },
        }
    }

    /// Whether an installed file or link differs from its copy in `tree_dir`.
    fn differs(&self, tree_dir: &Path, tree: &BTreeMap<String, Meta>) -> Result<bool, Failure> {
        for (path, meta) in tree.iter().filter(|(_, m)| m.kind != Kind::Dir) {
            if !same(&tree_dir.join(path.trim_start_matches('/')), &self.host.real(path), meta.kind)
                .map_err(|e| io(path, e))?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The logical paths of the files the receipt lists.
    fn listed(
        &self,
        order: &Order,
        found: &receipt::Receipt,
        deadline: Duration,
    ) -> Result<BTreeSet<String>, Failure> {
        let out = self.run(Tool::Pkgutil, &receipt::files_args(&order.identifier), deadline)?;
        if out.code != Some(0) {
            return Err(Failure::new(
                Reason::IoError,
                format!("pkgutil --files exited {:?}", out.code),
            ));
        }
        let files = receipt::parse_files(&out.stdout, may_be_cut(&out.stdout))
            .map_err(|e| Failure::new(Reason::IoError, format!("pkgutil --files {e}")))?;
        Ok(files.iter().map(|f| found.path_of(f)).collect())
    }

    /// Each parent directory of a payload path that is on the host is a directory of root's
    /// that no other account can write but the group `admin` (`unsafe_directory`).
    fn check_parents(&self, tree: &BTreeMap<String, Meta>) -> Result<(), Failure> {
        let dirs: BTreeSet<String> = tree.keys().flat_map(|path| parents(path)).collect();
        for dir in dirs {
            let Some(meta) = self.host.meta(&dir).map_err(|e| io(&dir, e))? else {
                continue;
            };
            let group_writes = meta.mode & 0o020 != 0 && meta.gid != GID_ADMIN;
            if meta.kind != Kind::Dir || meta.uid != 0 || meta.mode & 0o002 != 0 || group_writes {
                return Err(Failure::new(
                    Reason::UnsafeDirectory,
                    format!("{dir} is {meta:?}: not a directory only root (and admin) can write"),
                ));
            }
        }
        Ok(())
    }

    /// No payload file already on the host is listed by the receipt of another identifier
    /// (`package_conflict`); one no receipt lists is replaced, and is the package's from then.
    fn check_owners(
        &self,
        order: &Order,
        tree: &BTreeMap<String, Meta>,
        deadline: Duration,
    ) -> Result<(), Failure> {
        for path in files_of(tree) {
            if self.host.meta(&path).map_err(|e| io(&path, e))?.is_none() {
                continue;
            }
            let other = self
                .owners(&path, deadline)?
                .into_iter()
                .find(|owner| *owner != order.identifier);
            if let Some(other) = other {
                return Err(Failure::new(
                    Reason::PackageConflict,
                    format!("{path} is a file of the package {other}"),
                ));
            }
        }
        Ok(())
    }

    /// `installer -allowUntrusted -pkg <dir>/artifact -target /`: hostkeeper made the trust
    /// decision at `stage`, and the bytes are the sealed ones.
    fn install(&self, dir: &Path, deadline: Duration) -> Result<(), Failure> {
        let args = [
            "-allowUntrusted".into(),
            "-pkg".into(),
            arg(&dir.join(ARTIFACT))?,
            "-target".into(),
            "/".into(),
        ];
        let out = self.tools.run(Tool::Installer, &args, deadline).map_err(|e| {
            Failure::new(Reason::InstallerError, format!("{}: {e}", Tool::Installer.path()))
        })?;
        if out.code == Some(0) {
            return Ok(());
        }
        let said = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            out.stderr_text()
        )
        .to_lowercase();
        let reason = match BUSY.iter().any(|busy| said.contains(busy)) {
            true => Reason::PackageManagerBusy,
            false => Reason::InstallerError,
        };
        Err(Failure::new(
            reason,
            format!("installer exited {:?}: {}", out.code, last_line(&out)),
        ))
    }

    /// The receipt shows the order's version, and every payload file and link at its install
    /// path is its copy in `tree/` (`installer_error`).
    fn prove(
        &self,
        order: &Order,
        dir: &Path,
        tree: &BTreeMap<String, Meta>,
        deadline: Duration,
    ) -> Result<(), Failure> {
        let shown = self.receipt(order, deadline)?.map(|r| r.version);
        if shown.as_deref() != Some(order.version.as_str()) {
            return Err(Failure::new(
                Reason::InstallerError,
                format!("pkgutil shows {shown:?} after installer, not {}", order.version),
            ));
        }
        if self.differs(&dir.join(TREE), tree)? {
            return Err(Failure::new(
                Reason::InstallerError,
                "an installed file differs from its copy in tree/",
            ));
        }
        Ok(())
    }

    fn launchctl(&self, verb: &str, label: &Label, deadline: Duration) -> Result<(), Failure> {
        let args = [verb.to_owned(), format!("system/{}", label.as_str())];
        let out = self.run(Tool::Launchctl, &args, deadline)?;
        match out.code {
            Some(0) => Ok(()),
            code => Err(Failure::new(
                Reason::InstallerError,
                format!("launchctl {verb} system/{label} exited {code:?}: {}", out.stderr_text().trim()),
            )),
        }
    }

    fn save(&self, order: &Order, record: &PkgRecord) -> Result<(), Failure> {
        self.records
            .save(&order.service, record)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))
    }
}

/// The logical paths of a tree's files and links.
pub fn files_of(tree: &BTreeMap<String, Meta>) -> BTreeSet<String> {
    tree.iter()
        .filter(|(_, meta)| meta.kind != Kind::Dir)
        .map(|(path, _)| path.clone())
        .collect()
}

/// Whether the installed entry at `installed` is the sealed `copy`: the same bytes for a file,
/// the same target for a link.
pub fn same(copy: &Path, installed: &Path, kind: Kind) -> std::io::Result<bool> {
    match kind {
        Kind::Link => Ok(link_target(installed)? == link_target(copy)?),
        _ => {
            let got = file_digest(installed)?;
            Ok(got.is_some() && got == file_digest(copy)?)
        }
    }
}
