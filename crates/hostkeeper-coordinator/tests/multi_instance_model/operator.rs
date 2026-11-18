//! The model `operator`: an operator's edit of a decision input (a halt raised, a hold, a change of
//! the desired version) against the deciding beats of the instances (design sections 5.2, 5.3 and
//! 6, principle 4).
//!
//! Rows: the gate row, with its holder and a first-in first-out queue of the plain `FOR UPDATE`
//! requests waiting for it; one service `s1` with its `halted` and `held` flags and its desired
//! version (1, and 2 once the operator has changed it); and the order rows of the hosts `h0` and
//! `h1`, one per host (a plain insert with `ON CONFLICT (host_id) DO NOTHING`), each holding the
//! version it was placed for.
//!
//! Each instance takes beats of either host, one after another, forever. A beat asks for the gate
//! with `SKIP LOCKED`: when the gate is held nothing is granted and the beat is answered with the
//! orders already stored for its host, writing nothing. When the gate is granted the beat reads the
//! flags, the desired version and the host's order row from the rows in one statement, decides (no
//! order when `s1` is halted or held, or the host has an order; else an order at the desired
//! version it read), writes the order in a separate statement, and `COMMIT`s, which makes the order
//! visible and releases the gate.
//!
//! The gate is handed to the head of the queue at the moment its holder releases it. So the gate is
//! never free while a request is queued, and the refusal of a `SKIP LOCKED` request while a
//! conflicting request is queued (which Postgres makes) is not a separate step here: a queued
//! request implies a held gate, and a beat meeting a held gate is refused. The invariant
//! `beat_never_overtakes_a_queued_operator_edit` therefore guards against a beat joining the queue;
//! the refusal rule itself is checked against a real Postgres, not here.
//!
//! The operator is one process, outside the count of instances, making one edit chosen at the
//! start: raise the halt, set the hold, or set desired to 2. Its transaction queues at the gate
//! with a plain `FOR UPDATE`, is granted when the gate is free and it heads the queue, writes the
//! row in a separate statement, and `COMMIT`s, which is the acknowledgement. The database may go
//! down once: every transaction in flight is rolled back (the gate is released, the queue is
//! emptied, buffered writes are dropped), and while it is down no instance and no operator step
//! runs (fail closed); it then comes back up, and an edit that was rolled back may be tried again.
//! The operator's call is served by instance 0, which matters only in the variants that give an
//! instance a cache or a memory.
//!
//! The only abort modelled is that whole-database outage. The abort of one hung holder by a session
//! timeout, which would hand the gate to the queue head while other sessions run on, and the
//! operator's own lock timeout, which would remove it from the queue and refuse the edit, are not
//! modelled. Each only ends a transaction without a commit and so is a subset of the outage's
//! effects (a release without a write, or an edit never acknowledged); neither makes a commit
//! happen that the gate would not allow.
//!
//! Ghost flags: whether a committed order was placed after an edit was acknowledged (for the halt,
//! for the hold, for the desired change with an order at version 1), and whether a beat was
//! granted the gate while the operator's request was queued for it. The hold and desired-change
//! invariant is stated through the committed order, not through what a decision read: a decision
//! that read a stale input commits an order the flags then show.
//!
//! A beat never waits for a reply or for a hold: the reply is not modelled, only the rows.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "operator",
    bound: "N instances, each taking beats of either of 2 hosts without end; one operator edit \
            (halt, hold, or desired 1 to 2); one database outage",
    invariants: &[
        "acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task",
        "acknowledged_hold_and_desired_change_are_read_by_the_next_decision",
        "beat_never_overtakes_a_queued_operator_edit",
    ],
    cases: &[
        VariantCase {
            variant: "operator_edit_without_the_gate",
            breaks: "acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "operator_edit_without_the_gate",
            breaks: "acknowledged_hold_and_desired_change_are_read_by_the_next_decision",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "decision_reads_a_cache",
            breaks: "acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "decision_reads_a_cache",
            breaks: "acknowledged_hold_and_desired_change_are_read_by_the_next_decision",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "beat_queues_at_the_gate",
            breaks: "beat_never_overtakes_a_queued_operator_edit",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "halt_acknowledged_from_memory_while_the_database_is_down",
            breaks: "acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task",
            fails_from: Some(2),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Operator::new(n, variant), only)
}

const HOSTS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// The protocol of the design: the gate for the edit and for every deciding beat.
    Locked,
    /// The operator's edit is a row transaction without the gate.
    OperatorEditWithoutTheGate,
    /// A decision reads the flags and the desired version from its instance's cache, refreshed
    /// from the rows by a separate step at any moment. The operator's call is served by instance
    /// 0, whose cache its commit updates.
    DecisionReadsACache,
    /// A beat takes the gate with a plain `FOR UPDATE` and waits in the queue.
    BeatQueuesAtTheGate,
    /// While the database is down the operator's halt is acknowledged from instance 0's memory
    /// and written to the rows when the database is back. Instance 0's decisions read its memory
    /// as well as the rows; the other instances read the rows alone.
    HaltAcknowledgedFromMemoryWhileTheDatabaseIsDown,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("operator_edit_without_the_gate") => Variant::OperatorEditWithoutTheGate,
            Some("decision_reads_a_cache") => Variant::DecisionReadsACache,
            Some("beat_queues_at_the_gate") => Variant::BeatQueuesAtTheGate,
            Some("halt_acknowledged_from_memory_while_the_database_is_down") => {
                Variant::HaltAcknowledgedFromMemoryWhileTheDatabaseIsDown
            }
            Some(other) => panic!("the model operator has no variant {other}"),
        }
    }
}

/// The edit the operator makes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Edit {
    Halt,
    Hold,
    Desired,
}

/// The inputs of a decision.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Inputs {
    halted: bool,
    held: bool,
    desired: u8,
}

/// Who holds the gate, or waits for it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Who {
    Inst(u8),
    Operator,
}

/// Where an instance is in its beat.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// A beat of the host arrived.
    Arrived(u8),
    /// Waiting in the queue of the gate (a plain `FOR UPDATE`).
    Queued(u8),
    /// The gate is granted; nothing read yet.
    Holding(u8),
    /// The inputs are read and the decision made: `order` is the version to insert, if any.
    Decided {
        host: u8,
        order: Option<u8>,
    },
    /// The insert is sent (not yet committed).
    Written {
        host: u8,
        order: Option<u8>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum OpPc {
    Idle,
    Queued,
    Holding,
    Written,
    Acknowledged,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
    /// What the instance last cached of the inputs (read only under the cache variant).
    cache: Inputs,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    rows: Inputs,
    /// The version each host's order row was placed for, 0 when it has none.
    orders: [u8; HOSTS],
    gate: Option<Who>,
    queue: Vec<Who>,
    insts: Vec<Inst>,
    edit: Edit,
    op: OpPc,
    down: bool,
    outage_left: bool,
    /// Instance 0's memory holds the acknowledged halt, not yet written to the rows.
    memory_halt: bool,
    memory_halt_unwritten: bool,
    /// An order committed after the halt was acknowledged.
    order_after_halt: bool,
    /// An order committed after the hold was acknowledged.
    order_after_hold: bool,
    /// An order for version 1 committed after desired 2 was acknowledged.
    old_order_after_desired: bool,
    /// A beat was granted the gate while the operator's request was queued.
    beat_overtook: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Arrive { inst: usize, host: u8 },
    GateGranted(usize),
    GateBusyAnswered(usize),
    GateQueued(usize),
    Read(usize),
    Write(usize),
    Commit(usize),
    CacheRefresh(usize),
    OperatorBegins,
    OperatorWrites,
    OperatorCommits,
    OperatorAcknowledgesFromMemory,
    MemoryHaltWritten,
    DatabaseDown,
    DatabaseUp,
}

pub struct Operator {
    n: usize,
    variant: Variant,
}

impl Operator {
    fn new(n: usize, variant: Option<&str>) -> Operator {
        Operator {
            n,
            variant: Variant::of(variant),
        }
    }

    /// The inputs instance `i` reads for a decision.
    fn inputs(&self, s: &State, i: usize) -> Inputs {
        match self.variant {
            Variant::DecisionReadsACache => s.insts[i].cache,
            Variant::HaltAcknowledgedFromMemoryWhileTheDatabaseIsDown if i == 0 => Inputs {
                halted: s.rows.halted || s.memory_halt,
                ..s.rows
            },
            _ => s.rows,
        }
    }

    /// The pure decision: the version to insert for `host`, if any.
    fn decide(inputs: Inputs, orders: &[u8; HOSTS], host: u8) -> Option<u8> {
        let found = orders[usize::from(host)] != 0;
        (!found && !inputs.halted && !inputs.held).then_some(inputs.desired)
    }

    /// Releases the gate and grants it to the head of the queue.
    fn release_gate(s: &mut State) {
        s.gate = None;
        if s.queue.is_empty() {
            return;
        }
        let who = s.queue.remove(0);
        s.gate = Some(who);
        match who {
            Who::Operator => s.op = OpPc::Holding,
            Who::Inst(j) => {
                if s.queue.contains(&Who::Operator) {
                    s.beat_overtook = true;
                }
                let j = usize::from(j);
                if let Pc::Queued(host) = s.insts[j].pc {
                    s.insts[j].pc = Pc::Holding(host);
                }
            }
        }
    }

    /// Every transaction in flight is rolled back.
    fn abort_all(s: &mut State) {
        s.gate = None;
        s.queue.clear();
        for inst in &mut s.insts {
            if !matches!(inst.pc, Pc::Idle | Pc::Arrived(_)) {
                inst.pc = Pc::Idle;
            }
        }
        if s.op != OpPc::Acknowledged {
            s.op = OpPc::Idle;
        }
    }
}

impl Model for Operator {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        let rows = Inputs {
            halted: false,
            held: false,
            desired: 1,
        };
        [Edit::Halt, Edit::Hold, Edit::Desired]
            .into_iter()
            .map(|edit| State {
                rows,
                orders: [0; HOSTS],
                gate: None,
                queue: Vec::new(),
                insts: (0..self.n)
                    .map(|_| Inst {
                        pc: Pc::Idle,
                        cache: rows,
                    })
                    .collect(),
                edit,
                op: OpPc::Idle,
                down: false,
                outage_left: true,
                memory_halt: false,
                memory_halt_unwritten: false,
                order_after_halt: false,
                order_after_hold: false,
                old_order_after_desired: false,
                beat_overtook: false,
            })
            .collect()
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        if s.down {
            let mut t = s.clone();
            t.down = false;
            out.push((Step::DatabaseUp, t));
            if self.variant == Variant::HaltAcknowledgedFromMemoryWhileTheDatabaseIsDown
                && s.edit == Edit::Halt
                && s.op == OpPc::Idle
            {
                let mut t = s.clone();
                t.op = OpPc::Acknowledged;
                t.memory_halt = true;
                t.memory_halt_unwritten = true;
                out.push((Step::OperatorAcknowledgesFromMemory, t));
            }
            return;
        }
        if s.outage_left {
            let mut t = s.clone();
            t.outage_left = false;
            t.down = true;
            Self::abort_all(&mut t);
            out.push((Step::DatabaseDown, t));
        }
        if s.memory_halt_unwritten {
            let mut t = s.clone();
            t.memory_halt_unwritten = false;
            t.rows.halted = true;
            out.push((Step::MemoryHaltWritten, t));
        }
        self.next_operator(s, out);
        for i in 0..s.insts.len() {
            self.next_inst(s, i, out);
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task",
                check: Check::Always(|_, s: &State| {
                    s.order_after_halt
                        .then(|| "an order committed after the halt was acknowledged".to_string())
                }),
            },
            Invariant {
                name: "acknowledged_hold_and_desired_change_are_read_by_the_next_decision",
                check: Check::Always(|_, s: &State| {
                    if s.order_after_hold {
                        Some("an order committed after the hold was acknowledged".to_string())
                    } else if s.old_order_after_desired {
                        Some(
                            "an order for version 1 committed after desired 2 was acknowledged"
                                .to_string(),
                        )
                    } else {
                        None
                    }
                }),
            },
            Invariant {
                name: "beat_never_overtakes_a_queued_operator_edit",
                check: Check::Always(|_, s: &State| {
                    s.beat_overtook.then(|| {
                        "a beat was granted the gate while the operator's edit was queued for it"
                            .to_string()
                    })
                }),
            },
        ]
    }
}

impl Operator {
    fn next_operator(&self, s: &State, out: &mut Vec<(Step, State)>) {
        match s.op {
            OpPc::Idle => {
                let mut t = s.clone();
                if self.variant == Variant::OperatorEditWithoutTheGate {
                    t.op = OpPc::Holding;
                } else if s.gate.is_none() {
                    t.gate = Some(Who::Operator);
                    t.op = OpPc::Holding;
                } else {
                    t.queue.push(Who::Operator);
                    t.op = OpPc::Queued;
                }
                out.push((Step::OperatorBegins, t));
            }
            OpPc::Queued | OpPc::Acknowledged => {}
            OpPc::Holding => {
                let mut t = s.clone();
                t.op = OpPc::Written;
                out.push((Step::OperatorWrites, t));
            }
            OpPc::Written => {
                let mut t = s.clone();
                match s.edit {
                    Edit::Halt => t.rows.halted = true,
                    Edit::Hold => t.rows.held = true,
                    Edit::Desired => t.rows.desired = 2,
                }
                if self.variant == Variant::DecisionReadsACache {
                    t.insts[0].cache = t.rows;
                }
                if s.gate == Some(Who::Operator) {
                    Self::release_gate(&mut t);
                }
                t.op = OpPc::Acknowledged;
                out.push((Step::OperatorCommits, t));
            }
        }
    }

    fn next_inst(&self, s: &State, i: usize, out: &mut Vec<(Step, State)>) {
        let me = Who::Inst(i as u8);
        if self.variant == Variant::DecisionReadsACache {
            let mut t = s.clone();
            t.insts[i].cache = s.rows;
            if t != *s {
                out.push((Step::CacheRefresh(i), t));
            }
        }
        match s.insts[i].pc {
            Pc::Idle => {
                for host in 0..HOSTS as u8 {
                    let mut t = s.clone();
                    t.insts[i].pc = Pc::Arrived(host);
                    out.push((Step::Arrive { inst: i, host }, t));
                }
            }
            Pc::Arrived(host) => {
                let mut t = s.clone();
                if s.gate.is_none() {
                    t.gate = Some(me);
                    t.insts[i].pc = Pc::Holding(host);
                    out.push((Step::GateGranted(i), t));
                } else if self.variant == Variant::BeatQueuesAtTheGate {
                    t.queue.push(me);
                    t.insts[i].pc = Pc::Queued(host);
                    out.push((Step::GateQueued(i), t));
                } else {
                    // SKIP LOCKED: no row back. The beat is answered with the stored orders and
                    // writes nothing.
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::GateBusyAnswered(i), t));
                }
            }
            Pc::Queued(_) => {}
            Pc::Holding(host) => {
                let mut t = s.clone();
                let order = Self::decide(self.inputs(s, i), &s.orders, host);
                t.insts[i].pc = Pc::Decided { host, order };
                out.push((Step::Read(i), t));
            }
            Pc::Decided { host, order } => {
                let mut t = s.clone();
                t.insts[i].pc = Pc::Written { host, order };
                out.push((Step::Write(i), t));
            }
            Pc::Written { host, order } => {
                let mut t = s.clone();
                let h = usize::from(host);
                if let Some(version) = order
                    && t.orders[h] == 0
                {
                    t.orders[h] = version;
                    let acknowledged = s.op == OpPc::Acknowledged;
                    t.order_after_halt |= acknowledged && s.edit == Edit::Halt;
                    t.order_after_hold |= acknowledged && s.edit == Edit::Hold;
                    t.old_order_after_desired |=
                        acknowledged && s.edit == Edit::Desired && version == 1;
                }
                if s.gate == Some(me) {
                    Self::release_gate(&mut t);
                }
                t.insts[i].pc = Pc::Idle;
                out.push((Step::Commit(i), t));
            }
        }
    }
}
