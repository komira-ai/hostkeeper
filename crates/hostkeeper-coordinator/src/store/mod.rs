//! Where the coordinator's state is durable: the [`Backend`] the three stores call underneath
//! (`docs/design/coordinator-storage.md`, section 4.1).
//!
//! The gate and the API code change state through the stores' own calls ([`OpsStore::change`]
//! and its siblings, [`DesiredStore::record`], [`VerdictFile::record`]). A store makes its edit on
//! a clone of its state under its writers' lock, as it always has, and hands the backend the state
//! before and after with the audit lines of the change, in one [`Commit`]. A backend answers Ok
//! only when the whole commit is durable.
//!
//! Two backends are built: [`Files`], the files of `--state-file` and `--ops-file` written as they
//! always were (each part replaced whole), and [`Fake`], an in-memory copy for a coordinator built
//! without files and for the in-process tests, which fail the commits they choose through it.
//!
//! A commit that names the desired state and another part (a force, [`crate::force`]) is made by
//! [`commit_across`]: one commit when both stores are over the same backend; otherwise, and
//! always for the files, which cannot replace two files at once, the desired state first and the
//! rest after, with the desired state put back when the rest fails ([`in_turn`]).
//!
//! [`OpsStore::change`]: crate::ops::OpsStore::change
//! [`DesiredStore::record`]: crate::desired::DesiredStore::record
//! [`VerdictFile::record`]: crate::verdict_file::VerdictFile::record

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_oidc::{Clock, SpentJtis};

use crate::audit_file::{AuditLine, Logged, Query};
use crate::desired::{self, StoreError};
use crate::jti_file::JtiError;
use crate::ops::{OpsError, OpsState};
use crate::verdict_file::{self, VerdictsError};

mod fake;
mod files;
pub(crate) mod postgres;

pub(crate) use fake::Fake;
#[cfg(test)]
pub(crate) use fake::store_failure;
pub(crate) use files::Files;
pub(crate) use postgres::{Landing, Postgres};
#[cfg(test)]
pub(crate) use files::OpsWriter;

/// Where the coordinator's state is durable.
pub(crate) trait Backend: Send + Sync {
    /// Everything, read once at start.
    ///
    /// # Errors
    /// [`BackendError`]: what is held cannot be read, or holds what this build refuses.
    fn load(&self) -> Result<Loaded, BackendError>;

    /// One change, durable when this returns Ok, or not at all.
    ///
    /// # Errors
    /// [`BackendError`]: the change is not durable.
    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError>;

    /// The page of the audit log `query` asks for, oldest first.
    ///
    /// # Errors
    /// [`BackendError`]: the audit log cannot be read.
    fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, BackendError>;

    /// Visits every audit line held, in the order it was committed.
    ///
    /// # Errors
    /// [`BackendError`]: the audit log cannot be read.
    fn audit_each(&self, visit: &mut dyn FnMut(Logged)) -> Result<(), BackendError>;

    /// The number of the newest audit line held: 0 when none is.
    fn last_seq(&self) -> u64;

    /// The time of the oldest audit line still held when older ones were deleted by a retention;
    /// `None` when none was.
    ///
    /// # Errors
    /// [`BackendError`]: the audit log cannot be read.
    fn audit_pruned_before(&self) -> Result<Option<u64>, BackendError>;

    /// Records that CI's token id `jti` is spent until `until_unix`, durably, dropping the ids
    /// already past at `now_unix`.
    ///
    /// # Errors
    /// [`BackendError::Spent`]: the id is spent and not yet past. Any other: it is not recorded.
    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError>;

    /// Whether the backend takes writes now. A store that cannot lose its link to where it is
    /// durable (the files, the fake) is always up.
    fn health(&self) -> Health {
        Health::Up
    }

    /// Asks a backend whose link is lost to make it again, without waiting for it: called at
    /// every heartbeat.
    fn revive(&self) {}

    /// What a commit of unknown outcome turned out to be, once its link is back, taken once.
    fn take_landing(&self) -> Option<Landing> {
        None
    }

    /// The epoch this coordinator holds the store at, for a store that has one.
    fn epoch(&self) -> Option<u64> {
        None
    }
}

/// Whether a backend takes writes (`docs/design/coordinator-storage.md`, section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    /// It takes writes.
    Up,
    /// Its link is lost: every write is refused at once until it is made again. The coordinator
    /// orders nothing meanwhile.
    Lost,
    /// Another coordinator led the store while the link was lost, at epoch `found`: this one's
    /// memory is stale, and it exits.
    Deposed {
        /// The epoch found.
        found: u64,
    },
}

/// The state before and after one change, and its audit lines.
#[derive(Clone, Copy, Default)]
pub(crate) struct Commit<'a> {
    /// The desired state of every service, and the registry of each.
    pub(crate) desired: Option<(&'a desired::Held, &'a desired::Held)>,
    /// The operations state, the halts and the plan positions among it.
    pub(crate) ops: Option<(&'a OpsState, &'a OpsState)>,
    /// The verdicts acknowledged and counted.
    pub(crate) verdicts: Option<(&'a verdict_file::Held, &'a verdict_file::Held)>,
    /// The audit lines of the change, in their order.
    pub(crate) audit: &'a [AuditLine],
}

impl Commit<'_> {
    /// Whether the commit changes nothing but the desired state.
    fn desired_alone(&self) -> bool {
        matches!(
            self,
            Commit {
                ops: None,
                verdicts: None,
                audit: [],
                ..
            }
        )
    }
}

/// Everything a backend holds, as [`Backend::load`] reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Loaded {
    /// The desired state of every service, and the registry of each.
    pub(crate) desired: desired::Held,
    /// The operations state.
    pub(crate) ops: OpsState,
    /// The verdicts.
    pub(crate) verdicts: verdict_file::Held,
    /// The spent ids of CI's tokens, each with the second until which it is refused.
    pub(crate) spent: Vec<(String, u64)>,
}

/// Why a backend did not load or commit: the error of the part it failed on, as the store of that
/// part has always said it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendError {
    /// The desired state could not be read or written.
    #[error(transparent)]
    Desired(#[from] StoreError),
    /// The operations state or its audit could not be read or written.
    #[error(transparent)]
    Ops(#[from] OpsError),
    /// The verdicts could not be read or written.
    #[error(transparent)]
    Verdicts(#[from] VerdictsError),
    /// The spent token ids could not be read or written.
    #[error(transparent)]
    Jti(#[from] JtiError),
    /// The token id is spent and not yet past: a replay.
    #[error("the token id is spent")]
    Spent,
}

impl BackendError {
    /// The error as a file operation's: what it is when it is one, and otherwise an I/O error
    /// that says it, for a store whose own error is a file operation's.
    pub(crate) fn into_store(self) -> StoreError {
        match self {
            Self::Desired(e)
            | Self::Ops(OpsError::Io(e))
            | Self::Verdicts(VerdictsError::Io(e)) => e,
            other => StoreError::Io {
                path: PathBuf::from("store"),
                source: io::Error::other(other.to_string()),
            },
        }
    }
}

/// The shared handle of a backend.
pub(crate) type Shared = Arc<dyn Backend>;

/// Commits `change`, whose desired state is `desired`'s part, over `desired` and `rest`: in one
/// commit when they are the same backend, else [`in_turn`].
///
/// # Errors
/// [`BackendError`], as [`Backend::commit`].
pub(crate) fn commit_across(
    desired: &Shared,
    rest: &Shared,
    change: &Commit<'_>,
) -> Result<(), BackendError> {
    match change.desired {
        Some(part) if !Arc::ptr_eq(desired, rest) => in_turn(&**desired, &**rest, part, change),
        _ => rest.commit(change),
    }
}

/// Commits the desired state `part` of `change` to `first`, then the rest of it to `rest`; when
/// the rest fails, or the desired state's file is in place but not synced, the desired state is
/// put back as it was, so the change lands whole or, as far as files allow, not at all. A put-back
/// that fails is logged.
///
/// # Errors
/// [`BackendError`]: the first commit that failed.
pub(crate) fn in_turn(
    first: &dyn Backend,
    rest: &dyn Backend,
    part: (&desired::Held, &desired::Held),
    change: &Commit<'_>,
) -> Result<(), BackendError> {
    let (before, after) = part;
    let alone = |from, to| Commit {
        desired: Some((from, to)),
        ..Commit::default()
    };
    let put_back = |why: &BackendError| {
        if let Err(left) = first.commit(&alone(after, before)) {
            tracing::error!(
                %why,
                %left,
                "a change failed after its desired state was written, and the desired state could not be put back"
            );
        }
    };
    if let Err(why) = first.commit(&alone(before, after)) {
        if let BackendError::Desired(StoreError::NotSynced { .. }) = why {
            put_back(&why);
        }
        return Err(why);
    }
    let others = Commit {
        desired: None,
        ..*change
    };
    rest.commit(&others).inspect_err(put_back)
}

/// CI's spent token ids over a backend: the ids it loaded, and each spend committed to it before
/// the verifier accepts the token ([`SpentJtis`]).
pub(crate) struct Spent {
    backend: Shared,
    loaded: Vec<(String, u64)>,
    clock: Clock,
}

impl Spent {
    /// The spent ids `loaded` from `backend`, which records each spend by `clock`'s seconds.
    pub(crate) fn new(backend: Shared, loaded: Vec<(String, u64)>, clock: Clock) -> Self {
        Self {
            backend,
            loaded,
            clock,
        }
    }
}

impl SpentJtis for Spent {
    fn load(&self) -> Vec<(String, u64)> {
        self.loaded.clone()
    }

    fn record(&self, jti: &str, until: u64) -> io::Result<()> {
        self.backend
            .spend(jti, until, (self.clock)())
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
#[path = "conformance_tests.rs"]
mod conformance_tests;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
