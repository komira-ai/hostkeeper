//! The ops store's audit-only append ([`super::OpsStore::append_audit_only`]): a numbered audit
//! line that changes no operations state (`docs/design/console-broker.md`, decision D4), kept apart
//! so that `ops.rs` stays short.
//!
//! The line takes the next audit number, as every change's does, and lands where every change's
//! lines land (the audit file, or the database's `audit` table), so `Audit.List` pages it with
//! them. It is not a change: the state in memory, the ops file and the ring that `/v1/status`
//! shows are left as they are, so a burst of console lines cannot push an operator's changes out
//! of the ring, and a store that rebuilds the ring from its audit at start leaves such lines out
//! ([`AuditOnly::is_audit_only`]). The facts' generations are not raised either: no console line
//! changes what a host's facts say.
//!
//! Lines land in their numbers' order: what is pending in memory is committed first, and the
//! append is refused when that fails, as a storage-first change is. Over Postgres the line is
//! numbered by the audit counter in an edit that holds the counter's row and nothing else
//! ([`Scope::AUDIT`]): it takes neither the gate nor a part.

use std::io;

use super::{AuditEntry, OpsError, OpsStore, announce};
use crate::audit_actions::AuditOnly;
use crate::audit_file::AuditLine;
use crate::ops::AuditBy;
use crate::store::{Commit, Scope};

impl OpsStore {
    /// Appends the audit line of `action` by `by`, concerning the host `node_id` with `reason`,
    /// and answers it numbered.
    ///
    /// # Errors
    /// [`OpsError::AuditUnavailable`]: the line could not be appended, or what was pending before
    /// it could not be committed. The caller does nothing. The line was not appended, and the next
    /// line takes the number this one would have, but for a database commit of unknown outcome:
    /// that line may have landed, and its number stays taken whether or not it did.
    pub(crate) fn append_audit_only(
        &self,
        action: AuditOnly,
        by: &(impl AuditBy + ?Sized),
        node_id: Option<&str>,
        reason: Option<&str>,
    ) -> Result<AuditEntry, OpsError> {
        let _writer = self.writing.lock();
        self.flush().map_err(unavailable)?;
        let mut entry = AuditEntry::new(by, action.as_str());
        entry.node_id = node_id.map(str::to_owned);
        entry.reason = reason.map(str::to_owned);
        let edit = self
            .backend
            .begin(Scope::AUDIT)
            .map_err(OpsError::from)
            .map_err(unavailable)?;
        let ringed = self.lock().audit.last().map_or(0, |last| last.seq);
        entry.seq = edit
            .next_seq()
            .unwrap_or_else(|| self.backend.last_seq().max(ringed) + 1);
        let line = AuditLine::of(&entry, (self.unix)());
        let commit = Commit {
            audit: std::slice::from_ref(&line),
            ..Commit::default()
        };
        edit.commit_or(&commit, |commit| self.backend.commit(commit))
            .map_err(OpsError::from)
            .map_err(unavailable)?;
        announce(&entry);
        Ok(entry)
    }
}

/// `why` as the one error an audit-only append answers: whatever kept the line out, the caller
/// does nothing (`audit_unavailable`).
fn unavailable(why: OpsError) -> OpsError {
    match why {
        OpsError::AuditUnavailable(_) => why,
        other => OpsError::AuditUnavailable(io::Error::other(other.to_string())),
    }
}
