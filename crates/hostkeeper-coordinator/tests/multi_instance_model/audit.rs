//! The model `audit`: the numbers of the audit lines (design section 6, "Audit numbers"), and the
//! two re-seeds that the start of the new release runs after the release before ran (section 6,
//! "Return after a back-out", and section 16).
//!
//! Rows: the audit lines (each line is its number; the list is in commit order), the
//! `audit_counter` row (`next`, and its row lock), and the `desired` rows of services `s1` and
//! `s2` (the version, `set_seq`, `set_for`) with the sequence `desired_seq`. Both services start
//! stamped, `s1` first (`set_seq` 1 and 2, `set_for` equal to the version). A ghost list keeps
//! the order in which the services' desired versions were last set; it is read by the invariants
//! only. The primary key on the audit number is not modelled: a number used twice is the broken
//! invariant itself (in the database that commit would fail).
//!
//! Each instance of the new release writes one audit line: its other writes (not modelled, they
//! touch no row of this model), then `UPDATE audit_counter SET next = next + 1 RETURNING next - 1`
//! as the last write (it waits while another transaction holds the row lock, and the lock is held
//! until `COMMIT`), then `COMMIT`, where the line becomes visible with its number. Then the
//! instances stop. The release before runs once, at a moment when no instance of the new release
//! runs (it holds the exclusive fence lock): in one step it writes an audit line numbered
//! `max(seq) + 1` without reading or writing the counter, and sets a new version of `s1` without
//! touching `set_seq` or `set_for`. Then every instance starts again. Its start is two
//! statements, each under the shared lock and before it serves: the counter re-seed
//! (`next = greatest(next, max(seq) + 1)`, which waits for the counter's row lock), and the queue
//! re-stamp under the gate (one transaction: the rows with `set_for` different from the version,
//! or with no `set_seq`, are given fresh numbers from `desired_seq`, raised first to the greatest
//! `set_seq`, in the order of the services; only `s1` can be changed, so the release before's own
//! queue order has one row to rank; the raise of `desired_seq` never lifts it, and no row lacks a
//! `set_seq`, so those two duties of the re-stamp are not checked). After its start, an instance writes one audit line as above
//! and sets one new desired version of a service of its choice (a transaction under the gate:
//! `set_seq` from `desired_seq`, `set_for` the new version), in either order.
//!
//! The queue order is the services ordered by `set_seq`, a missing one first. It is compared with
//! the ghost order from the moment the first instance finished its start (before the release
//! before ran, the rows are stamped in the order they were set). Instances are the processes
//! counted by N; the release before is the environment. A desired change is modelled without its
//! audit line, since the gate serializes it with the re-stamp and it touches no row of the other
//! statements. An audit transaction may also roll back after the counter `UPDATE` (a crash or a
//! stop mid-transaction): the row lock is released and `next` is restored, so the rollback leaves no
//! gap; a sequence would keep its gap, so the variant that takes numbers from a sequence has no
//! rollback, to isolate the ordering. The bound is one line per instance
//! before and after the release before, one change by the release before and one by each instance.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "audit",
    bound: "N instances each writing one audit line before and one after the release before ran, \
            which writes one line and changes the version of s1 once; each instance then sets one \
            desired version of s1 or s2; two services; the re-stamp has no row without set_seq \
            and no desired_seq behind the rows",
    invariants: &[
        "audit_numbers_follow_commit_order",
        "new_release_again_continues_audit_numbers_after_the_release_before_ran",
        "set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order",
    ],
    cases: &[
        VariantCase {
            variant: "audit_number_from_a_sequence",
            breaks: "audit_numbers_follow_commit_order",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "no_audit_counter_reseed_at_start",
            breaks: "new_release_again_continues_audit_numbers_after_the_release_before_ran",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "no_set_seq_restamp_at_start",
            breaks: "set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Audit::new(n, variant), only)
}

const SERVICES: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The number is taken from a sequence when the statement runs, with no row lock held to the
    /// commit.
    AuditNumberFromASequence,
    /// The start does not raise `audit_counter.next`.
    NoAuditCounterReseedAtStart,
    /// The start does not give fresh `set_seq` numbers to the rows the release before changed.
    NoSetSeqRestampAtStart,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("audit_number_from_a_sequence") => Variant::AuditNumberFromASequence,
            Some("no_audit_counter_reseed_at_start") => Variant::NoAuditCounterReseedAtStart,
            Some("no_set_seq_restamp_at_start") => Variant::NoSetSeqRestampAtStart,
            Some(other) => panic!("the model audit has no variant {other}"),
        }
    }

    fn row_locked(self) -> bool {
        self != Variant::AuditNumberFromASequence
    }
}

/// Where an instance is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    /// Between transactions: it can take a number, set a desired version once started, or stop
    /// after its line of round 0.
    Idle,
    /// The number is taken (the line carries it); not committed.
    Numbered(u8),
    /// Stopped, until the release before has run.
    Stopped,
    /// Started again: the counter re-seed is next.
    Reseed,
    /// The queue re-stamp is next.
    Restamp,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
    /// 0 before the release before ran, 1 after.
    round: u8,
    /// The instance wrote its audit line of this round.
    wrote: bool,
    /// The instance set its desired version (round 1).
    set: bool,
}

/// A `desired` row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Desired {
    version: u8,
    set_seq: Option<u8>,
    set_for: u8,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// The numbers of the audit lines, in commit order.
    lines: Vec<u8>,
    /// `audit_counter.next`.
    next: u8,
    /// The instance holding the counter's row lock.
    holder: Option<u8>,
    desired: [Desired; SERVICES],
    /// The last number of `desired_seq`.
    desired_seq: u8,
    /// Ghost: the services in the order their desired versions were last set.
    set_order: Vec<u8>,
    /// The release before has run.
    release_ran: bool,
    /// Ghost: the position in `lines` of the release before's line.
    release_line: Option<usize>,
    /// Ghost: an instance has finished its start.
    started: bool,
    insts: Vec<Inst>,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    TakeNumber(usize),
    Abort(usize),
    Commit(usize),
    Stop(usize),
    ReleaseBeforeRuns,
    Reseed(usize),
    Restamp(usize),
    SetDesired { inst: usize, service: usize },
}

pub struct Audit {
    n: usize,
    variant: Variant,
}

impl Audit {
    fn new(n: usize, variant: Option<&str>) -> Audit {
        Audit {
            n,
            variant: Variant::of(variant),
        }
    }

    /// `max(seq)` of the committed lines (0 when there are none).
    fn max_seq(s: &State) -> u8 {
        s.lines.iter().copied().max().unwrap_or(0)
    }

    /// The services in queue order: by `set_seq`, a missing one first.
    fn queue(s: &State) -> Vec<u8> {
        let mut q: Vec<u8> = (0..SERVICES as u8).collect();
        q.sort_by_key(|t| (s.desired[usize::from(*t)].set_seq.unwrap_or(0), *t));
        q
    }
}

impl Model for Audit {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        let row = |seq| Desired {
            version: 1,
            set_seq: Some(seq),
            set_for: 1,
        };
        vec![State {
            lines: Vec::new(),
            next: 1,
            holder: None,
            desired: [row(1), row(2)],
            desired_seq: 2,
            set_order: vec![0, 1],
            release_ran: false,
            release_line: None,
            started: false,
            insts: vec![
                Inst {
                    pc: Pc::Idle,
                    round: 0,
                    wrote: false,
                    set: false,
                };
                self.n
            ],
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        let row_locked = self.variant.row_locked();
        for (i, inst) in s.insts.iter().enumerate() {
            let lock_free = !row_locked || s.holder.is_none();
            let reseed_free = s.holder.is_none();
            match inst.pc {
                Pc::Idle => {
                    if !inst.wrote && lock_free {
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Numbered(s.next);
                        t.next = s.next.saturating_add(1);
                        if row_locked {
                            t.holder = Some(i as u8);
                        }
                        out.push((Step::TakeNumber(i), t));
                    }
                    if inst.round == 1 && !inst.set {
                        for service in 0..SERVICES {
                            let mut t = s.clone();
                            t.desired_seq = s.desired_seq.saturating_add(1);
                            let row = &mut t.desired[service];
                            row.version = row.version.saturating_add(1);
                            row.set_seq = Some(t.desired_seq);
                            row.set_for = row.version;
                            t.set_order.retain(|x| usize::from(*x) != service);
                            t.set_order.push(service as u8);
                            t.insts[i].set = true;
                            out.push((Step::SetDesired { inst: i, service }, t));
                        }
                    }
                    if inst.wrote && inst.round == 0 {
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Stopped;
                        out.push((Step::Stop(i), t));
                    }
                }
                Pc::Numbered(number) => {
                    if row_locked {
                        let mut t = s.clone();
                        t.next = number;
                        t.holder = None;
                        t.insts[i].pc = Pc::Idle;
                        out.push((Step::Abort(i), t));
                    }
                    let mut t = s.clone();
                    t.lines.push(number);
                    if t.holder == Some(i as u8) {
                        t.holder = None;
                    }
                    t.insts[i].pc = Pc::Idle;
                    t.insts[i].wrote = true;
                    out.push((Step::Commit(i), t));
                }
                Pc::Stopped => {}
                Pc::Reseed => {
                    if reseed_free {
                        let mut t = s.clone();
                        if self.variant != Variant::NoAuditCounterReseedAtStart {
                            t.next = s.next.max(Self::max_seq(s).saturating_add(1));
                        }
                        t.insts[i].pc = Pc::Restamp;
                        out.push((Step::Reseed(i), t));
                    }
                }
                Pc::Restamp => {
                    let mut t = s.clone();
                    if self.variant != Variant::NoSetSeqRestampAtStart {
                        let top = t
                            .desired
                            .iter()
                            .filter_map(|d| d.set_seq)
                            .max()
                            .unwrap_or(0);
                        t.desired_seq = t.desired_seq.max(top);
                        for d in &mut t.desired {
                            if d.set_seq.is_none() || d.set_for != d.version {
                                t.desired_seq = t.desired_seq.saturating_add(1);
                                d.set_seq = Some(t.desired_seq);
                                d.set_for = d.version;
                            }
                        }
                    }
                    t.started = true;
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::Restamp(i), t));
                }
            }
        }
        // The release before runs when every instance has written its line and stopped.
        if !s.release_ran && s.insts.iter().all(|i| i.pc == Pc::Stopped) {
            let mut t = s.clone();
            let line = Self::max_seq(s).saturating_add(1);
            t.release_line = Some(t.lines.len());
            t.lines.push(line);
            t.desired[0].version = t.desired[0].version.saturating_add(1);
            t.set_order.retain(|x| *x != 0);
            t.set_order.push(0);
            t.release_ran = true;
            for inst in &mut t.insts {
                *inst = Inst {
                    pc: Pc::Reseed,
                    round: 1,
                    wrote: false,
                    set: false,
                };
            }
            out.push((Step::ReleaseBeforeRuns, t));
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "audit_numbers_follow_commit_order",
                check: Check::Always(|_, s: &State| {
                    s.lines
                        .iter()
                        .enumerate()
                        .position(|(k, n)| usize::from(*n) != k + 1)
                        .map(|k| {
                            format!(
                                "the line committed {}th carries {}; numbers in commit order: {:?}",
                                k + 1,
                                s.lines[k],
                                s.lines
                            )
                        })
                }),
            },
            Invariant {
                name: "new_release_again_continues_audit_numbers_after_the_release_before_ran",
                check: Check::Always(|_, s: &State| {
                    let twice = s
                        .lines
                        .iter()
                        .enumerate()
                        .find(|(k, n)| s.lines[..*k].contains(n))
                        .map(|(_, n)| format!("audit number {n} is used twice: {:?}", s.lines));
                    let below = s.release_line.and_then(|r| {
                        s.lines[r + 1..]
                            .iter()
                            .find(|n| **n <= s.lines[r])
                            .map(|n| {
                                format!(
                                    "a line after the release before's carries {n}: {:?}",
                                    s.lines
                                )
                            })
                    });
                    twice.or(below)
                }),
            },
            Invariant {
                name: "set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order",
                check: Check::Always(|_, s: &State| {
                    let queue = Audit::queue(s);
                    (s.started && queue != s.set_order).then(|| {
                        format!(
                            "queue order {queue:?} differs from the order the versions were set \
                             {:?}",
                            s.set_order
                        )
                    })
                }),
            },
        ]
    }
}
