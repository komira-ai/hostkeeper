//! The model `periodic`: periodic work without a leader (design section 9): the claim of a job
//! by advancing its due time in one statement, then the scan and its effect, a conditional
//! write.
//!
//! Rows: the job's `next_run` (a period number: the job is due in every period from it on), the
//! `alerts` row of the alert (present or not; the alert condition holds from the start, so every
//! run of the scan finds it), and, as ghosts, the claims made in each period and the `alert=`
//! lines logged. Time is the current period, 0, 1 or 2, and ends at 2.
//!
//! Each instance ticks without end. A tick is the claim, one statement: if `next_run` is not
//! after the current period it advances `next_run` to the next period and returns the job
//! (`FOR UPDATE SKIP LOCKED` and the re-check of a waiting `UPDATE` both leave exactly one
//! claimer of a due row, and the statement is one step). The claimer then runs the scan and its
//! effect as a second step, `INSERT INTO alerts ... ON CONFLICT DO NOTHING RETURNING`: the
//! instance whose insert returns a row logs the line. The scan only reads rows that nothing
//! else here changes, so it is part of the effect step. The insert and the log line are one step
//! too: a death between them, which would lose the line, is not modelled, because the invariant
//! is at most once, not at least once. One instance may die once after
//! claiming and before its effect; it then comes back as a fresh instance and ticks again.
//!
//! Time moves to the next period only after some instance has attempted a claim in the current
//! one: instances tick far more often than the period, so every period sees at least one tick
//! before the next begins. A run is far shorter than a period too: time does not move while an
//! instance is between the statements of a claim or between its claim and its effect (it moves
//! again once that instance has finished or died).

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "periodic",
    bound: "N instances ticking without end; one job with an alert condition that holds from the \
            start; periods 0, 1 and 2, time moving on after the first claim attempt of a \
            period; one claimer dies once after claiming",
    invariants: &[
        "periodic_job_runs_once_per_period_with_n_claimers",
        "claimer_death_loses_one_run_only",
        "alert_is_logged_once",
    ],
    cases: &[
        VariantCase {
            variant: "claim_reads_then_writes",
            breaks: "periodic_job_runs_once_per_period_with_n_claimers",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "claim_reads_then_writes",
            breaks: "alert_is_logged_once",
            fails_from: None,
        },
        VariantCase {
            variant: "claim_held_until_the_run_ends",
            breaks: "claimer_death_loses_one_run_only",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "alert_logged_on_every_run",
            breaks: "alert_is_logged_once",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Periodic::new(n, variant), only)
}

/// The last period.
const LAST: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The claim is a `SELECT` of the due row, then an unconditional `UPDATE` of `next_run` as a
    /// second statement.
    ClaimReadsThenWrites,
    /// The claim is a lease: a running flag is set at the claim and cleared after the run, a
    /// tick skips the job while it is set, and it has no expiry.
    ClaimHeldUntilTheRunEnds,
    /// The effect is an unconditional insert and log line on every run.
    AlertLoggedOnEveryRun,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("claim_reads_then_writes") => Variant::ClaimReadsThenWrites,
            Some("claim_held_until_the_run_ends") => Variant::ClaimHeldUntilTheRunEnds,
            Some("alert_logged_on_every_run") => Variant::AlertLoggedOnEveryRun,
            Some(other) => panic!("the model periodic has no variant {other}"),
        }
    }
}

/// Where an instance is in its tick.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// The `SELECT` of the variant saw the job due; the `UPDATE` is not sent yet.
    SawDue,
    /// The claim returned the job; the scan and effect are next.
    Claimed,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// The job's `next_run`.
    next_run: u8,
    /// The lease flag of the variant that holds the claim until the run ends.
    running: bool,
    /// Whether the `alerts` row is present.
    alert: bool,
    /// The current period.
    period: u8,
    /// Whether some instance has attempted a claim in the current period.
    attempted: bool,
    insts: Vec<Inst>,
    /// Ghost: the claims made in each period (counted up to 2).
    claims: [u8; LAST + 1],
    /// Ghost: the `alert=` lines logged (counted up to 2).
    logged: u8,
    /// Ghost: the periods in which a claimer died.
    died_in: [bool; LAST + 1],
    /// Whether the one death of a claimer is still available.
    death_left: bool,
}

/// One step of the model, named by the instance that takes it.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    /// The claim statement found the job due and advanced `next_run`.
    Claim(usize),
    /// The claim statement found the job not due, or held, and returned nothing.
    ClaimSkipped(usize),
    /// The `SELECT` of the read-then-write claim.
    Select(usize),
    /// The unconditional `UPDATE` of the read-then-write claim.
    Update(usize),
    /// The scan and the conditional insert, and the log line of the inserter.
    Effect(usize),
    /// The instance dies after its claim and returns as a fresh one.
    Die(usize),
    /// The current period ends and the next begins.
    TimeMoves,
}

pub struct Periodic {
    n: usize,
    variant: Variant,
}

impl Periodic {
    fn new(n: usize, variant: Option<&str>) -> Periodic {
        Periodic {
            n,
            variant: Variant::of(variant),
        }
    }

    /// The claim: advance `next_run`, set the lease flag (`lease`, for the lease variant) and
    /// count the claim in the current period.
    fn take(s: &mut State, i: usize, lease: bool) {
        let p = usize::from(s.period);
        s.next_run = s.period + 1;
        s.running = lease;
        s.claims[p] = s.claims[p].saturating_add(1).min(2);
        s.insts[i].pc = Pc::Claimed;
    }
}

impl Model for Periodic {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            next_run: 0,
            running: false,
            alert: false,
            period: 0,
            attempted: false,
            insts: vec![Inst { pc: Pc::Idle }; self.n],
            claims: [0; LAST + 1],
            logged: 0,
            died_in: [false; LAST + 1],
            death_left: true,
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        let busy = s.insts.iter().any(|i| i.pc != Pc::Idle);
        if s.attempted && !busy && usize::from(s.period) < LAST {
            let mut t = s.clone();
            t.period += 1;
            t.attempted = false;
            out.push((Step::TimeMoves, t));
        }
        for (i, inst) in s.insts.iter().enumerate() {
            let due = s.next_run <= s.period;
            match inst.pc {
                Pc::Idle => {
                    let mut t = s.clone();
                    t.attempted = true;
                    match self.variant {
                        Variant::ClaimReadsThenWrites => {
                            if due {
                                t.insts[i].pc = Pc::SawDue;
                            }
                            out.push((Step::Select(i), t));
                        }
                        Variant::ClaimHeldUntilTheRunEnds if due && !s.running => {
                            Self::take(&mut t, i, true);
                            out.push((Step::Claim(i), t));
                        }
                        Variant::Locked | Variant::AlertLoggedOnEveryRun if due => {
                            Self::take(&mut t, i, false);
                            out.push((Step::Claim(i), t));
                        }
                        _ => {
                            if t != *s {
                                out.push((Step::ClaimSkipped(i), t));
                            }
                        }
                    }
                }
                Pc::SawDue => {
                    let mut t = s.clone();
                    Self::take(&mut t, i, false);
                    out.push((Step::Update(i), t));
                }
                Pc::Claimed => {
                    let mut t = s.clone();
                    let inserted = !t.alert;
                    t.alert = true;
                    if inserted || self.variant == Variant::AlertLoggedOnEveryRun {
                        t.logged = t.logged.saturating_add(1).min(2);
                    }
                    t.running = false;
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::Effect(i), t));
                    if s.death_left {
                        // The instance dies and returns as a fresh one; a lease it held stays.
                        let mut t = s.clone();
                        t.death_left = false;
                        t.died_in[usize::from(s.period)] = true;
                        t.insts[i].pc = Pc::Idle;
                        out.push((Step::Die(i), t));
                    }
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "periodic_job_runs_once_per_period_with_n_claimers",
                check: Check::Always(|_, s: &State| {
                    s.claims
                        .iter()
                        .position(|c| *c > 1)
                        .map(|p| format!("the job was claimed twice in period {p}"))
                }),
            },
            Invariant {
                name: "claimer_death_loses_one_run_only",
                check: Check::Always(|_, s: &State| {
                    (0..usize::from(s.period))
                        .find(|p| s.claims[*p] == 0 && !s.died_in[*p])
                        .map(|p| {
                            format!("period {p} is over, the job did not run in it and no claimer died in it")
                        })
                }),
            },
            Invariant {
                name: "alert_is_logged_once",
                check: Check::Always(|_, s: &State| {
                    (s.logged > 1).then(|| format!("the alert= line was logged {} times", s.logged))
                }),
            },
        ]
    }
}
