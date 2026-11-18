//! The model `steps`: the rollout steps of design section 5.3 whose race is decided by a
//! conditional write: the stage advance (a compare-and-set on the stage number), the halt on a
//! threshold (`WHERE NOT halted`), and roll back first, then delete.
//!
//! Three scenarios, one initial state each, so each stays small; an invariant of another scenario
//! holds there for lack of rows.
//!
//! Advance: the plan of one service is at stage 1 of 2, stage 1 done and baked. Rows: the plan's
//! stage, the number of advances made, the number of `rollout_advance` audit lines. A deciding
//! beat that reads the stage as 1 runs `UPDATE plan SET stage = 2 WHERE stage = 1` and writes the
//! audit line only when the row count is 1. Once, an operator calls `Rollouts.Advance` with the
//! stage it saw (1): the same compare-and-set, and zero rows is `not_waiting`.
//!
//! Halt: a failed verdict has crossed the threshold. Rows: whether the service is halted, the halt
//! generation, the number of halt audit lines. A deciding beat that reads the service as not
//! halted runs `UPDATE halts SET generation = generation + 1 WHERE NOT halted` and writes the
//! audit line only when the row count is 1.
//!
//! Rollback: version 2 of a package is rolling and the package is also removed from the host's
//! group (an uninstall is wanted). Rows: the verdicts (whether a failure report is committed),
//! whether the desired row was set back to `last_good` (the rollback), whether the uninstall is
//! ordered. A failure report arrives once, in a beat, and is written as a verdict: an edit of the
//! verdicts, which takes the gate. A deciding beat reads the verdict and the desired row, then
//! the pure decision in the rule order of `decide.rs`: a failure seen and no rollback yet is the
//! rollback (it sets `desired` back to `last_good`, restarts the plan and writes its audit line),
//! otherwise an uninstall that is wanted and not yet ordered is the uninstall order.
//!
//! Each instance takes beats one after another, forever. A beat is the statements of the deciding
//! path: ask for the gate with `SKIP LOCKED` (busy: the beat is answered, nothing is written),
//! read the rows (one step), the write (one step, with its `COMMIT`; a statement's rows are
//! visible at once). Without the gate the read and the write are two statements with nothing
//! around them. The operator's call and the failure report are not processes of their own: each
//! reaches one instance, which takes it in place of a beat, so with one instance nothing
//! interleaves with a beat. The operator's `Rollouts.Advance` is one statement and its audit line
//! and does not take the gate (its compare-and-set decides). The failure report takes the gate
//! (it waits while the gate is held) or, without it, writes at once.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "steps",
    bound: "N instances, each taking beats without end; three scenarios: a plan at stage 1 of 2 \
            advanced by beats and by one operator call, a failed threshold halted by beats, \
            a failure report arriving once while a package is both rolling back and being \
            uninstalled",
    invariants: &[
        "stage_advance_by_racing_instances_is_one_advance_and_one_audit_line",
        "halt_raised_by_two_instances_is_one_generation_and_one_audit_line",
        "rollback_is_committed_before_the_uninstall_is_ordered",
    ],
    cases: &[
        VariantCase {
            variant: "stage_advance_without_compare_and_set_and_without_the_gate",
            breaks: "stage_advance_by_racing_instances_is_one_advance_and_one_audit_line",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "stage_advance_without_the_gate",
            breaks: "stage_advance_by_racing_instances_is_one_advance_and_one_audit_line",
            fails_from: None,
        },
        VariantCase {
            variant: "threshold_halt_without_not_halted_guard_and_without_the_gate",
            breaks: "halt_raised_by_two_instances_is_one_generation_and_one_audit_line",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "threshold_halt_without_the_gate",
            breaks: "halt_raised_by_two_instances_is_one_generation_and_one_audit_line",
            fails_from: None,
        },
        VariantCase {
            variant: "rollback_and_uninstall_decided_and_verdict_edited_without_the_gate",
            breaks: "rollback_is_committed_before_the_uninstall_is_ordered",
            fails_from: Some(2),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Steps::new(n, variant), only)
}

/// The count of a ghost number saturates here: one above what an invariant allows.
const CAP: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Scenario {
    Advance,
    Halt,
    Rollback,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The advance is a plain update (no `WHERE stage = n`) and the beat has no gate.
    StageAdvanceWithoutCompareAndSetAndWithoutTheGate,
    /// The advance keeps its compare-and-set; the beat has no gate.
    StageAdvanceWithoutTheGate,
    /// The halt update has no `WHERE NOT halted` and the beat has no gate.
    ThresholdHaltWithoutNotHaltedGuardAndWithoutTheGate,
    /// The halt keeps `WHERE NOT halted`; the beat has no gate.
    ThresholdHaltWithoutTheGate,
    /// The rollback and the uninstall are decided without the gate, and so is the verdict's edit.
    RollbackAndUninstallDecidedAndVerdictEditedWithoutTheGate,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("stage_advance_without_compare_and_set_and_without_the_gate") => {
                Variant::StageAdvanceWithoutCompareAndSetAndWithoutTheGate
            }
            Some("stage_advance_without_the_gate") => Variant::StageAdvanceWithoutTheGate,
            Some("threshold_halt_without_not_halted_guard_and_without_the_gate") => {
                Variant::ThresholdHaltWithoutNotHaltedGuardAndWithoutTheGate
            }
            Some("threshold_halt_without_the_gate") => Variant::ThresholdHaltWithoutTheGate,
            Some("rollback_and_uninstall_decided_and_verdict_edited_without_the_gate") => {
                Variant::RollbackAndUninstallDecidedAndVerdictEditedWithoutTheGate
            }
            Some(other) => panic!("the model steps has no variant {other}"),
        }
    }

    /// Whether the beats of `scenario` take the gate.
    fn gated(self, scenario: Scenario) -> bool {
        match scenario {
            Scenario::Advance => !matches!(
                self,
                Variant::StageAdvanceWithoutCompareAndSetAndWithoutTheGate
                    | Variant::StageAdvanceWithoutTheGate
            ),
            Scenario::Halt => !matches!(
                self,
                Variant::ThresholdHaltWithoutNotHaltedGuardAndWithoutTheGate
                    | Variant::ThresholdHaltWithoutTheGate
            ),
            Scenario::Rollback => {
                self != Variant::RollbackAndUninstallDecidedAndVerdictEditedWithoutTheGate
            }
        }
    }

    /// Whether a beat's stage advance is conditional on the stage it read.
    fn advance_is_compare_and_set(self) -> bool {
        self != Variant::StageAdvanceWithoutCompareAndSetAndWithoutTheGate
    }

    /// Whether a beat's halt update is conditional on `NOT halted`.
    fn halt_guarded(self) -> bool {
        self != Variant::ThresholdHaltWithoutNotHaltedGuardAndWithoutTheGate
    }
}

/// What a deciding beat chose after reading the rows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Act {
    Nothing,
    /// Advance the stage it read.
    Advance,
    /// Raise the halt it read as not raised.
    Halt,
    /// Set `desired` back to `last_good`.
    Rollback,
    /// Order the uninstall.
    Uninstall,
}

/// Where an instance is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// A beat arrived.
    Arrived,
    /// The gate is granted; nothing read yet.
    Holding,
    /// The rows are read and the decision made.
    Decided(Act),
    /// Holding the gate to write the verdict of the failure report.
    Reporting,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Rows {
    /// The plan's stage (1 or 2).
    stage: u8,
    /// The number of stage advances made; the audit lines written for them.
    advances: u8,
    advance_audits: u8,
    halted: bool,
    /// The halt generation, and the audit lines written for halts.
    generation: u8,
    halt_audits: u8,
    /// A failure verdict is committed.
    failed: bool,
    /// `desired` was set back to `last_good`.
    rolled_back: bool,
    uninstall_ordered: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    scenario: Scenario,
    rows: Rows,
    /// The instance holding the gate row.
    gate: Option<u8>,
    insts: Vec<Inst>,
    /// The operator's `Rollouts.Advance` is still to come.
    operator_left: bool,
    /// The failure report is still to come.
    report_left: bool,
    /// Ghost: an uninstall order was written while a failure verdict was committed and the
    /// rollback was not.
    uninstall_before_rollback: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Arrive(usize),
    GateGranted(usize),
    GateBusyAnswered(usize),
    Read(usize),
    Write(usize),
    OperatorAdvance(usize),
    ReportTakesGate(usize),
    ReportWritesVerdict(usize),
}

pub struct Steps {
    n: usize,
    variant: Variant,
}

impl Steps {
    fn new(n: usize, variant: Option<&str>) -> Steps {
        Steps {
            n,
            variant: Variant::of(variant),
        }
    }

    /// The pure decision of a beat over the rows it read.
    fn decide(scenario: Scenario, rows: &Rows) -> Act {
        match scenario {
            Scenario::Advance if rows.stage == 1 => Act::Advance,
            Scenario::Halt if !rows.halted => Act::Halt,
            Scenario::Rollback if rows.failed && !rows.rolled_back => Act::Rollback,
            Scenario::Rollback if !rows.uninstall_ordered => Act::Uninstall,
            _ => Act::Nothing,
        }
    }

    /// The stage advance `UPDATE plan SET stage = 2 [WHERE stage = 1]` and its audit line,
    /// written when the row count is 1.
    fn advance(rows: &mut Rows, compare_and_set: bool) {
        if !compare_and_set || rows.stage == 1 {
            rows.stage = 2;
            rows.advances = (rows.advances + 1).min(CAP);
            rows.advance_audits = (rows.advance_audits + 1).min(CAP);
        }
    }

    fn release_gate(s: &mut State, i: usize) {
        if s.gate == Some(i as u8) {
            s.gate = None;
        }
    }
}

impl Model for Steps {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        [Scenario::Advance, Scenario::Halt, Scenario::Rollback]
            .into_iter()
            .map(|scenario| State {
                scenario,
                rows: Rows {
                    stage: 1,
                    advances: 0,
                    advance_audits: 0,
                    halted: false,
                    generation: 0,
                    halt_audits: 0,
                    failed: false,
                    rolled_back: false,
                    uninstall_ordered: false,
                },
                gate: None,
                insts: vec![Inst { pc: Pc::Idle }; self.n],
                operator_left: scenario == Scenario::Advance,
                report_left: scenario == Scenario::Rollback,
                uninstall_before_rollback: false,
            })
            .collect()
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        let gated = self.variant.gated(s.scenario);
        for (i, inst) in s.insts.iter().enumerate() {
            match inst.pc {
                Pc::Idle => {
                    let mut t = s.clone();
                    t.insts[i].pc = Pc::Arrived;
                    out.push((Step::Arrive(i), t));
                    if s.operator_left {
                        // Zero rows when the stage moved on: `not_waiting`, nothing written.
                        let mut t = s.clone();
                        t.operator_left = false;
                        Self::advance(&mut t.rows, true);
                        out.push((Step::OperatorAdvance(i), t));
                    }
                    if s.report_left {
                        if !gated {
                            let mut t = s.clone();
                            t.report_left = false;
                            t.rows.failed = true;
                            out.push((Step::ReportWritesVerdict(i), t));
                        } else if s.gate.is_none() {
                            let mut t = s.clone();
                            t.report_left = false;
                            t.gate = Some(i as u8);
                            t.insts[i].pc = Pc::Reporting;
                            out.push((Step::ReportTakesGate(i), t));
                        }
                    }
                }
                Pc::Arrived => {
                    if !gated {
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Decided(Self::decide(s.scenario, &s.rows));
                        out.push((Step::Read(i), t));
                    } else if s.gate.is_none() {
                        let mut t = s.clone();
                        t.gate = Some(i as u8);
                        t.insts[i].pc = Pc::Holding;
                        out.push((Step::GateGranted(i), t));
                    } else {
                        // SKIP LOCKED: no row back; the beat is answered, nothing is written.
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Idle;
                        out.push((Step::GateBusyAnswered(i), t));
                    }
                }
                Pc::Holding => {
                    let mut t = s.clone();
                    t.insts[i].pc = Pc::Decided(Self::decide(s.scenario, &s.rows));
                    out.push((Step::Read(i), t));
                }
                Pc::Decided(act) => {
                    let mut t = s.clone();
                    match act {
                        Act::Nothing => {}
                        Act::Advance => {
                            Self::advance(&mut t.rows, self.variant.advance_is_compare_and_set());
                        }
                        Act::Halt => {
                            if !self.variant.halt_guarded() || !t.rows.halted {
                                t.rows.halted = true;
                                t.rows.generation = (t.rows.generation + 1).min(CAP);
                                t.rows.halt_audits = (t.rows.halt_audits + 1).min(CAP);
                            }
                        }
                        Act::Rollback => t.rows.rolled_back = true,
                        Act::Uninstall => {
                            if t.rows.failed && !t.rows.rolled_back {
                                t.uninstall_before_rollback = true;
                            }
                            t.rows.uninstall_ordered = true;
                        }
                    }
                    Self::release_gate(&mut t, i);
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::Write(i), t));
                }
                Pc::Reporting => {
                    let mut t = s.clone();
                    t.rows.failed = true;
                    Self::release_gate(&mut t, i);
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::ReportWritesVerdict(i), t));
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "stage_advance_by_racing_instances_is_one_advance_and_one_audit_line",
                check: Check::Always(|_, s: &State| {
                    let r = &s.rows;
                    (r.advances > 1 || r.advance_audits > 1 || r.stage > 2).then(|| {
                        format!(
                            "stage {}, {} advances, {} rollout_advance audit lines",
                            r.stage, r.advances, r.advance_audits
                        )
                    })
                }),
            },
            Invariant {
                name: "halt_raised_by_two_instances_is_one_generation_and_one_audit_line",
                check: Check::Always(|_, s: &State| {
                    let r = &s.rows;
                    (r.generation > 1 || r.halt_audits > 1).then(|| {
                        format!(
                            "halt generation {}, {} halt audit lines",
                            r.generation, r.halt_audits
                        )
                    })
                }),
            },
            Invariant {
                name: "rollback_is_committed_before_the_uninstall_is_ordered",
                check: Check::Always(|_, s: &State| {
                    s.uninstall_before_rollback.then(|| {
                        "an uninstall order was written while a failure verdict was committed \
                         and the rollback was not"
                            .to_string()
                    })
                }),
            },
        ]
    }
}
