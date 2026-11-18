//! The model `quiet`: the cluster's quiet rule after a gap in the beats (design sections 5.1
//! and 5.3): the gap rule inside the beat's one statement, and the deciding transaction that
//! reads `cluster_quiet` after it holds the gate.
//!
//! Rows: `fleet_hosts` of hosts `h0` and `h1` (each absent, stale or fresh), `cluster_quiet`
//! (in the future or not; time is abstract), the halts (an operator's halt, and a halt raised on
//! a threshold, which comes with a rollback), the orders, and a sentinel row that only the
//! variant keyed on it reads and writes. Ghost flags record what the committed decisions did.
//! `beaten` (which hosts have beaten since the last gap) is also the model's abstraction of the
//! quiet window: `cluster_quiet` ends only once every host has beaten since the last gap, which
//! is the assumption that `--quiet-secs` is longer than a beat period, and a decision is "in the
//! gap" while some host has not beaten.
//!
//! Each instance takes beats of either host one after another, forever. A beat is its one
//! statement (read `max(seen_unix)` as the snapshot has it, before the beat's own upsert; when
//! it is older than `DOWN_AFTER` or the table is empty, set `cluster_quiet`; upsert the host
//! fresh), then the deciding transaction, one step per statement: ask for the gate with `SKIP
//! LOCKED` (busy: no decision of its own), read `cluster_quiet`, read the halts and the fleet
//! and decide, write and `COMMIT`. A decision that reads a stale or absent host and is not held
//! back may either order or, as the host "did not come back", raise the threshold halt and roll
//! back; the rollout's own inputs are not modelled, so both are taken. The halts and the fleet
//! are read in one step because the gate keeps every halt writer out between that read and the
//! commit, and a host's row only gets fresher except when it goes silent, which is a step of
//! its own. An operator raises a halt once, under the gate; it is never held back by the quiet
//! rule, and every decision reads it.
//!
//! The environment makes up to two gaps, each of a kind: the database crashes and its
//! `fleet_hosts` is empty again (the sentinel row is gone), or Postgres restarts cleanly or is
//! out for longer than `DOWN_AFTER` (the rows are kept and all stale; the sentinel row is
//! there): the two leave the same rows, so they are one step. A gap can happen at any moment
//! and aborts every transaction in flight: each instance is idle again and the gate is free. The
//! model starts at the first start, with the table empty. A fresh host may also go silent
//! (stale) at any moment; it is not forgotten by `beaten`, so a threshold halt on a host that
//! stays silent after the quiet window is allowed, and one decided while some host has not
//! beaten since the last gap is not. A decision is judged by the fleet it read.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "quiet",
    bound: "N instances, each taking beats of either of 2 hosts without end and deciding after \
            each beat; one operator halt; up to 2 gaps in the beats, each a database crash or \
            a restart or outage, which abort what is in flight; a fresh host may go silent",
    invariants: &[
        "no_order_after_a_gap_in_beats_until_quiet_ends",
        "partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing",
        "operator_halt_in_quiet_window_is_read_by_every_decision",
        "quiet_ends_once_every_host_has_beaten",
        "a_decision_after_the_operator_halt_can_still_wait_on_it",
    ],
    cases: &[
        VariantCase {
            variant: "quiet_keyed_on_a_missing_sentinel",
            breaks: "partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "quiet_keyed_on_a_missing_sentinel",
            breaks: "no_order_after_a_gap_in_beats_until_quiet_ends",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "gap_rule_reads_its_own_upsert",
            breaks: "no_order_after_a_gap_in_beats_until_quiet_ends",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "gap_rule_reads_its_own_upsert",
            breaks: "partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "quiet_gates_orders_only",
            breaks: "partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "operator_halt_gated_by_the_quiet_rule",
            breaks: "operator_halt_in_quiet_window_is_read_by_every_decision",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "decision_ignores_the_operator_halt",
            breaks: "operator_halt_in_quiet_window_is_read_by_every_decision",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "decision_ignores_the_operator_halt",
            breaks: "a_decision_after_the_operator_halt_can_still_wait_on_it",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "quiet_never_ends",
            breaks: "quiet_ends_once_every_host_has_beaten",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Quiet::new(n, variant), only)
}

const HOSTS: usize = 2;
const GAPS: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The beat sets `cluster_quiet` only when a sentinel row is missing (a crash empties it);
    /// the beat writes the sentinel after.
    QuietKeyedOnAMissingSentinel,
    /// The beat reads `max(seen_unix)` after its own upsert, so it always sees a fresh row.
    GapRuleReadsItsOwnUpsert,
    /// Only an order waits for the quiet window; a threshold halt and a rollback do not.
    QuietGatesOrdersOnly,
    /// The operator's halt is refused while `cluster_quiet` is in the future.
    OperatorHaltGatedByTheQuietRule,
    /// `cluster_quiet` is never ended: nothing clears it once every host has beaten.
    QuietNeverEnds,
    /// A decision does not read the operator's halt.
    DecisionIgnoresTheOperatorHalt,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("quiet_keyed_on_a_missing_sentinel") => Variant::QuietKeyedOnAMissingSentinel,
            Some("gap_rule_reads_its_own_upsert") => Variant::GapRuleReadsItsOwnUpsert,
            Some("quiet_gates_orders_only") => Variant::QuietGatesOrdersOnly,
            Some("quiet_never_ends") => Variant::QuietNeverEnds,
            Some("decision_ignores_the_operator_halt") => Variant::DecisionIgnoresTheOperatorHalt,
            Some("operator_halt_gated_by_the_quiet_rule") => {
                Variant::OperatorHaltGatedByTheQuietRule
            }
            Some(other) => panic!("the model quiet has no variant {other}"),
        }
    }
}

/// A row of `fleet_hosts`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Seen {
    Absent,
    /// Older than `DOWN_AFTER`.
    Stale,
    Fresh,
}

/// What a decision chose.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Choice {
    /// No decision of its own.
    Wait,
    /// No decision of its own, because the threshold halt stands.
    WaitOnThresholdHalt,
    /// No decision of its own, because the operator's halt stands.
    WaitOnOperatorHalt,
    Order,
    /// The threshold halt (a host "did not come back") and the rollback, in one transaction.
    Verdict,
}

/// Where an instance is in its beat.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// The beat statement is done; the gate is not asked for yet.
    Beaten,
    /// The gate is granted; nothing read yet.
    Holding,
    /// `cluster_quiet` is read; `quiet` is what it said.
    ReadQuiet {
        quiet: bool,
    },
    /// The halts and the fleet are read and the decision made. `partial`: some host had not beaten since the last gap at the read.
    Decided {
        choice: Choice,
        partial: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Operator {
    /// Has not asked for its halt yet.
    Pending,
    Halted,
    Refused,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Rows {
    hosts: [Seen; HOSTS],
    /// `cluster_quiet.until_unix` is in the future.
    quiet: bool,
    /// The sentinel row, kept only by the variant that reads it.
    sentinel: bool,
    operator_halt: bool,
    threshold_halt: bool,
    orders: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    rows: Rows,
    /// The instance holding the gate row.
    gate: Option<u8>,
    insts: Vec<Pc>,
    operator: Operator,
    gaps_left: u8,
    /// Which hosts have beaten since the last gap.
    beaten: [bool; HOSTS],
    /// A decision that read the operator's halt committed without a decision of its own.
    waited_on_operator_halt: bool,
    /// A committed order was decided in a gap.
    order_in_gap: bool,
    /// A committed threshold halt and rollback was decided in a gap.
    verdict_in_gap: bool,
    /// An order committed while the operator's halt was there.
    order_after_halt: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Beat { inst: usize, host: usize },
    GateGranted(usize),
    GateBusyNoDecision(usize),
    ReadQuiet(usize),
    Decide(usize),
    Commit(usize),
    OperatorHalts,
    OperatorHaltRefused,
    HostGoesSilent(usize),
    DatabaseCrash,
    RestartOrOutage,
    QuietEnds,
}

pub struct Quiet {
    n: usize,
    variant: Variant,
}

impl Quiet {
    fn new(n: usize, variant: Option<&str>) -> Quiet {
        Quiet {
            n,
            variant: Variant::of(variant),
        }
    }

    /// What a decision may choose over the rows it read.
    fn choices(&self, rows: &Rows, quiet: bool) -> Vec<Choice> {
        let reads_operator_halt = self.variant != Variant::DecisionIgnoresTheOperatorHalt;
        if rows.operator_halt && reads_operator_halt {
            return vec![Choice::WaitOnOperatorHalt];
        }
        if rows.threshold_halt {
            return vec![Choice::WaitOnThresholdHalt];
        }
        let missing = rows.hosts.iter().any(|h| *h != Seen::Fresh);
        let verdict = if missing { Some(Choice::Verdict) } else { None };
        let mut out = Vec::new();
        if quiet {
            out.push(Choice::Wait);
            if self.variant == Variant::QuietGatesOrdersOnly {
                out.extend(verdict);
            }
        } else {
            out.push(Choice::Order);
            out.extend(verdict);
        }
        out
    }
}

impl Model for Quiet {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            rows: Rows {
                hosts: [Seen::Absent; HOSTS],
                quiet: false,
                sentinel: false,
                operator_halt: false,
                threshold_halt: false,
                orders: false,
            },
            gate: None,
            insts: vec![Pc::Idle; self.n],
            operator: Operator::Pending,
            gaps_left: GAPS,
            beaten: [false; HOSTS],
            waited_on_operator_halt: false,
            order_in_gap: false,
            verdict_in_gap: false,
            order_after_halt: false,
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        let all_beaten = s.beaten.iter().all(|b| *b);
        if s.rows.quiet && all_beaten && self.variant != Variant::QuietNeverEnds {
            let mut t = s.clone();
            t.rows.quiet = false;
            out.push((Step::QuietEnds, t));
        }
        if s.gate.is_none() && s.operator == Operator::Pending {
            let mut t = s.clone();
            if self.variant == Variant::OperatorHaltGatedByTheQuietRule && s.rows.quiet {
                t.operator = Operator::Refused;
                out.push((Step::OperatorHaltRefused, t));
            } else {
                t.operator = Operator::Halted;
                t.rows.operator_halt = true;
                out.push((Step::OperatorHalts, t));
            }
        }
        for h in 0..HOSTS {
            if s.rows.hosts[h] == Seen::Fresh {
                let mut t = s.clone();
                t.rows.hosts[h] = Seen::Stale;
                out.push((Step::HostGoesSilent(h), t));
            }
        }
        if s.gaps_left > 0 {
            let mut aborted = s.clone();
            aborted.gate = None;
            aborted.insts = vec![Pc::Idle; self.n];
            let s = &aborted;
            let mut t = s.clone();
            t.gaps_left -= 1;
            t.beaten = [false; HOSTS];
            t.rows.hosts = [Seen::Absent; HOSTS];
            t.rows.sentinel = false;
            out.push((Step::DatabaseCrash, t));
            let mut t = s.clone();
            t.gaps_left -= 1;
            t.beaten = [false; HOSTS];
            for h in &mut t.rows.hosts {
                if *h == Seen::Fresh {
                    *h = Seen::Stale;
                }
            }
            out.push((Step::RestartOrOutage, t));
        }
        for (i, pc) in s.insts.iter().enumerate() {
            match *pc {
                Pc::Idle => {
                    for host in 0..HOSTS {
                        let mut t = s.clone();
                        let rows = &mut t.rows;
                        let gap = match self.variant {
                            Variant::QuietKeyedOnAMissingSentinel => !s.rows.sentinel,
                            // Read after the beat's own upsert: that host is fresh.
                            Variant::GapRuleReadsItsOwnUpsert => false,
                            _ => s.rows.hosts.iter().all(|h| *h != Seen::Fresh),
                        };
                        if gap {
                            rows.quiet = true;
                        }
                        rows.hosts[host] = Seen::Fresh;
                        if self.variant == Variant::QuietKeyedOnAMissingSentinel {
                            rows.sentinel = true;
                        }
                        t.beaten[host] = true;
                        t.insts[i] = Pc::Beaten;
                        out.push((Step::Beat { inst: i, host }, t));
                    }
                }
                Pc::Beaten => {
                    let mut t = s.clone();
                    if s.gate.is_none() {
                        t.gate = Some(i as u8);
                        t.insts[i] = Pc::Holding;
                        out.push((Step::GateGranted(i), t));
                    } else {
                        // SKIP LOCKED: no row back, no decision of its own.
                        t.insts[i] = Pc::Idle;
                        out.push((Step::GateBusyNoDecision(i), t));
                    }
                }
                Pc::Holding => {
                    let mut t = s.clone();
                    t.insts[i] = Pc::ReadQuiet {
                        quiet: s.rows.quiet,
                    };
                    out.push((Step::ReadQuiet(i), t));
                }
                Pc::ReadQuiet { quiet } => {
                    let partial = !all_beaten;
                    for choice in self.choices(&s.rows, quiet) {
                        let mut t = s.clone();
                        t.insts[i] = Pc::Decided { choice, partial };
                        out.push((Step::Decide(i), t));
                    }
                }
                Pc::Decided { choice, partial } => {
                    let mut t = s.clone();
                    match choice {
                        Choice::Wait => {}
                        Choice::WaitOnThresholdHalt => {}
                        Choice::WaitOnOperatorHalt => t.waited_on_operator_halt = true,
                        Choice::Order => {
                            t.rows.orders = true;
                            t.order_in_gap |= partial;
                            t.order_after_halt |= s.rows.operator_halt;
                        }
                        Choice::Verdict => {
                            t.rows.threshold_halt = true;
                            t.verdict_in_gap |= partial;
                        }
                    }
                    t.gate = None;
                    t.insts[i] = Pc::Idle;
                    out.push((Step::Commit(i), t));
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "quiet_ends_once_every_host_has_beaten",
                check: Check::Reachable(|_, s: &State| !s.rows.quiet),
            },
            Invariant {
                name: "a_decision_after_the_operator_halt_can_still_wait_on_it",
                check: Check::Reachable(|_, s: &State| {
                    s.rows.operator_halt && s.waited_on_operator_halt
                }),
            },
            Invariant {
                name: "no_order_after_a_gap_in_beats_until_quiet_ends",
                check: Check::Always(|_, s: &State| {
                    s.order_in_gap.then(|| {
                        "an order committed while some host had not beaten since the last gap"
                            .to_string()
                    })
                }),
            },
            Invariant {
                name: "partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing",
                check: Check::Always(|_, s: &State| {
                    s.verdict_in_gap.then(|| {
                        "a threshold halt and rollback committed while some host had not \
                         beaten since the last gap"
                            .to_string()
                    })
                }),
            },
            Invariant {
                name: "operator_halt_in_quiet_window_is_read_by_every_decision",
                check: Check::Always(|_, s: &State| {
                    if s.operator == Operator::Refused {
                        Some("the operator's halt was refused while quiet".to_string())
                    } else if s.order_after_halt {
                        Some("an order committed after the operator's halt".to_string())
                    } else {
                        None
                    }
                }),
            },
        ]
    }
}
