//! The ops store's hooks for tests ([`super::OpsStore`]): kept apart so that `ops.rs` stays
//! short.

use std::sync::PoisonError;

use super::{Appender, AuditEntry, OpsError, OpsState, OpsStore, UnixClock, Writer};

impl OpsStore {
    /// Every change of the audit log, oldest first ([`Self::audit_each`]).
    pub(crate) fn audit_log(&self) -> Result<Vec<AuditEntry>, OpsError> {
        let mut all = Vec::new();
        self.audit_each(&mut |entry| all.push(entry))?;
        Ok(all)
    }

    /// The store with `unix` as its clock for the audit file.
    pub(crate) fn with_unix(mut self, unix: UnixClock) -> Self {
        self.unix = unix;
        self
    }

    /// The store with `write` putting its states into the file instead, its marker kept.
    pub(crate) fn with_write(mut self, write: Writer) -> Self {
        self.write = write;
        self
    }

    /// The store with `append` appending its audit lines instead of the audit file, which is
    /// still what the store reads.
    pub(crate) fn with_append(mut self, append: Appender) -> Self {
        self.append = append;
        self
    }

    /// Makes `change` at the start of the next change, under the writers' lock, as another
    /// writer that got there between that caller's read and its change would.
    pub(crate) fn interleave(&self, change: impl FnOnce(&mut OpsState) + Send + 'static) {
        *self
            .interleave
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(change));
    }

    pub(super) fn interleaved(&self) {
        let change = self
            .interleave
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(change) = change {
            change(&mut self.lock());
        }
    }
}
