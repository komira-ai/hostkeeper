//! One edit as one transaction (`docs/design/coordinator-multi-instance.md`, section 6): what a
//! store's change locks, what it reads afresh under those locks, and the handle it commits
//! through.
//!
//! A store makes a change in three steps, all under its writers' lock: it begins an [`Edit`] of
//! its [`Scope`], takes into memory each part the edit read afresh (a copy whose version is older
//! than the rows), makes its edit on that memory as it always has, and commits the state before
//! and after through the edit. Over a backend whose rows other coordinators write (Postgres) the
//! edit is a transaction that holds, from its beginning to its commit, the rollout gate when a
//! decision reads one of its parts, then the row of each part in [`Part`]'s order, then the audit
//! counter when it writes audit lines: the edit is made on the rows as they are under those
//! locks, its audit lines are numbered by the counter, and nothing another coordinator commits
//! can come between. Over the files or the in-memory store, which one process owns, an edit holds
//! nothing and its commit is the backend's [`Backend::commit`](super::Backend::commit).

use std::collections::BTreeMap;

use super::postgres::Open;
use super::postgres::decide::Joined;
use super::{BackendError, Commit};
use crate::accounts::Accounts;
use crate::desired;
use crate::layout::Layout;
use crate::ops::OpsState;
use crate::verdict_file;

/// A part of the state: the unit of lock, cache and version. The order of the variants is the
/// order every transaction takes their rows in, after the gate and before the audit counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Part {
    /// The desired state, its registries and the host groups' desired versions.
    Desired,
    /// The operations state: halts, holds, forces, plans, labels, and the ring of changes.
    Ops,
    /// The users and API tokens.
    Accounts,
    /// The lab's layout.
    Layout,
    /// The verdicts acknowledged and counted.
    Verdicts,
}

impl Part {
    /// Every part, in the order of their locks.
    pub(crate) const ALL: [Part; 5] = [
        Part::Desired,
        Part::Ops,
        Part::Accounts,
        Part::Layout,
        Part::Verdicts,
    ];

    /// The name of its row of `state_versions`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Part::Desired => "desired",
            Part::Ops => "ops",
            Part::Accounts => "accounts",
            Part::Layout => "layout",
            Part::Verdicts => "verdicts",
        }
    }

    /// Whether a rollout decision reads it, so that an edit of it takes the gate first.
    pub(crate) fn decided(self) -> bool {
        self != Part::Accounts
    }
}

/// How an edit asks for the rollout gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// An operator's edit, and anything not made inside a heartbeat: it queues for the gate, for
    /// at most `--operator-gate-wait-ms`, and is refused when that runs out.
    Operator,
    /// A heartbeat's own write: it asks for the gate without waiting (`SKIP LOCKED`), and a gate
    /// held, or asked for by a waiting edit, refuses it at once. The next heartbeat writes again.
    Beat,
}

/// What one edit locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    /// The parts it writes, in [`Part`]'s order, each at most once.
    pub(crate) parts: &'static [Part],
    /// Whether it writes audit lines, numbered by the audit counter.
    pub(crate) audited: bool,
    /// How it asks for the gate, when one of its parts is [`Part::decided`].
    pub(crate) wait: Wait,
}

impl Scope {
    /// A change of the desired state alone, which writes no audit line (CI's set).
    pub(crate) const DESIRED: Scope = Scope {
        parts: &[Part::Desired],
        audited: false,
        wait: Wait::Operator,
    };

    /// A change of the operations state, with its audit lines.
    pub(crate) const OPS: Scope = Scope {
        parts: &[Part::Ops],
        audited: true,
        wait: Wait::Operator,
    };

    /// A change of the desired and the operations state together, with its audit lines: a force,
    /// a group's set, a host group's or a repository's change.
    pub(crate) const DESIRED_AND_OPS: Scope = Scope {
        parts: &[Part::Desired, Part::Ops],
        audited: true,
        wait: Wait::Operator,
    };

    /// A change of the users and API tokens.
    pub(crate) const ACCOUNTS: Scope = Scope {
        parts: &[Part::Accounts],
        audited: false,
        wait: Wait::Operator,
    };

    /// A change of the layout.
    pub(crate) const LAYOUT: Scope = Scope {
        parts: &[Part::Layout],
        audited: false,
        wait: Wait::Operator,
    };

    /// A heartbeat's acknowledgement or count of verdicts: never waits for the gate.
    pub(crate) const VERDICTS: Scope = Scope {
        parts: &[Part::Verdicts],
        audited: false,
        wait: Wait::Beat,
    };

    /// A heartbeat's decision (`docs/design/coordinator-multi-instance.md`, section 5.2): every
    /// part a decision reads and may write, with its audit lines, under the gate it never waits
    /// for. The layout is read by no decision and written by none.
    pub(crate) const DECIDE: Scope = Scope {
        parts: &[Part::Desired, Part::Ops, Part::Verdicts],
        audited: true,
        wait: Wait::Beat,
    };

    /// An audit line that changes no state (the console's).
    pub(crate) const AUDIT: Scope = Scope {
        parts: &[],
        audited: true,
        wait: Wait::Operator,
    };

    /// A commit of any parts made without a store's edit ([`super::Backend::commit`]): it holds
    /// every part.
    pub(crate) const WHOLE: Scope = Scope {
        parts: &Part::ALL,
        audited: true,
        wait: Wait::Operator,
    };

    /// Every scope an edit is begun with: what the lock order is checked over.
    #[cfg(test)]
    pub(crate) const ALL: [Scope; 9] = [
        Scope::DESIRED,
        Scope::OPS,
        Scope::DESIRED_AND_OPS,
        Scope::ACCOUNTS,
        Scope::LAYOUT,
        Scope::VERDICTS,
        Scope::DECIDE,
        Scope::AUDIT,
        Scope::WHOLE,
    ];

    /// Whether it takes the gate.
    pub(crate) fn gated(self) -> bool {
        self.parts.iter().any(|part| part.decided())
    }

    /// The locks it takes, in the order it takes them.
    pub(crate) fn locks(self) -> Vec<Lock> {
        let gate = self.gated().then_some(Lock::Gate);
        let parts = self.parts.iter().map(|part| Lock::Part(*part));
        let counter = self.audited.then_some(Lock::AuditCounter);
        gate.into_iter().chain(parts).chain(counter).collect()
    }
}

/// A row a transaction locks, in the one global order: the gate, the parts' rows of
/// `state_versions` in [`Part`]'s order, the audit counter last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lock {
    /// The row of `rollout_gate`.
    Gate,
    /// A part's row of `state_versions`.
    Part(Part),
    /// The row of `audit_counter`.
    AuditCounter,
}

/// The facts generation of each host that has one, by host id ([`crate::facts`]).
pub(crate) type Facts = BTreeMap<String, u64>;

/// The parts an edit or a reload read afresh: each one this coordinator's copy was older than.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fresh {
    pub(crate) desired: Option<desired::Held>,
    /// The operations state with its ring of changes, and the hosts' facts generations.
    pub(crate) ops: Option<(OpsState, Facts)>,
    pub(crate) accounts: Option<Accounts>,
    pub(crate) layout: Option<Layout>,
    pub(crate) verdicts: Option<verdict_file::Held>,
}

/// An edit begun ([`super::Backend::begin`]), until it is committed or dropped. Dropped without a
/// commit, it changes nothing: its transaction, if it has one, is rolled back.
#[derive(Default)]
pub(crate) struct Edit {
    fresh: Fresh,
    next_seq: Option<u64>,
    open: Option<Open>,
    /// The heartbeat's decision this edit is made in, when its thread holds one.
    joined: Option<Joined>,
}

impl Edit {
    /// An edit over a backend that one process owns: it holds nothing, reads nothing afresh, and
    /// its commit is the backend's.
    pub(crate) fn passing() -> Edit {
        Edit::default()
    }

    /// An edit holding the transaction `open`, with what it read afresh and the number of its
    /// first audit line, if it writes any.
    pub(crate) fn holding(open: Open, fresh: Fresh, next_seq: Option<u64>) -> Edit {
        Edit {
            fresh,
            next_seq,
            open: Some(open),
            joined: None,
        }
    }

    /// An edit made in the heartbeat's decision `joined`, its first audit line numbered
    /// `next_seq`: it holds the decision's locks and reads nothing afresh.
    pub(crate) fn joined(joined: Joined, next_seq: Option<u64>) -> Edit {
        Edit {
            next_seq,
            joined: Some(joined),
            ..Edit::default()
        }
    }

    /// Whether it holds a transaction: while it does, the store makes no other write.
    pub(crate) fn holds(&self) -> bool {
        self.open.is_some() || self.joined.is_some()
    }

    /// The number its first audit line takes: none over a backend that numbers no lines itself.
    pub(crate) fn next_seq(&self) -> Option<u64> {
        self.next_seq
    }

    /// Takes `part` as read afresh: the caller puts it in memory now, and its copy then carries
    /// the version it was read at.
    fn take<T>(&mut self, part: Part, pick: impl FnOnce(&mut Fresh) -> Option<T>) -> Option<T> {
        let taken = pick(&mut self.fresh)?;
        self.open.iter_mut().for_each(|open| open.taken(part));
        Some(taken)
    }

    /// The desired state read afresh, for the caller's memory.
    pub(crate) fn take_desired(&mut self) -> Option<desired::Held> {
        self.take(Part::Desired, |fresh| fresh.desired.take())
    }

    /// The operations state and the facts generations read afresh, for the caller's memory.
    pub(crate) fn take_ops(&mut self) -> Option<(OpsState, Facts)> {
        self.take(Part::Ops, |fresh| fresh.ops.take())
    }

    /// The accounts read afresh, for the caller's memory.
    pub(crate) fn take_accounts(&mut self) -> Option<Accounts> {
        self.take(Part::Accounts, |fresh| fresh.accounts.take())
    }

    /// The layout read afresh, for the caller's memory.
    pub(crate) fn take_layout(&mut self) -> Option<Layout> {
        self.take(Part::Layout, |fresh| fresh.layout.take())
    }

    /// The verdicts read afresh, for the caller's memory.
    pub(crate) fn take_verdicts(&mut self) -> Option<verdict_file::Held> {
        self.take(Part::Verdicts, |fresh| fresh.verdicts.take())
    }

    /// Commits `change`, whose audit lines are numbered from [`Edit::next_seq`] when it has one:
    /// in the edit's transaction when it holds one, and otherwise by `passing`, the backend's own
    /// commit.
    ///
    /// # Errors
    /// [`BackendError`]: the change is not durable.
    pub(crate) fn commit_or(
        self,
        change: &Commit<'_>,
        passing: impl FnOnce(&Commit<'_>) -> Result<(), BackendError>,
    ) -> Result<(), BackendError> {
        match (self.open, self.joined) {
            (Some(open), _) => open.commit(change),
            (None, Some(joined)) => joined.commit(change),
            (None, None) => passing(change),
        }
    }
}
