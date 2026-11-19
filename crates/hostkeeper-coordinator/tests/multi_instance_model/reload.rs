//! The model `reload`: the cache of one part of the store, its invalidation by the version
//! vector and its reload (design section 6, "Invalidation" and "Reload rule"), with the converged
//! path of the heartbeat (design section 5.2).
//!
//! Rows: the part `desired`, abstractly the desired version `d` of one host's service (a rollout
//! is needed while `d` is above the version the host runs, `HOST_RUNS`), and `state_versions`
//! for that part, the number `ver`. Every operator edit is one transaction that writes the rows
//! and bumps `ver`, so the rows of a snapshot are the rows of its `ver`. The order row of the
//! host (`order`: the version it orders, or none). Each instance holds a cache of the part: its
//! content (`d`) and the version its label says. A ghost keeps the rows of every version
//! (`d_at`, indexed by `ver`), to state the first invariant. All start with the label, the content
//! and the database at version 0, `d` 0.
//!
//! The operator is a process outside the N instances. It makes at most two edits; each sets `d`
//! to 0 or to 1 (an edit that does not move `d` is an edit of other rows of the part), and each
//! is one commit of the rows and the bumped `ver`. An edit that does not move `d` still bumps
//! `ver`, so instances reload although their content is right.
//!
//! Each instance takes beats of the host one after another, forever. A beat is: the read of the
//! version vector (one statement with the beat upsert); with the vector equal to the label, the
//! converged path, which replies and decides nothing, but only when the host is not behind
//! according to the cache (`d` at most `HOST_RUNS`) or already has an order at the cache's `d`;
//! with the vector equal to the label and the host behind without such an order, the deciding
//! transaction without a reload; with the vector differing from the label, the reload of the
//! part, and then, when the reloaded content says the host is behind (`d` above `HOST_RUNS`), the
//! deciding transaction. The deciding transaction can also be skipped (the gate is busy, or the
//! transaction aborts), which leaves the order as it was: the next beat decides. Locked, the reload is one statement over one snapshot: the version, then the rows,
//! and the cache is labelled with that version. The deciding transaction reads the rows, not the
//! cache, and upserts the host's one order row at the `d` it reads; it is one step, since it
//! runs under the gate, whose interleavings the model `orders` covers. A new `d` replaces the
//! host's order of an older one. The host does not complete the order within the bound.
//!
//! The first invariant states the labelling rule: the content of a cache is the rows of the
//! version in its label. The second is a reachability check, from every state; its goal is met
//! only once the last edit is made, so it bites after the last edit: from there, a beat of some
//! instance must be able to lead to an order at the current `d` unless the host is not behind.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "reload",
    bound: "N instances, each taking beats of one host without end; one part with its version; \
            an operator making two edits, each setting the desired version to 0 or 1",
    invariants: &[
        "cache_is_never_labelled_newer_than_its_content",
        "host_behind_the_desired_version_is_ordered_after_a_reload",
    ],
    cases: &[
        VariantCase {
            variant: "cache_reloaded_in_two_reads",
            breaks: "cache_is_never_labelled_newer_than_its_content",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "cache_reloaded_in_two_reads",
            breaks: "host_behind_the_desired_version_is_ordered_after_a_reload",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Reload::new(n, variant), only)
}

/// The version the host runs: it is behind while the desired version is above it.
const HOST_RUNS: u8 = 0;
const EDITS: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The rows are read in one statement and the version in another, so a commit can fall
    /// between them.
    CacheReloadedInTwoReads,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("cache_reloaded_in_two_reads") => Variant::CacheReloadedInTwoReads,
            Some(other) => panic!("the model reload has no variant {other}"),
        }
    }
}

/// Where an instance is in its beat.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    Idle,
    /// The version vector is read and differs from the label: the part is to be reloaded.
    Reloading,
    /// Two reads only: the rows are read (`d`); the version is not.
    RowsRead {
        d: u8,
    },
    /// The cache is reloaded; the deciding transaction is next if the content says behind.
    Reloaded,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Cache {
    /// The version the cache is labelled with.
    label: u8,
    /// The content: the desired version.
    d: u8,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Inst {
    pc: Pc,
    cache: Cache,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// The part's rows: the desired version.
    d: u8,
    /// `state_versions` of the part.
    ver: u8,
    /// Ghost: the rows `d` at every version, indexed by `ver`.
    d_at: Vec<u8>,
    /// The version the host's order row orders, if it has one.
    order: Option<u8>,
    edits_left: u8,
    insts: Vec<Inst>,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    Edit { d: u8 },
    ReadVersionVector(usize),
    ConvergedReply(usize),
    ReloadInOneSnapshot(usize),
    DecideWithoutReload(usize),
    ReadRows(usize),
    ReadVersion(usize),
    Decide(usize),
    SkipDecision(usize),
    GateBusy(usize),
}

pub struct Reload {
    n: usize,
    variant: Variant,
}

impl Reload {
    fn new(n: usize, variant: Option<&str>) -> Reload {
        Reload {
            n,
            variant: Variant::of(variant),
        }
    }
}

impl Model for Reload {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            d: 0,
            ver: 0,
            d_at: vec![0],
            order: None,
            edits_left: EDITS,
            insts: vec![
                Inst {
                    pc: Pc::Idle,
                    cache: Cache { label: 0, d: 0 },
                };
                self.n
            ],
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        if s.edits_left > 0 {
            for d in 0..=1u8 {
                let mut t = s.clone();
                t.d = d;
                t.ver += 1;
                t.d_at.push(d);
                t.edits_left -= 1;
                out.push((Step::Edit { d }, t));
            }
        }
        for (i, inst) in s.insts.iter().enumerate() {
            let mut t = s.clone();
            match inst.pc {
                Pc::Idle => {
                    if s.ver == inst.cache.label {
                        let c = inst.cache.d;
                        if c == HOST_RUNS || s.order == Some(c) {
                            out.push((Step::ConvergedReply(i), t));
                        } else {
                            t.insts[i].pc = Pc::Reloaded;
                            out.push((Step::DecideWithoutReload(i), t));
                        }
                    } else {
                        t.insts[i].pc = Pc::Reloading;
                        out.push((Step::ReadVersionVector(i), t));
                    }
                }
                Pc::Reloading => match self.variant {
                    Variant::Locked => {
                        t.insts[i].cache = Cache {
                            label: s.ver,
                            d: s.d,
                        };
                        t.insts[i].pc = Pc::Reloaded;
                        out.push((Step::ReloadInOneSnapshot(i), t));
                    }
                    Variant::CacheReloadedInTwoReads => {
                        t.insts[i].pc = Pc::RowsRead { d: s.d };
                        out.push((Step::ReadRows(i), t));
                    }
                },
                Pc::RowsRead { d } => {
                    t.insts[i].cache = Cache { label: s.ver, d };
                    t.insts[i].pc = Pc::Reloaded;
                    out.push((Step::ReadVersion(i), t));
                }
                Pc::Reloaded => {
                    t.insts[i].pc = Pc::Idle;
                    if inst.cache.d > HOST_RUNS {
                        // The deciding transaction reads the rows, not the cache.
                        if s.d > HOST_RUNS {
                            t.order = Some(s.d);
                        }
                        out.push((Step::Decide(i), t));
                        let mut busy = s.clone();
                        busy.insts[i].pc = Pc::Idle;
                        out.push((Step::GateBusy(i), busy));
                    } else {
                        out.push((Step::SkipDecision(i), t));
                    }
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "cache_is_never_labelled_newer_than_its_content",
                check: Check::Always(|_, s: &State| {
                    s.insts.iter().enumerate().find_map(|(i, inst)| {
                        let rows = s.d_at[usize::from(inst.cache.label)];
                        (inst.cache.d != rows).then(|| {
                            format!(
                                "instance {i} labels its cache {} but holds d {}, the rows of that version have d {rows}",
                                inst.cache.label, inst.cache.d
                            )
                        })
                    })
                }),
            },
            Invariant {
                name: "host_behind_the_desired_version_is_ordered_after_a_reload",
                check: Check::Reachable(|_, s: &State| {
                    s.edits_left == 0 && (s.d == HOST_RUNS || s.order == Some(s.d))
                }),
            },
        ]
    }
}
