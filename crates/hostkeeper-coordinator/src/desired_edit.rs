//! The desired store's changes as edits of its backend ([`crate::store::Edit`],
//! `docs/design/coordinator-multi-instance.md`, section 6), kept apart so that `desired.rs` stays
//! short.
//!
//! A change of the desired state alone (CI's set, a rollback of a group's version) is staged under
//! the writers' lock in an edit begun there ([`Scope::DESIRED`]): over Postgres the edit holds the
//! gate and the part's row from the moment the change is staged, and what it read afresh is taken
//! into memory before the change is made on it, so a precondition the change checks (an
//! `if_generation`) is checked against the rows as they are.
//!
//! A change staged to commit with the operations state's audit entries
//! ([`crate::ops::OpsStore::decide_changes_with`]: a force, a group's set, a host group's or a
//! repository's change) is staged on memory, which every request reads again first, and its edit
//! ([`Scope::DESIRED_AND_OPS`]) is begun by the ops store once it holds its own writers' lock: a
//! store's lock is always taken before any lock of the database, so no thread waits for a store's
//! lock while it holds the gate. When that edit finds the desired state's rows newer than memory
//! (another coordinator committed a change of it since this request read it), memory takes the
//! rows and the change is refused, to be made again on them ([`Staged::stale`]).

use std::sync::{MutexGuard, PoisonError};

use super::{DesiredStore, Held, Recorded, StoreError};
use crate::store::{BackendError, Commit, Edit, Part, Scope, Shared};

/// A desired state made and not yet in memory: what [`DesiredStore::stage`] answers, for a change
/// that commits it with another part ([`crate::force`]). The store's writers wait while it lives,
/// and the edit it was staged in, if any, holds what it locks until it is committed or dropped.
pub(crate) struct Staged<'a> {
    store: &'a DesiredStore,
    _writer: MutexGuard<'a, ()>,
    edit: Option<Result<Edit, BackendError>>,
    pub(crate) before: Held,
    pub(crate) after: Held,
}

impl Staged<'_> {
    /// The backend the store commits to.
    pub(crate) fn backend(&self) -> &Shared {
        &self.store.backend
    }

    /// The part of a commit it is: none when it changes nothing.
    pub(crate) fn part(&self) -> Option<(&Held, &Held)> {
        (self.before != self.after).then_some((&self.before, &self.after))
    }

    /// The edit it was staged in, for its commit: none for a change staged on memory, whose
    /// edit its committer begins.
    pub(crate) fn take_edit(&mut self) -> Option<Result<Edit, BackendError>> {
        self.edit.take()
    }

    /// The change was staged on a desired state older than the rows `fresh` an edit read: memory
    /// takes them, and the change is not made.
    pub(crate) fn stale(&self, fresh: Held) {
        *self.store.lock() = fresh;
    }

    /// Takes it into memory: its commit is durable.
    pub(crate) fn finish(self) {
        *self.store.lock() = self.after;
    }
}

impl DesiredStore {
    /// A change of the desired state alone, staged under the writers' lock in an edit begun
    /// there, with nothing changed yet: memory takes the desired state the edit read afresh
    /// first. An edit that cannot be begun is kept as its error, which the commit answers.
    pub(super) fn staging(&self) -> Staged<'_> {
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut edit = self.backend.begin(Scope::DESIRED);
        if let Some(held) = edit.as_mut().ok().and_then(Edit::take_desired) {
            *self.lock() = held;
        }
        self.staged_from(writer, Some(edit))
    }

    /// A change staged under the writers' lock on memory, to commit with the operations state
    /// in the edit the ops store begins.
    pub(super) fn staging_with_ops(&self) -> Staged<'_> {
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        self.staged_from(writer, None)
    }

    fn staged_from<'a>(
        &'a self,
        writer: MutexGuard<'a, ()>,
        edit: Option<Result<Edit, BackendError>>,
    ) -> Staged<'a> {
        let before = self.lock().clone();
        Staged {
            store: self,
            _writer: writer,
            edit,
            after: before.clone(),
            before,
        }
    }

    /// Commits `staged` and takes it into memory; nothing when it changes nothing.
    pub(crate) fn commit_staged(&self, mut staged: Staged<'_>) -> Result<Recorded, StoreError> {
        if staged.part().is_none() {
            return Ok(Recorded::Unchanged);
        }
        let edit = staged
            .take_edit()
            .unwrap_or_else(|| Ok(Edit::passing()))
            .map_err(BackendError::into_store)?;
        let commit = Commit {
            desired: staged.part(),
            ..Commit::default()
        };
        match edit
            .commit_or(&commit, |commit| self.backend.commit(commit))
            .map_err(BackendError::into_store)
        {
            Ok(()) => {}
            Err(e @ StoreError::NotSynced { .. }) => {
                staged.finish();
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        staged.finish();
        Ok(Recorded::Written)
    }

    /// Reads the desired state again when the backend's rows moved on since memory's copy was
    /// read (`docs/design/coordinator-multi-instance.md`, section 6, "Invalidation"); not while a
    /// change holds the writers' lock, which reads the part afresh itself.
    pub(crate) fn refresh(&self) {
        let Ok(_writer) = self.writing.try_lock() else {
            return;
        };
        match self.backend.reload(Part::Desired) {
            Ok(fresh) => {
                if let Some(held) = fresh.desired {
                    *self.lock() = held;
                }
            }
            Err(why) => tracing::warn!(%why, "the desired state could not be read again"),
        }
    }
}
