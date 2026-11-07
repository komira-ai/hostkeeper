//! The files of today behind the [`Backend`]: each part of a commit replaces its file whole, as
//! its store always did (`desired.json` by [`crate::desired`], the ops file and its halts file by
//! [`crate::ops_file`], the verdicts by [`crate::verdict_file`]), and the audit lines of a commit
//! are appended to the audit file ([`crate::audit_file`]) before the ops file is written, so a
//! commit whose line cannot be appended writes nothing of the ops state.
//!
//! A part with no file is kept nowhere: a commit of it is taken as done, and a load reads it as
//! empty. A commit of the desired state and another part is made [`in_turn`]: two files cannot be
//! replaced at once.
//!
//! An audit line already in the file (a commit tried again after some of its lines landed) is not
//! appended again, so the file never holds a number twice.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_oidc::{Clock, SpentJtis};

use super::{Backend, BackendError, Commit, Loaded, in_turn};
use crate::audit_file::{self, Appender, AuditLine, AuditLog, Logged, Query, Rotation};
use crate::desired::{self, StoreError, SyncDir, io_at};
use crate::jti_file::{JtiError, JtiFile};
use crate::ops::{OpsError, OpsState};
use crate::verdict_file::{self, VerdictsError};

/// What puts an operations state into the ops file and its halts file.
pub(crate) type OpsWriter = Box<dyn Fn(&OpsState) -> Result<(), OpsError> + Send + Sync>;

/// The desired-state file.
struct DesiredFile {
    path: PathBuf,
    /// The legacy service: the one whose state alone is written as the single state.
    legacy: Option<String>,
    /// Syncs the directory of the file after a rename.
    sync_dir: SyncDir,
}

/// The ops file, its halts file and its audit file.
struct OpsFiles {
    path: PathBuf,
    log: Arc<AuditLog>,
    write: OpsWriter,
    append: Appender,
}

/// The coordinator's state files: those of the parts it was given.
#[derive(Default)]
pub(crate) struct Files {
    desired: Option<DesiredFile>,
    ops: Option<OpsFiles>,
    verdicts: Option<PathBuf>,
    jti: Option<JtiFile>,
}

/// The file at `path`: none if it is not there.
fn read_if_any(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Files {
    /// The files with the desired-state file at `path` added: a file in the single-state form is
    /// the state of `legacy`.
    #[must_use]
    pub(crate) fn with_desired(mut self, path: &Path, legacy: Option<&str>) -> Self {
        self.desired = Some(DesiredFile {
            path: path.to_owned(),
            legacy: legacy.map(str::to_owned),
            sync_dir: desired::sync_dir,
        });
        self
    }

    /// The files with the desired-state file's directory synced by `sync` after a rename, to fail
    /// the sync after the rename.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_sync_dir(mut self, sync: SyncDir) -> Self {
        self.desired = self.desired.map(|file| DesiredFile {
            sync_dir: sync,
            ..file
        });
        self
    }

    /// The files with the ops file at `path` added, its halts file and its audit file beside it,
    /// the audit file rotated by `rotation`.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the audit file exists and cannot be read.
    pub(crate) fn with_ops(mut self, path: &Path, rotation: Rotation) -> Result<Self, OpsError> {
        let log = Arc::new(AuditLog::open(audit_file::path_of(path), rotation)?);
        let appends = Arc::clone(&log);
        let at = path.to_owned();
        self.ops = Some(OpsFiles {
            path: path.to_owned(),
            log,
            write: Box::new(move |state| crate::ops_file::write(&at, state)),
            append: Box::new(move |line| appends.append(line)),
        });
        Ok(self)
    }

    /// The files with `write` putting the ops states into the ops file instead.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_ops_write(mut self, write: OpsWriter) -> Self {
        self.ops = self.ops.map(|ops| OpsFiles { write, ..ops });
        self
    }

    /// The files with `append` appending the audit lines instead of the audit file, which is
    /// still what is read.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_append(mut self, append: Appender) -> Self {
        self.ops = self.ops.map(|ops| OpsFiles { append, ..ops });
        self
    }

    /// The files with the verdicts' file at `path` added.
    #[must_use]
    pub(crate) fn with_verdicts(mut self, path: &Path) -> Self {
        self.verdicts = Some(path.to_owned());
        self
    }

    /// The files with the spent token ids' file at `path` added: created if it is not there, its
    /// ids already past by `clock` dropped and the file rewritten without them ([`JtiFile`]).
    ///
    /// # Errors
    /// [`JtiError`]: the file cannot be read or rewritten, or a line of it is malformed.
    pub(crate) fn with_jti(mut self, path: &Path, clock: Clock) -> Result<Self, JtiError> {
        self.jti = Some(JtiFile::open(path, clock)?);
        Ok(self)
    }

    /// The desired state the file holds; none without one.
    ///
    /// # Errors
    /// [`StoreError`]: the file exists and cannot be read, or holds no desired state.
    pub(crate) fn load_desired(&self) -> Result<desired::Held, StoreError> {
        let Some(file) = &self.desired else {
            return Ok(desired::Held::default());
        };
        match read_if_any(&file.path).map_err(io_at(&file.path))? {
            Some(bytes) => desired::parse_file(&bytes, file.legacy.as_deref()).map_err(|source| {
                StoreError::Invalid {
                    path: file.path.clone(),
                    source,
                }
            }),
            None => Ok(desired::Held::default()),
        }
    }

    /// The operations state the ops file holds, joined with its halts file once a staged halts
    /// file is settled ([`crate::ops_file::joined`]); empty without one.
    ///
    /// # Errors
    /// [`OpsError`]: a file cannot be read or holds what this build refuses.
    pub(crate) fn load_ops(&self) -> Result<OpsState, OpsError> {
        let Some(ops) = &self.ops else {
            return Ok(OpsState::default());
        };
        let state = match read_if_any(&ops.path).map_err(io_at(&ops.path))? {
            Some(bytes) => crate::ops::parse(&ops.path, &bytes)?,
            None => OpsState::default(),
        };
        let (state, _skipped) = crate::ops_file::joined(&ops.path, state)?;
        Ok(state)
    }

    /// The verdicts the file holds; none without one.
    ///
    /// # Errors
    /// [`VerdictsError`]: the file cannot be read, or holds a value this build cannot read.
    pub(crate) fn load_verdicts(&self) -> Result<verdict_file::Held, VerdictsError> {
        let Some(path) = &self.verdicts else {
            return Ok(verdict_file::Held::default());
        };
        match read_if_any(path).map_err(io_at(path))? {
            Some(bytes) => verdict_file::parse(path, &bytes),
            None => Ok(verdict_file::Held::default()),
        }
    }

    /// The spent token ids the file holds, not yet past when it was opened; none without one.
    #[must_use]
    pub(crate) fn load_spent(&self) -> Vec<(String, u64)> {
        self.jti.as_ref().map(JtiFile::load).unwrap_or_default()
    }

    fn write_desired(&self, held: &desired::Held) -> Result<(), BackendError> {
        let Some(file) = &self.desired else {
            return Ok(());
        };
        let text = desired::encode(held, file.legacy.as_deref())
            .map_err(io::Error::from)
            .map_err(io_at(&file.path))?;
        Ok(desired::write_whole_syncing(
            &file.path,
            &text,
            file.sync_dir,
        )?)
    }

    fn write_ops(&self, state: &OpsState, lines: &[AuditLine]) -> Result<(), BackendError> {
        let Some(ops) = &self.ops else {
            return Ok(());
        };
        for line in lines.iter().filter(|line| line.seq > ops.log.last_seq()) {
            (ops.append)(line).map_err(|why| {
                tracing::error!(
                    target: "hostkeeper_coordinator::audit",
                    seq = line.seq,
                    %why,
                    "the audit file could not be appended to: the change is not written"
                );
                OpsError::AuditUnavailable(why)
            })?;
        }
        Ok((ops.write)(state)?)
    }

    fn write_verdicts(&self, held: &verdict_file::Held) -> Result<(), BackendError> {
        let Some(path) = &self.verdicts else {
            return Ok(());
        };
        let text = verdict_file::text_of(path, held).map_err(VerdictsError::Io)?;
        Ok(desired::write_whole(path, &text).map_err(VerdictsError::Io)?)
    }
}

impl Backend for Files {
    fn load(&self) -> Result<Loaded, BackendError> {
        Ok(Loaded {
            desired: self.load_desired()?,
            ops: self.load_ops()?,
            verdicts: self.load_verdicts()?,
            spent: self.load_spent(),
        })
    }

    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError> {
        if let Some(part) = change.desired {
            if change.desired_alone() {
                return self.write_desired(part.1);
            }
            return in_turn(self, self, part, change);
        }
        if let Some((_, after)) = change.ops {
            self.write_ops(after, change.audit)?;
        }
        if let Some((_, after)) = change.verdicts {
            self.write_verdicts(after)?;
        }
        Ok(())
    }

    fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, BackendError> {
        match &self.ops {
            Some(ops) => Ok(ops.log.page(query)?),
            None => Ok(Vec::new()),
        }
    }

    fn audit_each(&self, visit: &mut dyn FnMut(Logged)) -> Result<(), BackendError> {
        if let Some(ops) = &self.ops {
            ops.log.each(&mut |line| visit(line.into_logged()))?;
        }
        Ok(())
    }

    fn last_seq(&self) -> u64 {
        self.ops.as_ref().map_or(0, |ops| ops.log.last_seq())
    }

    fn audit_pruned_before(&self) -> Result<Option<u64>, BackendError> {
        match &self.ops {
            Some(ops) => Ok(audit_file::pruned_before(ops.log.path())?),
            None => Ok(None),
        }
    }

    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError> {
        let Some(file) = &self.jti else {
            return Ok(());
        };
        file.spend_at(jti, until_unix, now_unix)
            .map_err(|source| match source.kind() {
                io::ErrorKind::AlreadyExists => BackendError::Spent,
                _ => BackendError::Jti(JtiError::Io {
                    path: file.path().to_owned(),
                    source,
                }),
            })
    }
}
