//! The model `locks`: the one lock order of the coordinator's row transactions (design sections
//! 4, 5.2 and 6), checked for a deadlock.
//!
//! Locks: the gate row, the `state_versions` row of each part (`desired`, `ops`, `accounts`,
//! `layout`, `verdicts`, `enrollment`), the `audit_counter` row, the migration lock, and the
//! shared fence lock. A row lock or the migration lock is held by one transaction at a time, until
//! that transaction commits; a transaction that asks for a lock another holds waits for it. The
//! fence lock is shared: every instance holds it together, nothing in the new release waits for
//! it (only the release before asks for it exclusively, which is outside this model), so taking
//! it is a step that never waits.
//!
//! Processes: N instances. An instance is idle or runs one transaction of one kind, one lock
//! after another, one lock per step, then commits and releases all its locks in one step. An
//! idle instance may start a transaction of any kind, at any time, so that N concurrent
//! transactions of any mix are explored, and a finished instance may start another. The kinds
//! and the locks they take, in the order of the locked protocol: the deciding transaction of a
//! beat (gate, `ops`, `audit_counter`), an operator edit of `desired` (gate, `desired`,
//! `audit_counter`), an operator edit of `ops`, a hold or a halt raised or cleared (gate, `ops`,
//! `audit_counter`), a force or a retire (gate, `desired`, `ops`, `audit_counter`), an edit of
//! `accounts` (`accounts`, `audit_counter`), an edit of a user with its token, which spans
//! `accounts` and `enrollment` (`accounts`, `enrollment`, `audit_counter`), an enrollment
//! approve (`enrollment`, `audit_counter`), an edit of `layout` (gate, `layout`,
//! `audit_counter`), a verdict (gate, `verdicts`, `audit_counter`), and the migration runner
//! (fence lock, then migration lock), which never takes the gate or a `state_versions` row. The
//! migration runner shares no lock with any other kind, so it can join no cycle: that kind checks
//! only that the runner takes neither the gate nor a part row, as the design states. The model
//! holds the migration lock until commit, where the design holds it as a session lock around the
//! whole runner; with no other kind asking for it, the difference changes no wait.
//!
//! The kinds that span parts rest on the design's reading that an edit of accounts or of
//! enrollment, and an enrollment approve or renew, take the `state_versions` row of their part,
//! and that a flow across parts takes the rows in the list order (section 6). The edit of a user
//! with its token is taken to span `accounts` then `enrollment`, and the approve to take
//! `enrollment`; the second variant fails only because of these two readings.
//!
//! The gate of a beat's deciding transaction is asked for with `SKIP LOCKED`: when another
//! transaction holds it, the beat gives up at once, holding nothing, and never waits. Every other
//! lock request waits.
//!
//! Assumption: every other row a transaction reads or writes (order, plan and host rows, the
//! ledger insert) is reachable only while one of the modelled locks is held, or is never waited
//! on while another lock is held, so it adds no wait edge. The exceptions are the enrollment
//! request and host rows an approve reads `FOR UPDATE`, and the table locks of the migration
//! runner's DDL; the model leaves them out, the second being bounded by the DDL lock timeout of
//! section 4 and not by lock order.
//!
//! Left out: (a) a waiter is queued behind the holder alone, where the server also queues it
//! behind earlier waiters; a waiting transaction holds only locks earlier in the order, so the
//! extra waits add no cycle. (b) A crash, an abort or an idle timeout of a transaction is not a
//! step; each only releases locks, so none can make a cycle. (c) No lock timeout and no deadlock
//! detector of the server end a wait, because the design counts a deadlock as a bug: a cycle of
//! waiting transactions is a deadlock that stays, so the check finds any cycle.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "locks",
    bound: "N instances, each running one transaction at a time of any of 10 kinds and then \
            another, without end; 9 locks (gate, 6 part rows, audit counter, migration lock) \
            and the shared fence lock; no lock timeout",
    invariants: &["lock_order_has_no_cycle"],
    cases: &[
        VariantCase {
            variant: "edit_takes_the_gate_after_its_part_row",
            breaks: "lock_order_has_no_cycle",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "audit_counter_taken_before_a_part_row",
            breaks: "lock_order_has_no_cycle",
            fails_from: Some(2),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Locks::new(n, variant), only)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// An operator edit that takes the gate (of `desired`, `ops`, `layout` or verdicts) takes its
    /// `state_versions` row, then the gate.
    EditTakesTheGateAfterItsPartRow,
    /// An edit of `accounts` (alone, or with its token) takes the `audit_counter` row before its
    /// `state_versions` rows.
    AuditCounterTakenBeforeAPartRow,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("edit_takes_the_gate_after_its_part_row") => {
                Variant::EditTakesTheGateAfterItsPartRow
            }
            Some("audit_counter_taken_before_a_part_row") => {
                Variant::AuditCounterTakenBeforeAPartRow
            }
            Some(other) => panic!("the model locks has no variant {other}"),
        }
    }
}

/// A lock a transaction asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lock {
    /// The gate row; a beat asks for it with `SKIP LOCKED`, every other kind waits.
    Gate,
    /// The `state_versions` row of a part.
    Part(Part),
    /// The `audit_counter` row.
    Audit,
    /// The shared fence lock: never waits.
    Fence,
    /// The migration lock.
    Migrate,
}

/// The parts, in the order of the design's list, which is the order of their locks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Desired,
    Ops,
    Accounts,
    Layout,
    Verdicts,
    Enrollment,
}

impl Lock {
    /// The slot of an exclusive lock (the gate, the six part rows, the audit counter row, the
    /// migration lock); `None` for the shared fence lock.
    fn slot(self) -> Option<usize> {
        match self {
            Lock::Gate => Some(0),
            Lock::Part(p) => Some(1 + p as usize),
            Lock::Audit => Some(7),
            Lock::Migrate => Some(8),
            Lock::Fence => None,
        }
    }
}

/// The kinds of transaction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Decide,
    EditDesired,
    EditOps,
    ForceOrRetire,
    EditAccounts,
    EditUserWithToken,
    ApproveEnrollment,
    EditLayout,
    Verdict,
    MigrationRunner,
}

const KINDS: [Kind; 10] = [
    Kind::Decide,
    Kind::EditDesired,
    Kind::EditOps,
    Kind::ForceOrRetire,
    Kind::EditAccounts,
    Kind::EditUserWithToken,
    Kind::ApproveEnrollment,
    Kind::EditLayout,
    Kind::Verdict,
    Kind::MigrationRunner,
];

impl Kind {
    /// The locks of a transaction of this kind, in the order it asks for them.
    fn plan(self, variant: Variant) -> Vec<Lock> {
        use Lock::{Audit, Fence, Gate, Migrate, Part as P};
        let mut plan = match self {
            Kind::Decide => vec![Gate, P(Part::Ops), Audit],
            Kind::EditDesired => vec![Gate, P(Part::Desired), Audit],
            Kind::EditOps => vec![Gate, P(Part::Ops), Audit],
            Kind::ForceOrRetire => vec![Gate, P(Part::Desired), P(Part::Ops), Audit],
            Kind::EditAccounts => vec![P(Part::Accounts), Audit],
            Kind::EditUserWithToken => vec![P(Part::Accounts), P(Part::Enrollment), Audit],
            Kind::ApproveEnrollment => vec![P(Part::Enrollment), Audit],
            Kind::EditLayout => vec![Gate, P(Part::Layout), Audit],
            Kind::Verdict => vec![Gate, P(Part::Verdicts), Audit],
            Kind::MigrationRunner => vec![Fence, Migrate],
        };
        let gate_edit = matches!(
            self,
            Kind::EditDesired | Kind::EditOps | Kind::EditLayout | Kind::Verdict
        );
        if gate_edit && variant == Variant::EditTakesTheGateAfterItsPartRow {
            plan.swap(0, 1);
        }
        let edits_accounts = matches!(self, Kind::EditAccounts | Kind::EditUserWithToken);
        if edits_accounts && variant == Variant::AuditCounterTakenBeforeAPartRow {
            plan.retain(|l| *l != Audit);
            plan.insert(0, Audit);
        }
        plan
    }
}

/// An instance: idle, or in a transaction that has taken the first `taken` locks of its plan.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Inst {
    tx: Option<(Kind, u8)>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    insts: Vec<Inst>,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Start { inst: usize, kind: Kind },
    Take { inst: usize, lock: Lock },
    BeatFindsTheGateHeldAndGivesUp(usize),
    Commit(usize),
}

pub struct Locks {
    n: usize,
    variant: Variant,
}

impl Locks {
    fn new(n: usize, variant: Option<&str>) -> Locks {
        Locks {
            n,
            variant: Variant::of(variant),
        }
    }

    /// The instance that holds the exclusive lock in `slot`, if any.
    fn holder(&self, s: &State, slot: usize) -> Option<usize> {
        s.insts.iter().position(|inst| {
            inst.tx.is_some_and(|(kind, taken)| {
                kind.plan(self.variant)[..usize::from(taken)]
                    .iter()
                    .any(|l| l.slot() == Some(slot))
            })
        })
    }

    /// The lock the instance's next step asks for, if it has one.
    fn wanted(&self, inst: &Inst) -> Option<Lock> {
        let (kind, taken) = inst.tx?;
        kind.plan(self.variant).get(usize::from(taken)).copied()
    }

    /// The instance whose lock the instance `i` waits for. A beat's request for the gate never
    /// waits.
    fn waits_for(&self, s: &State, i: usize) -> Option<usize> {
        let lock = self.wanted(&s.insts[i])?;
        let (kind, _) = s.insts[i].tx?;
        if kind == Kind::Decide && lock == Lock::Gate {
            return None;
        }
        self.holder(s, lock.slot()?)
    }
}

impl Model for Locks {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            insts: vec![Inst { tx: None }; self.n],
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        for (i, inst) in s.insts.iter().enumerate() {
            let Some((kind, taken)) = inst.tx else {
                for kind in KINDS {
                    let mut t = s.clone();
                    t.insts[i].tx = Some((kind, 0));
                    out.push((Step::Start { inst: i, kind }, t));
                }
                continue;
            };
            let Some(lock) = self.wanted(inst) else {
                let mut t = s.clone();
                t.insts[i].tx = None;
                out.push((Step::Commit(i), t));
                continue;
            };
            let free = lock
                .slot()
                .is_none_or(|slot| self.holder(s, slot).is_none());
            if free {
                let mut t = s.clone();
                t.insts[i].tx = Some((kind, taken + 1));
                out.push((Step::Take { inst: i, lock }, t));
            } else if kind == Kind::Decide && lock == Lock::Gate {
                let mut t = s.clone();
                t.insts[i].tx = None;
                out.push((Step::BeatFindsTheGateHeldAndGivesUp(i), t));
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![Invariant {
            name: "lock_order_has_no_cycle",
            check: Check::Always(|m: &Locks, s: &State| {
                for start in 0..s.insts.len() {
                    let mut at = start;
                    for _ in 0..s.insts.len() {
                        match m.waits_for(s, at) {
                            Some(next) => at = next,
                            None => break,
                        }
                        if at == start {
                            return Some(format!(
                                "instance {start} waits, through the transactions it waits for, \
                                 on a lock it holds itself"
                            ));
                        }
                    }
                }
                None
            }),
        }]
    }
}
