//! The model `orders`: the deciding transaction of a heartbeat (design section 5.2) placing
//! orders under `--live-rollouts`, the queue, `max_updating` and `min_up`.
//!
//! Rows: `fleet_orders` (the order rows of each host, with the unique index on the host),
//! `fleet_defers` of the first service's hosts (written by their beats, without the gate, as every
//! beat's own rows are), and the gate row. Services `s1` (set first: `set_seq` 1) on hosts `h0`,
//! `h1`, `h2` in one stage with `max_updating` 1 and `min_up` 1, and `s2` (`set_seq` 2) on host
//! `h3`; `--live-rollouts` 1. At the start `s1`'s hosts defer, so `s2` may go first; they stop
//! deferring at any moment, once.
//!
//! Each instance takes beats one after another, of any host, forever. A beat is the statements of
//! the deciding path: ask for the gate with `SKIP LOCKED` (busy: the beat is answered with the
//! orders already stored for its host, and nothing is written), read every input from the rows,
//! the pure decision, the insert of the order (`ON CONFLICT (host_id) DO NOTHING`), `COMMIT`, and
//! the reply, which carries the host's order row, new or found. An instance may crash once, after
//! `COMMIT` and before the reply; and one instance may hang once inside its transaction, which the
//! server ends by its idle timeout (the transaction rolls back and the gate is released), after
//! which that instance takes beats again.
//!
//! Orders never age out within the bound (`ORDER_AGE` is far longer than the script) and hosts do
//! not finish their update in it: the model is the placing of orders, not their completion.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "orders",
    bound: "N instances, each taking beats of any of 4 hosts without end; s1 on h0, h1, h2 \
            (max_updating 1, min_up 1, deferring until it stops once), s2 on h3; \
            --live-rollouts 1; one crash after COMMIT, one hung holder",
    invariants: &[
        "n_instances_decide_one_rollout_at_a_time",
        "queue_follows_set_seq_on_both_tasks",
        "stage_never_has_more_updating_hosts_than_max_updating",
        "min_up_holds_in_every_stage",
        "one_host_never_has_two_orders",
        "order_after_crash_is_returned_again",
        "a_host_behind_gets_an_order_whichever_instance_hears_it",
    ],
    cases: &[
        VariantCase {
            variant: "decision_without_the_gate",
            breaks: "n_instances_decide_one_rollout_at_a_time",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "decision_without_the_gate",
            breaks: "stage_never_has_more_updating_hosts_than_max_updating",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "decision_without_the_gate",
            breaks: "min_up_holds_in_every_stage",
            fails_from: Some(3),
        },
        VariantCase {
            variant: "decision_without_the_gate",
            breaks: "one_host_never_has_two_orders",
            fails_from: None,
        },
        VariantCase {
            variant: "order_insert_without_unique_index_and_without_the_gate",
            breaks: "one_host_never_has_two_orders",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "queue_stamped_per_instance",
            breaks: "queue_follows_set_seq_on_both_tasks",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "reply_carries_only_a_new_order",
            breaks: "order_after_crash_is_returned_again",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "hung_holder_is_never_terminated",
            breaks: "a_host_behind_gets_an_order_whichever_instance_hears_it",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Orders::new(n, variant), only)
}

const HOSTS: usize = 4;
/// The service of each host: 0 is `s1`, 1 is `s2`.
const SERVICE: [usize; HOSTS] = [0, 0, 0, 1];
const MAX_UPDATING: [u8; 2] = [1, 1];
const MIN_UP: [u8; 2] = [1, 0];
const LIVE_ROLLOUTS: usize = 1;
/// The queue by `set_seq`: `s1` was set before `s2`.
const SET_SEQ_ORDER: [usize; 2] = [0, 1];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The decision's read and its insert are two statements with no gate around them.
    DecisionWithoutTheGate,
    /// As above, and the order rows have no unique index on the host (a plain insert).
    OrderInsertWithoutUniqueIndexAndWithoutTheGate,
    /// Each instance orders the queue by when it first saw each desired version (today's
    /// in-memory stamp), not by `set_seq`: an instance that started later saw them in another
    /// order.
    QueueStampedPerInstance,
    /// The reply carries an order only when its own transaction inserted it.
    ReplyCarriesOnlyANewOrder,
    /// The server never ends a hung holder's transaction (no idle timeout).
    HungHolderIsNeverTerminated,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("decision_without_the_gate") => Variant::DecisionWithoutTheGate,
            Some("order_insert_without_unique_index_and_without_the_gate") => {
                Variant::OrderInsertWithoutUniqueIndexAndWithoutTheGate
            }
            Some("queue_stamped_per_instance") => Variant::QueueStampedPerInstance,
            Some("reply_carries_only_a_new_order") => Variant::ReplyCarriesOnlyANewOrder,
            Some("hung_holder_is_never_terminated") => Variant::HungHolderIsNeverTerminated,
            Some(other) => panic!("the model orders has no variant {other}"),
        }
    }

    fn gated(self) -> bool {
        !matches!(
            self,
            Variant::DecisionWithoutTheGate
                | Variant::OrderInsertWithoutUniqueIndexAndWithoutTheGate
        )
    }
}

/// What a decision chose for its host.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Choice {
    /// The host already has an order row: the reply carries it.
    Found,
    /// Insert an order for the host.
    Insert,
    /// No order now.
    Wait,
}

/// Where an instance is in its beat.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// A beat of the host arrived.
    Arrived(u8),
    /// The gate is granted; nothing read yet.
    Holding(u8),
    /// The inputs are read and the decision made. `skipped`: it orders a service while one set
    /// before it could be ordered and is not live (the queue skipped).
    Decided {
        host: u8,
        choice: Choice,
        skipped: bool,
    },
    /// The insert is sent (not yet committed).
    Written {
        host: u8,
        choice: Choice,
        skipped: bool,
    },
    /// Committed; `new`: this transaction inserted the host's order row.
    Committed {
        host: u8,
        new: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
    /// Hung inside its transaction: it takes no step until the server ends the transaction.
    hung: bool,
    /// The queue this instance orders by.
    queue: [usize; 2],
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Rows {
    /// The number of order rows of each host.
    orders: [u8; HOSTS],
    /// Whether `s1`'s hosts defer.
    s1_defers: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    rows: Rows,
    /// The instance holding the gate row.
    gate: Option<u8>,
    insts: Vec<Inst>,
    /// Whether each host has received its order in a reply.
    received: [bool; HOSTS],
    crash_left: bool,
    hang_left: bool,
    /// A committed order skipped the queue (by the rows its decision read).
    queue_skipped: bool,
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
    Read(usize),
    Write(usize),
    Commit(usize),
    Reply(usize),
    CrashBeforeReply(usize),
    Hang(usize),
    ServerEndsHungTransaction(usize),
    S1StopsDeferring,
}

pub struct Orders {
    n: usize,
    variant: Variant,
}

impl Orders {
    fn new(n: usize, variant: Option<&str>) -> Orders {
        Orders {
            n,
            variant: Variant::of(variant),
        }
    }

    /// The pure decision for a beat of `host` over the rows `rows`, with the queue `queue`.
    /// Returns the choice and whether it orders past a service set before it (by `set_seq`).
    fn decide(rows: &Rows, host: u8, queue: [usize; 2]) -> (Choice, bool) {
        let h = usize::from(host);
        if rows.orders[h] > 0 {
            return (Choice::Found, false);
        }
        let svc = SERVICE[h];
        if !Self::can_order(rows, svc) {
            return (Choice::Wait, false);
        }
        let live = Self::live(rows);
        if !live[svc] {
            let holding = live.iter().filter(|l| **l).count();
            let ahead = Self::ahead(rows, svc, queue);
            if holding + ahead >= LIVE_ROLLOUTS {
                return (Choice::Wait, false);
            }
        }
        (Choice::Insert, Self::ahead(rows, svc, SET_SEQ_ORDER) > 0)
    }

    /// The services set before `svc` in `queue` that wait and can be ordered: each takes a
    /// place (`queue.rs`, `ahead_of`).
    fn ahead(rows: &Rows, svc: usize, queue: [usize; 2]) -> usize {
        let live = Self::live(rows);
        queue
            .iter()
            .take_while(|t| **t != svc)
            .filter(|t| !live[**t] && Self::can_order(rows, **t))
            .count()
    }

    /// Whether each service has an order row: its rollout is live.
    fn live(rows: &Rows) -> [bool; 2] {
        let mut live = [false; 2];
        for h in 0..HOSTS {
            if rows.orders[h] > 0 {
                live[SERVICE[h]] = true;
            }
        }
        live
    }

    /// Whether a host of `svc` with no order can be ordered now: it does not defer, and one more
    /// update keeps `max_updating` and `min_up`.
    fn can_order(rows: &Rows, svc: usize) -> bool {
        let hosts: Vec<usize> = (0..HOSTS).filter(|h| SERVICE[*h] == svc).collect();
        let updating: u8 = hosts.iter().map(|h| rows.orders[*h]).sum();
        let ordered = hosts.iter().filter(|h| rows.orders[**h] > 0).count();
        let waiting = hosts.len() - ordered;
        let defers = svc == 0 && rows.s1_defers;
        let up_after = u8::try_from(waiting).unwrap_or(u8::MAX).saturating_sub(1);
        waiting > 0 && !defers && updating < MAX_UPDATING[svc] && up_after >= MIN_UP[svc]
    }

    fn release_gate(s: &mut State, i: usize) {
        if s.gate == Some(i as u8) {
            s.gate = None;
        }
    }
}

impl Model for Orders {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        // Instance 0 saw the desired versions as they were set; under the variant, each later
        // instance may have seen them in either order.
        let orders = if self.variant == Variant::QueueStampedPerInstance {
            vec![[0, 1], [1, 0]]
        } else {
            vec![SET_SEQ_ORDER]
        };
        let mut queues: Vec<Vec<[usize; 2]>> = vec![vec![SET_SEQ_ORDER]];
        for _ in 1..self.n {
            queues = queues
                .into_iter()
                .flat_map(|q| {
                    orders.iter().map(move |o| {
                        let mut q = q.clone();
                        q.push(*o);
                        q
                    })
                })
                .collect();
        }
        queues
            .into_iter()
            .map(|qs| State {
                rows: Rows {
                    orders: [0; HOSTS],
                    s1_defers: true,
                },
                gate: None,
                insts: qs
                    .into_iter()
                    .map(|queue| Inst {
                        pc: Pc::Idle,
                        hung: false,
                        queue,
                    })
                    .collect(),
                received: [false; HOSTS],
                crash_left: true,
                hang_left: true,
                queue_skipped: false,
            })
            .collect()
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        if s.rows.s1_defers {
            let mut t = s.clone();
            t.rows.s1_defers = false;
            out.push((Step::S1StopsDeferring, t));
        }
        for (i, inst) in s.insts.iter().enumerate() {
            if inst.hung {
                if self.variant != Variant::HungHolderIsNeverTerminated {
                    let mut t = s.clone();
                    Self::release_gate(&mut t, i);
                    t.insts[i].hung = false;
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::ServerEndsHungTransaction(i), t));
                }
                continue;
            }
            let in_tx = matches!(
                inst.pc,
                Pc::Holding(_) | Pc::Decided { .. } | Pc::Written { .. }
            );
            if in_tx && s.hang_left && s.gate == Some(i as u8) {
                let mut t = s.clone();
                t.hang_left = false;
                t.insts[i].hung = true;
                out.push((Step::Hang(i), t));
            }
            match inst.pc {
                Pc::Idle => {
                    for host in 0..HOSTS as u8 {
                        let mut t = s.clone();
                        t.insts[i].pc = Pc::Arrived(host);
                        out.push((Step::Arrive { inst: i, host }, t));
                    }
                }
                Pc::Arrived(host) => {
                    if !self.variant.gated() {
                        let mut t = s.clone();
                        let (choice, skipped) = Self::decide(&s.rows, host, inst.queue);
                        t.insts[i].pc = Pc::Decided {
                            host,
                            choice,
                            skipped,
                        };
                        out.push((Step::Read(i), t));
                    } else if s.gate.is_none() {
                        let mut t = s.clone();
                        t.gate = Some(i as u8);
                        t.insts[i].pc = Pc::Holding(host);
                        out.push((Step::GateGranted(i), t));
                    } else {
                        // SKIP LOCKED: no row back. The beat is answered with the orders
                        // stored for its host; nothing is written.
                        let mut t = s.clone();
                        if self.variant != Variant::ReplyCarriesOnlyANewOrder
                            && s.rows.orders[usize::from(host)] > 0
                        {
                            t.received[usize::from(host)] = true;
                        }
                        t.insts[i].pc = Pc::Idle;
                        out.push((Step::GateBusyAnswered(i), t));
                    }
                }
                Pc::Holding(host) => {
                    let mut t = s.clone();
                    let (choice, skipped) = Self::decide(&s.rows, host, inst.queue);
                    t.insts[i].pc = Pc::Decided {
                        host,
                        choice,
                        skipped,
                    };
                    out.push((Step::Read(i), t));
                }
                Pc::Decided {
                    host,
                    choice,
                    skipped,
                } => {
                    let mut t = s.clone();
                    t.insts[i].pc = Pc::Written {
                        host,
                        choice,
                        skipped,
                    };
                    out.push((Step::Write(i), t));
                }
                Pc::Written {
                    host,
                    choice,
                    skipped,
                } => {
                    let mut t = s.clone();
                    let h = usize::from(host);
                    let mut new = false;
                    if choice == Choice::Insert {
                        let unique =
                            self.variant != Variant::OrderInsertWithoutUniqueIndexAndWithoutTheGate;
                        if !unique || t.rows.orders[h] == 0 {
                            t.rows.orders[h] = t.rows.orders[h].saturating_add(1).min(3);
                            new = true;
                            if skipped {
                                t.queue_skipped = true;
                            }
                        }
                    }
                    Self::release_gate(&mut t, i);
                    t.insts[i].pc = Pc::Committed { host, new };
                    out.push((Step::Commit(i), t));
                }
                Pc::Committed { host, new } => {
                    let mut t = s.clone();
                    let h = usize::from(host);
                    let carries = if self.variant == Variant::ReplyCarriesOnlyANewOrder {
                        new
                    } else {
                        t.rows.orders[h] > 0
                    };
                    if carries {
                        t.received[h] = true;
                    }
                    t.insts[i].pc = Pc::Idle;
                    out.push((Step::Reply(i), t));
                    if s.crash_left {
                        let mut t = s.clone();
                        t.crash_left = false;
                        t.insts[i].pc = Pc::Idle;
                        out.push((Step::CrashBeforeReply(i), t));
                    }
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "n_instances_decide_one_rollout_at_a_time",
                check: Check::Always(|_, s: &State| {
                    let live = Orders::live(&s.rows).iter().filter(|l| **l).count();
                    (live > LIVE_ROLLOUTS)
                        .then(|| format!("{live} services live, --live-rollouts {LIVE_ROLLOUTS}"))
                }),
            },
            Invariant {
                name: "queue_follows_set_seq_on_both_tasks",
                check: Check::Always(|_, s: &State| {
                    s.queue_skipped.then(|| {
                        "an order was placed for s2 while s1, set before it, waited and could be \
                         ordered"
                            .to_string()
                    })
                }),
            },
            Invariant {
                name: "stage_never_has_more_updating_hosts_than_max_updating",
                check: Check::Always(|_, s: &State| {
                    let updating: u8 = s.rows.orders[..3].iter().sum();
                    (updating > MAX_UPDATING[0])
                        .then(|| format!("{updating} hosts of s1 updating, max_updating 1"))
                }),
            },
            Invariant {
                name: "min_up_holds_in_every_stage",
                check: Check::Always(|_, s: &State| {
                    let up = s.rows.orders[..3].iter().filter(|o| **o == 0).count();
                    (up < usize::from(MIN_UP[0])).then(|| format!("{up} hosts of s1 up, min_up 1"))
                }),
            },
            Invariant {
                name: "one_host_never_has_two_orders",
                check: Check::Always(|_, s: &State| {
                    s.rows
                        .orders
                        .iter()
                        .position(|o| *o > 1)
                        .map(|h| format!("host h{h} has {} order rows", s.rows.orders[h]))
                }),
            },
            Invariant {
                name: "order_after_crash_is_returned_again",
                check: Check::Reachable(|_, s: &State| {
                    (0..HOSTS).all(|h| s.rows.orders[h] == 0 || s.received[h])
                }),
            },
            Invariant {
                name: "a_host_behind_gets_an_order_whichever_instance_hears_it",
                check: Check::Reachable(|_, s: &State| {
                    (0..HOSTS as u8)
                        .all(|h| Orders::decide(&s.rows, h, SET_SEQ_ORDER).0 != Choice::Insert)
                }),
            },
        ]
    }
}
