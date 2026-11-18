//! The model `beats`: the ordered beat upsert of a host's `fleet_hosts` row (design section 5.2),
//! `INSERT ... ON CONFLICT (host_id) DO UPDATE ... WHERE excluded.sent_unix >=
//! fleet_hosts.sent_unix OR fleet_hosts.seen_unix < now() - reorder`.
//!
//! Rows: the one host's `fleet_hosts` row, as the `sent_unix` it holds and whether it is older
//! than `--beat-reorder-secs` (`seen_unix` against the window). The row exists at the start, from
//! a first beat that was sent with `sent_unix` 4.
//!
//! Processes: one host, which sends beats without end, each with the next `sent_unix` of its own
//! clock (5, then 6, and 6 again and again: the same beat re-sent, saturating to keep the state
//! finite). Its clock may step back once; the beats after that carry 1, then 2, and 2 again and
//! again, below every earlier value, and never catch up within the model. The step happens between
//! beats, with none in flight. Left out by this assumption: the row is already older than the
//! window, a beat with 5 is in flight, the clock steps back, a beat with 1 is applied first (the
//! second arm lets it in) and the delayed 5 then overwrites it, because 5 >= 1. The guard orders by
//! `sent_unix` alone, so it cannot catch that; the row is then wrong until the next beat after the
//! window. Each beat is handed to an idle instance (an agent dials a new connection per beat, so
//! any instance may hear it; an instance has at most one beat in flight). The instance applies the
//! upsert as one statement at any later time, so beats of the host reach the row in any order
//! across instances. A beat the guard drops changes nothing (its reply is outside the model).
//!
//! Time is an event that ages the stored row past the reorder window. It is enabled only when no
//! beat is in flight. This assumes that an in-flight beat is applied well within the window: a beat
//! stalled on a pool wait for longer than `--beat-reorder-secs` (default 30 s) is not modelled, and
//! the default rests on the latency of a beat being far below it. An applied beat sets `seen_unix`,
//! so the row is fresh again after it.
//!
//! The statements that run only when the upsert applied (the bake and the slot release, a
//! `RETURNING` CTE) are not modelled: they follow the row the upsert leaves, and the model
//! checks that row.
//!
//! The ghost state is the order in which the beats were sent (by rank among those in flight, and
//! a flag on each beat that a beat sent after it has been applied), since `sent_unix` alone does
//! not say which of two beats is newer once the clock has stepped back.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "beats",
    bound: "N instances; one host sending beats without end, sent values 5 then 6 repeated, \
            its clock stepping back once to 1 then 2 repeated; the row ages past the reorder \
            window only with no beat in flight",
    invariants: &[
        "stale_beat_does_not_overwrite_a_newer_one",
        "host_whose_clock_stepped_back_is_believed_again",
    ],
    cases: &[
        VariantCase {
            variant: "unguarded_beat_upsert",
            breaks: "stale_beat_does_not_overwrite_a_newer_one",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "beat_guard_without_the_reorder_arm",
            breaks: "host_whose_clock_stepped_back_is_believed_again",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Beats::new(n, variant), only)
}

/// The `sent_unix` of the beat in the row at the start.
const FIRST_SENT: u8 = 4;
/// The values the host sends before and after its clock steps back: it counts up from the first
/// to the last and stays at the last.
const BEFORE_STEP: (u8, u8) = (5, 6);
const AFTER_STEP: (u8, u8) = (1, 2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// The guard of section 5.2: `sent >= stored OR stale`.
    Locked,
    /// The update has no `WHERE`: every beat that arrives overwrites the row.
    UnguardedBeatUpsert,
    /// The update is guarded by `excluded.sent_unix >= fleet_hosts.sent_unix` alone.
    BeatGuardWithoutTheReorderArm,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("unguarded_beat_upsert") => Variant::UnguardedBeatUpsert,
            Some("beat_guard_without_the_reorder_arm") => Variant::BeatGuardWithoutTheReorderArm,
            Some(other) => panic!("the model beats has no variant {other}"),
        }
    }
}

/// Where an instance is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// A beat was handed to the instance; its upsert is not yet applied.
    Beat {
        sent: u8,
        /// The place of the beat among the beats in flight, by the order they were sent: 0 is
        /// the oldest.
        rank: u8,
        /// A beat sent after this one has been applied.
        overtaken: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// The `sent_unix` in the row.
    stored: u8,
    /// The row is older than the reorder window.
    stale: bool,
    /// The `sent_unix` of the next beat, and the last value of its lane.
    next_sent: u8,
    lane_end: u8,
    /// The `sent_unix` of the latest beat the host sent.
    last_sent: u8,
    stepped_back: bool,
    insts: Vec<Pc>,
    /// An applied beat overwrote the row of a newer beat (one sent after it) with another value.
    overwrote_newer: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Send { inst: usize, sent: u8 },
    Apply(usize),
    ClockStepsBack,
    RowAgesPastTheWindow,
}

pub struct Beats {
    n: usize,
    variant: Variant,
}

impl Beats {
    fn new(n: usize, variant: Option<&str>) -> Beats {
        Beats {
            n,
            variant: Variant::of(variant),
        }
    }

    /// Whether the statement updates the row for a beat sent with `sent`.
    fn applies(&self, s: &State, sent: u8) -> bool {
        match self.variant {
            Variant::UnguardedBeatUpsert => true,
            Variant::BeatGuardWithoutTheReorderArm => sent >= s.stored,
            Variant::Locked => sent >= s.stored || s.stale,
        }
    }
}

impl Model for Beats {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            stored: FIRST_SENT,
            stale: false,
            next_sent: BEFORE_STEP.0,
            lane_end: BEFORE_STEP.1,
            last_sent: FIRST_SENT,
            stepped_back: false,
            insts: vec![Pc::Idle; self.n],
            overwrote_newer: false,
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        let in_flight = s
            .insts
            .iter()
            .filter(|pc| matches!(pc, Pc::Beat { .. }))
            .count();
        // The host sends its next beat to any idle instance.
        for (i, pc) in s.insts.iter().enumerate() {
            if *pc == Pc::Idle {
                let mut t = s.clone();
                let sent = s.next_sent;
                t.insts[i] = Pc::Beat {
                    sent,
                    rank: u8::try_from(in_flight).unwrap_or(u8::MAX),
                    overtaken: false,
                };
                t.last_sent = sent;
                t.next_sent = (s.next_sent + 1).min(s.lane_end);
                out.push((Step::Send { inst: i, sent }, t));
            }
        }
        if !s.stepped_back && in_flight == 0 {
            let mut t = s.clone();
            t.stepped_back = true;
            t.next_sent = AFTER_STEP.0;
            t.lane_end = AFTER_STEP.1;
            out.push((Step::ClockStepsBack, t));
        }
        if in_flight == 0 && !s.stale {
            let mut t = s.clone();
            t.stale = true;
            out.push((Step::RowAgesPastTheWindow, t));
        }
        // An instance applies the upsert of its beat.
        for (i, pc) in s.insts.iter().enumerate() {
            let Pc::Beat {
                sent,
                rank,
                overtaken,
            } = *pc
            else {
                continue;
            };
            let mut t = s.clone();
            let applied = self.applies(s, sent);
            if applied {
                if overtaken && sent != s.stored {
                    t.overwrote_newer = true;
                }
                t.stored = sent;
                t.stale = false;
            }
            for (j, other) in t.insts.iter_mut().enumerate() {
                if j == i {
                    continue;
                }
                if let Pc::Beat {
                    rank: other_rank,
                    overtaken: other_overtaken,
                    ..
                } = other
                {
                    if applied && *other_rank < rank {
                        *other_overtaken = true;
                    }
                    if *other_rank > rank {
                        *other_rank -= 1;
                    }
                }
            }
            t.insts[i] = Pc::Idle;
            out.push((Step::Apply(i), t));
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "stale_beat_does_not_overwrite_a_newer_one",
                check: Check::Always(|_, s: &State| {
                    s.overwrote_newer.then(|| {
                        "a beat sent earlier overwrote the row of a beat sent later".to_string()
                    })
                }),
            },
            Invariant {
                name: "host_whose_clock_stepped_back_is_believed_again",
                check: Check::Reachable(|_, s: &State| s.stored == s.last_sent),
            },
        ]
    }
}
