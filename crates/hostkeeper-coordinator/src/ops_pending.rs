//! The ops store's changes in memory and not yet committed ([`super::OpsStore`]), kept apart so
//! that `ops.rs` stays short (`docs/design/coordinator-storage.md`, section 4.3).
//!
//! A memory-first change ([`super::Write::AfterMemory`], [`super::Write::Halt`], an operator's
//! `abort`) is in memory before its commit is tried. Until a commit holding it lands it is
//! **pending**: the store keeps the state as last committed beside the state in memory, with the
//! audit lines not yet committed. Every heartbeat tries one commit of the difference
//! ([`super::OpsStore::retry_unwritten`]), and the next storage-first change commits it first, in
//! a commit of its own, and is refused if that fails: no change is committed ahead of an earlier
//! one.
//!
//! A process that dies with a change pending starts again without it. So before the commit of a
//! memory-first change is tried, the store writes its marker (`<ops-file>.halts.unfinished` for
//! a store over a file) listing the audit number of every change pending, as
//! `{"unwritten":[41,42]}`, rewritten when one is added, and removes it once nothing is pending.
//! Every memory-first change has an audit entry, in the ring of the state committed with it, so
//! a committed state whose newest entry is at least a listed number holds that change: commits
//! land in their order. At start ([`Marker::settle`]) a marker whose every number the committed
//! state holds is removed; any other refuses the start, and so does a marker that lists nothing
//! this build reads (the empty marker of the release before, which marked a halt only).

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{OpsError, OpsState, OpsStore, UNWRITTEN_LOG_EVERY};
use crate::audit_file::AuditLine;
use crate::desired::{StoreError, write_whole};
use crate::store::{Commit, Health, Landing, Shared};

/// The changes in memory and not yet committed.
#[derive(Debug, Default)]
pub(super) struct Pending {
    /// The state as last committed, while memory holds changes it does not.
    base: Option<OpsState>,
    /// The audit lines of the changes pending, in their order: kept until they land, after the
    /// state when only they failed.
    lines: Vec<AuditLine>,
    /// Whether the marker is on disk.
    marked: bool,
    /// When a retry last logged that it failed.
    logged: Option<Duration>,
}

/// What the marker holds: the audit numbers, and the epoch of the store's database when it has
/// one (`--halt-marker-file`, `docs/design/coordinator-storage.md` section 4.3).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Listed {
    unwritten: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<u64>,
}

/// The marker found at start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    /// It lists nothing this build reads: the marker of the release before, or one cut short.
    Unlisted,
    /// The audit numbers of the changes pending when it was written.
    Listed(Vec<u64>),
}

impl Marker {
    /// The marker at `path`: none if there is none. One that cannot be read is
    /// [`Marker::Unlisted`].
    #[must_use]
    pub(crate) fn read(path: &Path) -> Option<Self> {
        match fs::read(path) {
            Ok(bytes) => Some(
                serde_json::from_slice::<Listed>(&bytes)
                    .map_or(Self::Unlisted, |listed| Self::Listed(listed.unwritten)),
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(Self::Unlisted),
        }
    }
}

/// Of the audit `numbers` a marker lists, those that `committed`, the state as loaded, does not
/// hold: none when every change it lists landed. Commits land in their order, so the state holds
/// every change numbered up to the newest entry of its ring.
#[must_use]
pub(crate) fn unlanded(numbers: &[u64], committed: &OpsState) -> Vec<u64> {
    let newest = committed.audit.last().map_or(0, |entry| entry.seq);
    numbers.iter().copied().filter(|n| *n > newest).collect()
}

/// Writes the marker at `path` listing `numbers` (and `epoch`), whole and owner-only, its
/// directory synced.
fn write_marker(path: &Path, numbers: Vec<u64>, epoch: Option<u64>) -> Result<(), StoreError> {
    let mut text = serde_json::to_vec(&Listed {
        unwritten: numbers,
        epoch,
    })
    .map_err(io::Error::from)
    .map_err(crate::desired::io_at(path))?;
    text.push(b'\n');
    write_whole(path, &text)
}

impl OpsStore {
    fn pending_lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds a memory-first change pending: `before` is the state committed before it, unless one
    /// was already pending, and `lines` are its audit lines. The marker is written before its
    /// commit is tried.
    pub(super) fn hold(&self, before: OpsState, lines: Vec<AuditLine>) {
        let mut pending = self.pending_lock();
        pending.base.get_or_insert(before);
        pending.lines.extend(lines);
        self.mark(&mut pending);
    }

    /// Writes the marker listing every pending change. One that cannot be written is logged; the
    /// commit is tried all the same, and the marker is taken as there (part of it may be), so it
    /// is removed once nothing is pending.
    fn mark(&self, pending: &mut Pending) {
        let Some(marker) = &self.marker else {
            return;
        };
        let numbers = pending.lines.iter().map(|line| line.seq).collect();
        if let Err(why) = write_marker(marker, numbers, self.backend.epoch()) {
            tracing::error!(
                alert = "halt",
                path = %marker.display(),
                %why,
                "the marker of changes not yet written could not be made"
            );
        }
        pending.marked = true;
    }

    /// Commits what is pending, if anything is, and removes the marker once nothing is. Called
    /// under the writers' lock.
    ///
    /// # Errors
    /// [`OpsError`]: the commit failed; the changes stay pending.
    pub(super) fn flush(&self) -> Result<(), OpsError> {
        let mut pending = self.pending_lock();
        if pending.base.is_some() || !pending.lines.is_empty() {
            let current = self.state();
            let base = pending.base.clone().unwrap_or_else(|| current.clone());
            let commit = Commit {
                ops: Some((&base, &current)),
                audit: &pending.lines,
                ..Commit::default()
            };
            match self.backend.commit(&commit).map_err(OpsError::from) {
                Ok(()) => {
                    *pending = Pending {
                        marked: pending.marked,
                        ..Pending::default()
                    };
                }
                Err(OpsError::AuditUnavailable(why)) => {
                    // Only the lines failed: the state is committed without them, as a raise
                    // always was over files, and the lines stay pending until they land.
                    let alone = Commit {
                        ops: Some((&base, &current)),
                        ..Commit::default()
                    };
                    if pending.base.is_some() && self.backend.commit(&alone).is_ok() {
                        pending.base = None;
                    }
                    return Err(OpsError::AuditUnavailable(why));
                }
                Err(why) => return Err(why),
            }
        }
        self.unmark(&mut pending);
        Ok(())
    }

    /// Removes the marker, when it is on disk. One that cannot be removed is logged and stays
    /// marked, so the next heartbeat tries again.
    fn unmark(&self, pending: &mut Pending) {
        let (true, Some(marker)) = (pending.marked, &self.marker) else {
            return;
        };
        match fs::remove_file(marker) {
            Err(why) if why.kind() != io::ErrorKind::NotFound => tracing::error!(
                path = %marker.display(),
                %why,
                "the marker of changes written could not be removed"
            ),
            _ => pending.marked = false,
        }
    }

    /// Whether a change is in memory and not yet committed (a halt, a bad mark, a pause, an
    /// abort), or its marker is not yet removed. Orders for a halted service stay held all the
    /// while: the halt is in memory.
    #[must_use]
    pub fn halt_unwritten(&self) -> bool {
        let pending = self.pending_lock();
        pending.base.is_some() || !pending.lines.is_empty() || pending.marked
    }

    /// Commits what is pending, when anything is: called at every heartbeat with the
    /// coordinator's time `now`. A retry that fails is logged at most once per
    /// [`UNWRITTEN_LOG_EVERY`]; one that lands removes the marker.
    pub fn retry_unwritten(&self, now: Duration) {
        if !self.halt_unwritten() {
            return;
        }
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        {
            // The marker is written again before each try: one removed by hand, or not made,
            // is there while the change is not committed.
            let mut pending = self.pending_lock();
            if pending.base.is_some() || !pending.lines.is_empty() {
                self.mark(&mut pending);
            }
        }
        match self.flush() {
            Ok(()) => tracing::warn!(
                alert = "halt",
                "a change that could not be written is written now"
            ),
            Err(why) => {
                let mut pending = self.pending_lock();
                let due = pending
                    .logged
                    .is_none_or(|at| now.saturating_sub(at) >= UNWRITTEN_LOG_EVERY);
                if due {
                    pending.logged = Some(now);
                    drop(pending);
                    tracing::error!(
                        alert = "halt",
                        %why,
                        "a change is still not written: it stands in memory, and its commit is tried at every heartbeat"
                    );
                }
            }
        }
    }
}

impl OpsStore {
    /// The backend the store commits to.
    pub(crate) fn backend(&self) -> &Shared {
        &self.backend
    }

    /// Whether the backend takes writes now.
    pub(crate) fn health(&self) -> Health {
        self.backend.health()
    }

    /// Takes the change of a commit of unknown outcome that `landing` found landed into memory
    /// and into the state as last committed, each with what memory changed since laid over it
    /// row by row; its audit lines are no longer pending. A commit that did not land leaves both
    /// as they are (section 4.4).
    pub(crate) fn take_landed(&self, landing: &Landing) {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(taken) = landing.ops_over(&self.state()) else {
            return;
        };
        let mut pending = self.pending_lock();
        if let Some(base) = pending.base.take() {
            pending.base = Some(landing.ops_over(&base).unwrap_or(base));
        }
        let landed: Vec<u64> = landing.lines().iter().map(|line| line.seq).collect();
        pending.lines.retain(|line| !landed.contains(&line.seq));
        drop(pending);
        *self.lock() = taken;
    }

    /// Drops what is pending, because another coordinator led the database while this one's
    /// link was lost (at epoch `found`) and its state now governs: each change dropped is logged
    /// by its audit number, the marker is removed (they are dropped knowingly), and the
    /// coordinator is told to stop ([`OpsStore::deposed`]).
    pub(crate) fn depose(&self, found: u64) {
        let mut pending = self.pending_lock();
        let dropped: Vec<u64> = pending.lines.iter().map(|line| line.seq).collect();
        tracing::error!(
            alert = "store_unreachable",
            found,
            ?dropped,
            "another coordinator led the database while this one's session was lost: the changes numbered here are dropped, and the coordinator stops"
        );
        pending.base = None;
        pending.lines.clear();
        pending.marked = true;
        self.unmark(&mut pending);
        drop(pending);
        let _ = self.deposed_by.set(found);
        self.deposed.notify_one();
    }

    /// Waits until the store is deposed ([`OpsStore::depose`]); the epoch found.
    pub(crate) async fn deposed(&self) -> u64 {
        self.deposed.notified().await;
        self.deposed_by.get().copied().unwrap_or_default()
    }
}
