//! The ops store's hooks for tests ([`super::OpsStore`]): kept apart so that `ops.rs` stays
//! short.

use std::sync::{Arc, PoisonError};

use super::{AuditEntry, OpsError, OpsState, OpsStore, UnixClock};
use crate::audit_file::Appender;
use crate::store::{BackendError, Fake, Files, OpsWriter};

impl OpsStore {
    /// Every change of the audit log, oldest first ([`Self::audit_each`]).
    pub(crate) fn audit_log(&self) -> Result<Vec<AuditEntry>, OpsError> {
        let mut all = Vec::new();
        self.audit_each(&mut |entry| all.push(entry))?;
        Ok(all)
    }

    /// The store with `unix` as its clock for the audit.
    pub(crate) fn with_unix(mut self, unix: UnixClock) -> Self {
        self.unix = unix;
        self
    }

    /// A store over `state` on `fake`, which holds `state` as committed: the in-process tests'
    /// store, whose commits fail when the fake is told to ([`Fake::refuse`]).
    pub(crate) fn on_fake(fake: &Fake, state: OpsState) -> Self {
        Self::over(Arc::new(fake.clone()), state)
    }

    /// A store over `state` on a fake that holds it as committed and refuses every commit whose
    /// operations state `write` answers an error for, with that error: what a file system that
    /// fails those writes does.
    pub(crate) fn refusing(state: OpsState, write: OpsWriter) -> Self {
        let fake = Fake::holding(state.clone());
        fake.refuse(move |change| {
            let (_, after) = change.ops?;
            write(after).err().map(BackendError::Ops)
        });
        Self::on_fake(&fake, state)
    }

    /// The store over its file opened again with `hook` made to its files: a writer or an
    /// appender of the test's in place of the file system's.
    fn with_files(mut self, hook: impl FnOnce(Files) -> Files) -> Self {
        let (path, rotation) = self.files.clone().expect("a store over a file");
        let files = Files::default()
            .with_ops(&path, rotation)
            .expect("the ops files open again");
        self.backend = Arc::new(hook(files));
        self
    }

    /// The store with `write` putting its states into the file instead, its marker kept.
    pub(crate) fn with_write(self, write: OpsWriter) -> Self {
        self.with_files(|files| files.with_ops_write(write))
    }

    /// The store with `append` appending its audit lines instead of the audit file, which is
    /// still what the store reads.
    pub(crate) fn with_append(self, append: Appender) -> Self {
        self.with_files(|files| files.with_append(append))
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
