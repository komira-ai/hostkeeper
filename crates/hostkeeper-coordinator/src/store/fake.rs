//! The in-memory [`Backend`]: a durable copy of what was committed, kept in memory. It is what a
//! coordinator built without files keeps its state in ([`crate::ops::OpsStore::volatile`],
//! [`crate::verdict_file::VerdictFile::volatile`]), and the backend of the in-process tests,
//! which tell it which commits to refuse ([`Fake::refuse`]) in place of a file system that fails.
//!
//! Clones share the copy: a store opened again over a clone is a coordinator started again on
//! what the first one committed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{Backend, BackendError, Commit, Loaded};
use crate::audit_file::{AuditLine, Logged, Query};
use crate::desired;
use crate::ops::OpsState;
use crate::verdict_file;

/// What a test makes of each commit: `None` to take it, or the error to refuse it with.
#[cfg(test)]
pub(crate) type Refusal = Box<dyn FnMut(&Commit<'_>) -> Option<BackendError> + Send>;

/// What was committed.
#[derive(Default)]
struct Durable {
    desired: desired::Held,
    ops: OpsState,
    verdicts: verdict_file::Held,
    audit: Vec<AuditLine>,
    spent: BTreeMap<String, u64>,
    commits: u64,
    #[cfg(test)]
    refuse: Option<Refusal>,
}

/// The in-memory backend.
#[derive(Clone, Default)]
pub(crate) struct Fake {
    durable: Arc<Mutex<Durable>>,
}

impl Fake {
    /// A fake that holds nothing.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Durable> {
        self.durable.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Fake {
    /// A fake that holds `ops` as committed.
    #[must_use]
    pub(crate) fn holding(ops: OpsState) -> Self {
        let fake = Self::new();
        fake.lock().ops = ops;
        fake
    }

    /// From now on, `refuse` is asked of each commit: one it answers an error for is refused with
    /// it, and nothing of it is kept.
    pub(crate) fn refuse(
        &self,
        refuse: impl FnMut(&Commit<'_>) -> Option<BackendError> + Send + 'static,
    ) {
        self.lock().refuse = Some(Box::new(refuse));
    }

    /// From now on, every commit is refused as a store that cannot write ([`store_failure`]).
    pub(crate) fn fail(&self) {
        self.refuse(|_| Some(store_failure()));
    }

    /// From now on, every commit is taken again.
    pub(crate) fn heal(&self) {
        self.lock().refuse = None;
    }

    /// How many commits were taken.
    pub(crate) fn commits(&self) -> u64 {
        self.lock().commits
    }

    /// The operations state committed.
    pub(crate) fn ops(&self) -> OpsState {
        self.lock().ops.clone()
    }

    /// The desired state committed.
    pub(crate) fn desired(&self) -> desired::Held {
        self.lock().desired.clone()
    }

    /// The verdicts committed.
    pub(crate) fn verdicts(&self) -> verdict_file::Held {
        self.lock().verdicts.clone()
    }

    /// Every audit line committed, in order.
    pub(crate) fn audit(&self) -> Vec<AuditLine> {
        self.lock().audit.clone()
    }
}

/// The error of a store that cannot write: what a full disk or a lost database answers, refused
/// `store_failed`.
#[cfg(test)]
pub(crate) fn store_failure() -> BackendError {
    BackendError::Ops(crate::ops::OpsError::Io(desired::StoreError::Io {
        path: "fake".into(),
        source: std::io::Error::other("the fake refuses this commit"),
    }))
}

/// The error of a commit whose audit line cannot be kept, refused `audit_unavailable`.
#[cfg(test)]
pub(crate) fn audit_failure() -> BackendError {
    BackendError::Ops(crate::ops::OpsError::AuditUnavailable(
        std::io::Error::other("the fake refuses this commit's audit"),
    ))
}

impl Backend for Fake {
    fn load(&self) -> Result<Loaded, BackendError> {
        let durable = self.lock();
        Ok(Loaded {
            desired: durable.desired.clone(),
            ops: durable.ops.clone(),
            verdicts: durable.verdicts.clone(),
            spent: durable
                .spent
                .iter()
                .map(|(jti, until)| (jti.clone(), *until))
                .collect(),
        })
    }

    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError> {
        let mut durable = self.lock();
        #[cfg(test)]
        if let Some(why) = durable.refuse.as_mut().and_then(|refuse| refuse(change)) {
            return Err(why);
        }
        if let Some((_, after)) = change.desired {
            durable.desired = after.clone();
        }
        if let Some((_, after)) = change.ops {
            durable.ops = after.clone();
        }
        if let Some((_, after)) = change.verdicts {
            durable.verdicts = after.clone();
        }
        durable.audit.extend(change.audit.iter().cloned());
        durable.commits += 1;
        Ok(())
    }

    fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, BackendError> {
        let durable = self.lock();
        let mut page: Vec<Logged> = durable
            .audit
            .iter()
            .map(|line| line.clone().into_logged())
            .filter(|logged| {
                logged.entry.seq > query.since_seq && query.filter.keeps(&logged.entry)
            })
            .collect();
        let limit = query.limit.max(1);
        if query.newest {
            page.drain(..page.len().saturating_sub(limit));
        } else {
            page.truncate(limit);
        }
        Ok(page)
    }

    fn audit_each(&self, visit: &mut dyn FnMut(Logged)) -> Result<(), BackendError> {
        let lines = self.lock().audit.clone();
        lines.into_iter().for_each(|line| visit(line.into_logged()));
        Ok(())
    }

    fn last_seq(&self) -> u64 {
        self.lock().audit.last().map_or(0, |line| line.seq)
    }

    fn audit_pruned_before(&self) -> Result<Option<u64>, BackendError> {
        Ok(None)
    }

    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError> {
        let mut durable = self.lock();
        durable.spent.retain(|_, until| *until > now_unix);
        if durable.spent.contains_key(jti) {
            return Err(BackendError::Spent);
        }
        durable.spent.insert(jti.to_owned(), until_unix);
        durable.commits += 1;
        Ok(())
    }
}
