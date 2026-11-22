//! The ops store's changes as edits of its backend ([`crate::store::Edit`],
//! `docs/design/coordinator-multi-instance.md`, section 6), kept apart so that `ops.rs` stays
//! short.
//!
//! A change begins an edit of the operations state under the writers' lock, takes into memory
//! what the edit read afresh (over Postgres: the rows, when another coordinator changed them since
//! this one's copy was read), makes its edit on that memory, numbers its entries from the audit
//! counter's number when the edit has one, and commits through the edit: over Postgres, one
//! transaction that held the gate, the part's row and the counter from its beginning. Over the
//! files and the in-memory store an edit holds nothing, and the change is what it always was.
//!
//! A change pending in memory (a memory-first change whose commit failed) is committed before a
//! storage-first change's edit begins, so that no change lands ahead of an earlier one. A
//! memory-first change made while none is pending is made in an edit too, on the rows as they are;
//! one made while a change is pending, or when no edit can be begun (the database lost, the gate
//! not free in time), is made on memory and stays pending with the rest, as it always has.
//!
//! The writers' lock is taken before the edit is begun, and the desired store's (a change staged
//! with it, [`Staged`]) before that: a store's lock is never waited for by a thread that holds a
//! lock of the database.

use super::{AUDIT_RING, AuditEntry, OpsError, OpsState, OpsStore, Order, announce};
use crate::audit_file::AuditLine;
use crate::desired::Staged;
use crate::store::{Commit, Edit, Facts, Part, Scope, commit_across};

impl OpsStore {
    /// Takes the operations state and the facts generations `edit` read afresh into memory.
    pub(super) fn install(&self, edit: &mut Edit) {
        if let Some((state, facts)) = edit.take_ops() {
            self.take_fresh(state, facts);
        }
    }

    fn take_fresh(&self, state: OpsState, facts: Facts) {
        *self.lock() = state;
        self.generations.replace(facts);
    }

    /// Reads the operations state again when the backend's rows moved on since memory's copy was
    /// read (`docs/design/coordinator-multi-instance.md`, section 6, "Invalidation"). A store
    /// with a change pending keeps its memory, which holds a change the rows do not yet: the
    /// change's commit reads it again. A store whose writers' lock is held (a change is being
    /// made, and may be waiting for the gate) is not waited for: the change reads its part
    /// afresh, and the next request reads again what is still behind.
    pub(crate) fn refresh(&self) {
        let Some(_writer) = self.writing.try_lock() else {
            return;
        };
        if self.pending_changes() {
            return;
        }
        match self.backend.reload(Part::Ops) {
            Ok(fresh) => {
                if let Some((state, facts)) = fresh.ops {
                    self.take_fresh(state, facts);
                }
            }
            Err(why) => tracing::warn!(%why, "the operations state could not be read again"),
        }
    }

    /// [`OpsStore::decide_changes`] in the order `order`, the desired state `staged` committed
    /// with it when there is one.
    pub(super) fn audited_changes<E: From<OpsError>>(
        &self,
        order: Order,
        staged: Option<Staged<'_>>,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        if order == Order::StorageFirst {
            return self.stored_first(staged, edit);
        }
        let (entries, committed) = self.held_first(edit)?;
        match committed {
            // A raise stands when only its audit line failed, as it always has; the line is
            // committed with it later.
            Ok(()) | Err(OpsError::AuditUnavailable(_)) => Ok(entries),
            Err(why) => Err(why.into()),
        }
    }

    /// The edit a storage-first change is made in, of the operations state, and of the desired
    /// state too when `with_desired` (a change staged with it): begun under the writers' lock, with
    /// what is pending committed first, in an edit of its own, when the edit holds a transaction.
    fn opened(&self, with_desired: bool) -> Result<Edit, OpsError> {
        let scope = match with_desired {
            true => Scope::DESIRED_AND_OPS,
            false => Scope::OPS,
        };
        let edit = self.backend.begin(scope)?;
        if !edit.holds() || !self.pending_changes() {
            return Ok(edit);
        }
        drop(edit);
        self.flush()?;
        Ok(self.backend.begin(scope)?)
    }

    /// A storage-first change: what is pending committed first, so that no change is committed
    /// ahead of an earlier one, then the change (with the desired state `staged`, if any), and
    /// memory only once it is committed.
    fn stored_first<E: From<OpsError>>(
        &self,
        staged: Option<Staged<'_>>,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        let _writer = self.writing.lock();
        #[cfg(test)]
        self.interleaved();
        let mut tx = self.opened(staged.is_some())?;
        if let (Some(fresh), Some(staged)) = (tx.take_desired(), &staged) {
            // Staged on a desired state older than the rows: memory takes them, and the
            // operations state the edit read afresh too, so that the caller decides the change
            // again on the state as stored ([`OpsError::DesiredChanged`]). The edit is rolled
            // back: nothing changed.
            staged.stale(fresh);
            self.install(&mut tx);
            return Err(OpsError::DesiredChanged.into());
        }
        self.install(&mut tx);
        let before = self.state();
        let mut next = before.clone();
        let mut entries = edit(&mut next)?;
        if !tx.holds() {
            self.flush()?;
        }
        let lines = self.numbered(&mut next, &mut entries, tx.next_seq());
        let commit = Commit {
            desired: staged.as_ref().and_then(Staged::part),
            ops: Some((&before, &next)),
            audit: &lines,
            ..Commit::default()
        };
        tx.commit_or(&commit, |commit| match &staged {
            Some(staged) => commit_across(staged.backend(), &self.backend, commit),
            None => self.backend.commit(commit),
        })
        .map_err(OpsError::from)?;
        *self.lock() = next;
        self.generations.raise(&entries);
        if let Some(staged) = staged {
            staged.finish();
        }
        entries.iter().for_each(announce);
        Ok(entries)
    }

    /// A memory-first change: in memory, then held pending under the marker and committed. Answers
    /// the entries and how the commit went; a commit that failed leaves the change pending.
    pub(super) fn held_first<E>(
        &self,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<(Vec<AuditEntry>, Result<(), OpsError>), E> {
        let _writer = self.writing.lock();
        #[cfg(test)]
        self.interleaved();
        // With nothing pending the change is made in an edit, on the state as the backend holds
        // it; otherwise, and when no edit can be begun, on memory, committed with what is pending.
        let mut tx = match self.pending_changes() {
            true => Err(None),
            false => self.backend.begin(Scope::OPS).map_err(Some),
        };
        if let Ok(tx) = &mut tx {
            self.install(tx);
        }
        let before = self.state();
        let mut next = before.clone();
        let mut entries = edit(&mut next)?;
        let first = tx.as_ref().ok().and_then(Edit::next_seq);
        let lines = self.numbered(&mut next, &mut entries, first);
        *self.lock() = next;
        entries.iter().for_each(announce);
        self.hold(before, lines);
        let committed = match tx {
            Ok(tx) => self.commit_pending(tx),
            Err(None) => self.flush(),
            Err(Some(why)) => Err(why.into()),
        };
        if let Err(why) = &committed {
            tracing::error!(
                alert = "halt",
                %why,
                "a change stands in memory and is not yet written: its commit is tried again at every heartbeat and before the next change"
            );
        }
        Ok((entries, committed))
    }

    /// Numbers `entries` from `first`, the audit counter's number, or, over a backend that has
    /// none, on from the newest number in memory or in storage; puts them in the ring of `next`,
    /// and answers their audit lines, stamped now.
    pub(super) fn numbered(
        &self,
        next: &mut OpsState,
        entries: &mut [AuditEntry],
        first: Option<u64>,
    ) -> Vec<AuditLine> {
        let mut seq = match first {
            Some(first) => first.saturating_sub(1),
            None => {
                let ringed = next.audit.last().map_or(0, |last| last.seq);
                self.backend.last_seq().max(ringed)
            }
        };
        for entry in entries.iter_mut() {
            seq += 1;
            entry.seq = seq;
        }
        next.audit.extend(entries.iter().cloned());
        let surplus = next.audit.len().saturating_sub(AUDIT_RING);
        next.audit.drain(..surplus);
        let at = (self.unix)();
        entries.iter().map(|e| AuditLine::of(e, at)).collect()
    }
}
