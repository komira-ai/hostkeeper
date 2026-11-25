//! The model `ledger`: a commit of unknown outcome and its settle (design section 4).
//!
//! Rows: `tx_ledger` (the committed row of each instance's first id, with its unique index), the
//! counter row and the audit lines (one per instance). Each of `N` instances has one change to
//! make, a closure that increments the counter and writes one audit line, keyed by the
//! instance's own ledger id. The writes of a transaction are invisible until it commits and the
//! increments commute, so the row lock of the counter changes no outcome: the closure is the one
//! step `Write`.
//!
//! An attempt is a transaction on a backend: `INSERT tx_ledger (id)` as its first statement, the
//! write, `COMMIT`. The first attempt's `COMMIT` may come back of unknown outcome (the
//! connection drops): the instance then has no answer, and the old backend is a process of its
//! own that is still in progress and later commits or rolls back, at any moment. Each instance
//! loses at most one commit. The instance revives with a new session and settles by inserting
//! its id again with `ON CONFLICT DO NOTHING`: that insert waits for the old backend while it is
//! in progress (the unique index), and then inserts no row if the first attempt committed (it
//! landed) or inserts one, which fences the id, if it rolled back (it did not land). The outcome
//! goes to the caller, and only when it is "did not land" does the caller re-issue the change as
//! a new request, a new attempt with a new id, whose `COMMIT` is known.
//!
//! This is the settle as built: the outcome goes to the caller and the closure is not retried by
//! the same id. The re-issue is the caller's own new request; it is modelled so that the goal of
//! exactly one landing of each change can be checked. A session lost during the settle or during
//! the re-issue's commit is out of bound. A first `COMMIT` that never reached the server is the
//! step `BackendRollsBack`.
//!
//! The ids of new attempts never meet another row, so their inserts are one step with no
//! conflict. The old backends are environment processes; they are not counted in N.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "ledger",
    bound: "N instances, each with one change (increment a counter, write one audit line) and \
            one lost commit; the old backend commits or rolls back at any moment; one \
            re-issue by new request per instance",
    invariants: &[
        "unknown_outcome_commit_lands_once_by_retry",
        "settled_outcome_is_what_landed",
    ],
    cases: &[
        VariantCase {
            variant: "lookup_settle",
            breaks: "unknown_outcome_commit_lands_once_by_retry",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "lookup_settle",
            breaks: "settled_outcome_is_what_landed",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "retry_with_a_new_ledger_id",
            breaks: "unknown_outcome_commit_lands_once_by_retry",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Ledger::new(n, variant), only)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The settle is a `SELECT` of the ledger id: it does not wait for an in-progress commit,
    /// which it cannot see.
    LookupSettle,
    /// The revival retries the closure at once in a new transaction with a new id, and does not
    /// settle the first attempt.
    RetryWithANewLedgerId,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("lookup_settle") => Variant::LookupSettle,
            Some("retry_with_a_new_ledger_id") => Variant::RetryWithANewLedgerId,
            Some(other) => panic!("the model ledger has no variant {other}"),
        }
    }
}

/// How far a transaction on a backend has come.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Stage {
    /// Not begun.
    None,
    /// The ledger id is inserted (not committed).
    Ledgered,
    /// The closure's writes are sent (not committed).
    Written,
    Committed,
    RolledBack,
}

/// Where an instance is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    /// Running the first attempt (its stage is `Inst::first`); at `Written` the `COMMIT` is next.
    First,
    /// The `COMMIT` came back of unknown outcome: the session is gone.
    Lost,
    /// A new session is up; the first attempt is not settled.
    Revived,
    /// Running a new attempt, with a new id, in this stage.
    Again(Stage),
    Done,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
    /// The first attempt's transaction, on its backend.
    first: Stage,
    /// What the caller was told about the first attempt: whether it landed.
    answer: Option<bool>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// The committed row of each instance's first id in `tx_ledger`.
    ledger: Vec<bool>,
    counter: u8,
    /// The audit lines of each instance's change.
    audit: Vec<u8>,
    insts: Vec<Inst>,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    InsertLedger(usize),
    Write(usize),
    CommitAnswered(usize),
    CommitLost(usize),
    Revive(usize),
    Settle(usize),
    BackendCommits(usize),
    BackendRollsBack(usize),
}

pub struct Ledger {
    n: usize,
    variant: Variant,
}

impl Ledger {
    fn new(n: usize, variant: Option<&str>) -> Ledger {
        Ledger {
            n,
            variant: Variant::of(variant),
        }
    }
}

/// The effect of a commit of an instance's change.
fn apply(s: &mut State, i: usize) {
    s.counter = s.counter.saturating_add(1);
    s.audit[i] = s.audit[i].saturating_add(1);
}

impl Model for Ledger {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            ledger: vec![false; self.n],
            counter: 0,
            audit: vec![0; self.n],
            insts: vec![
                Inst {
                    pc: Pc::First,
                    first: Stage::None,
                    answer: None,
                };
                self.n
            ],
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        for (i, inst) in s.insts.iter().enumerate() {
            // The old backend: in progress while the instance is not sending its `COMMIT`.
            if inst.first == Stage::Written && inst.pc != Pc::First {
                let mut t = s.clone();
                t.insts[i].first = Stage::Committed;
                t.ledger[i] = true;
                apply(&mut t, i);
                out.push((Step::BackendCommits(i), t));
                let mut t = s.clone();
                t.insts[i].first = Stage::RolledBack;
                out.push((Step::BackendRollsBack(i), t));
            }
            match inst.pc {
                Pc::First => match inst.first {
                    Stage::None => {
                        let mut t = s.clone();
                        t.insts[i].first = Stage::Ledgered;
                        out.push((Step::InsertLedger(i), t));
                    }
                    Stage::Ledgered => {
                        let mut t = s.clone();
                        t.insts[i].first = Stage::Written;
                        out.push((Step::Write(i), t));
                    }
                    Stage::Written => {
                        let mut t = s.clone();
                        t.insts[i].first = Stage::Committed;
                        t.insts[i].answer = Some(true);
                        t.insts[i].pc = Pc::Done;
                        t.ledger[i] = true;
                        apply(&mut t, i);
                        out.push((Step::CommitAnswered(i), t));
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Lost;
                        out.push((Step::CommitLost(i), t));
                    }
                    Stage::Committed | Stage::RolledBack => {}
                },
                Pc::Lost => {
                    let mut t = s.clone();
                    t.insts[i].pc = if self.variant == Variant::RetryWithANewLedgerId {
                        Pc::Again(Stage::None)
                    } else {
                        Pc::Revived
                    };
                    out.push((Step::Revive(i), t));
                }
                Pc::Revived => {
                    let resolved = matches!(inst.first, Stage::Committed | Stage::RolledBack);
                    // The insert waits for the old backend while it is in progress; the
                    // lookup does not wait.
                    if resolved || self.variant == Variant::LookupSettle {
                        let mut t = s.clone();
                        let landed = s.ledger[i];
                        if self.variant != Variant::LookupSettle && !landed {
                            // No row was there: the insert adds one, which fences the id.
                            t.ledger[i] = true;
                        }
                        t.insts[i].answer = Some(landed);
                        t.insts[i].pc = if landed {
                            Pc::Done
                        } else {
                            Pc::Again(Stage::None)
                        };
                        out.push((Step::Settle(i), t));
                    }
                }
                Pc::Again(stage) => {
                    let mut t = s.clone();
                    match stage {
                        Stage::None => {
                            t.insts[i].pc = Pc::Again(Stage::Ledgered);
                            out.push((Step::InsertLedger(i), t));
                        }
                        Stage::Ledgered => {
                            t.insts[i].pc = Pc::Again(Stage::Written);
                            out.push((Step::Write(i), t));
                        }
                        Stage::Written => {
                            t.insts[i].pc = Pc::Done;
                            apply(&mut t, i);
                            out.push((Step::CommitAnswered(i), t));
                        }
                        Stage::Committed | Stage::RolledBack => {}
                    }
                }
                Pc::Done => {}
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                // A change applied twice leaves no state from which every change has landed
                // exactly once, since audit lines are never removed.
                name: "unknown_outcome_commit_lands_once_by_retry",
                check: Check::Reachable(|_, s: &State| {
                    s.insts.iter().all(|inst| inst.pc == Pc::Done)
                        && s.audit.iter().all(|a| *a == 1)
                        && usize::from(s.counter) == s.audit.len()
                }),
            },
            Invariant {
                name: "settled_outcome_is_what_landed",
                check: Check::Always(|_, s: &State| {
                    s.insts.iter().enumerate().find_map(|(i, inst)| {
                        let landed = inst.first == Stage::Committed;
                        inst.answer.filter(|told| *told != landed).map(|told| {
                            format!(
                                "the caller of instance {i} was told landed = {told}, the first \
                                 attempt's transaction is {:?}",
                                inst.first
                            )
                        })
                    })
                }),
            },
        ]
    }
}
