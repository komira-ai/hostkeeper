//! The explicit-state search every model of this test runs on: every state reachable from the
//! model's initial states, one step at a time, each step one statement of one process.
//!
//! The search is breadth first, so the trace it gives for a broken invariant is a shortest one.
//! Two kinds of property are checked:
//!
//! - **Always**: a predicate on one state, checked on every reachable state (safety).
//! - **Reachable**: from every reachable state some state where the goal holds can still be
//!   reached (`AG EF goal`): nothing the processes do can get the system stuck short of the goal.
//!   This is the liveness of a model whose processes may always take one more step (a host beats
//!   again, an instance ticks again); it is checked on the whole graph after the search, by a
//!   backward search from the goal states.
//!
//! A search that grows past [`MAX_STATES`] fails the test by name instead of running on: the
//! bound of each model is kept small on purpose and stated in its file and in the spec.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::hash::Hash;

/// The most states one search may visit. Every model at N = 3 stays far below it; a model that
/// reaches it has lost its bound, which is a defect of the model, so the search stops and says so.
pub const MAX_STATES: usize = 4_000_000;

/// What a property says about the states of a model.
pub enum Check<M: Model + ?Sized> {
    /// Holds in every reachable state: `None` when it holds, `Some(why)` when it is broken.
    Always(fn(&M, &M::State) -> Option<String>),
    /// From every reachable state, a state where the goal holds can be reached.
    Reachable(fn(&M, &M::State) -> bool),
}

/// One named property of a model. The name is the invariant's name in the spec and the name of
/// the test that checks it.
pub struct Invariant<M: Model + ?Sized> {
    pub name: &'static str,
    pub check: Check<M>,
}

/// A model: its initial states, the steps from a state, and its properties.
pub trait Model {
    /// The whole state: the shared rows and every process's own state (its program counter and
    /// what it read). Two states that are equal are the same state of the search.
    type State: Clone + Eq + Hash + Debug;
    /// The label of a step, for the trace of a broken property.
    type Step: Debug;

    fn init(&self) -> Vec<Self::State>;

    /// Every step enabled in `s`, with the state it leads to.
    fn next(&self, s: &Self::State, out: &mut Vec<(Self::Step, Self::State)>);

    fn invariants(&self) -> Vec<Invariant<Self>>;

    /// The one state the search keeps for every state that differs from `s` only by which
    /// instance is which. A model whose instances are interchangeable (no step and no property
    /// names an instance) may sort them here, which is the symmetry the design's argument for any
    /// N rests on; the default keeps every state as it is.
    fn canonical(&self, s: Self::State) -> Self::State {
        s
    }
}

/// Each state the search reached, with the state it was first reached from and the step that
/// reached it (none for an initial state).
type Reached<M> = Vec<(<M as Model>::State, Option<(usize, <M as Model>::Step)>)>;

/// A broken property and how it was reached.
#[derive(Debug)]
pub struct Found {
    pub invariant: &'static str,
    pub why: String,
    /// The steps from an initial state to the state that breaks it, then that state.
    pub trace: Vec<String>,
}

/// The result of one search.
#[derive(Debug)]
pub struct Outcome {
    pub states: usize,
    pub found: Option<Found>,
}

/// Searches every state of `model` reachable from its initial states, checking its properties,
/// or only the one named `only`. Stops at the first broken safety property; liveness is judged
/// once the whole graph is known.
pub fn explore<M: Model>(model: &M, only: Option<&str>) -> Outcome {
    let checks: Vec<Invariant<M>> = model
        .invariants()
        .into_iter()
        .filter(|i| only.is_none_or(|name| name == i.name))
        .collect();
    assert!(
        only.is_none() || !checks.is_empty(),
        "the model has no invariant {only:?}"
    );
    let liveness = checks
        .iter()
        .any(|i| matches!(i.check, Check::Reachable(_)));

    let mut index: HashMap<M::State, usize> = HashMap::new();
    let mut states: Reached<M> = Vec::new();
    // For liveness: the states each state is reached from (the reversed graph).
    let mut back: Vec<Vec<usize>> = Vec::new();
    let mut queue = VecDeque::new();
    for s in model.init() {
        let s = model.canonical(s);
        if !index.contains_key(&s) {
            index.insert(s.clone(), states.len());
            queue.push_back(states.len());
            states.push((s, None));
            back.push(Vec::new());
        }
    }
    let mut out = Vec::new();
    while let Some(at) = queue.pop_front() {
        let state = states[at].0.clone();
        for inv in &checks {
            if let Check::Always(holds) = inv.check
                && let Some(why) = holds(model, &state)
            {
                return Outcome {
                    states: states.len(),
                    found: Some(Found {
                        invariant: inv.name,
                        why,
                        trace: trace(&states, at),
                    }),
                };
            }
        }
        out.clear();
        model.next(&state, &mut out);
        for (step, s) in out.drain(..) {
            let s = model.canonical(s);
            let to = match index.get(&s) {
                Some(&to) => to,
                None => {
                    let to = states.len();
                    assert!(
                        to < MAX_STATES,
                        "the search passed {MAX_STATES} states: the model's bound is lost"
                    );
                    index.insert(s.clone(), to);
                    states.push((s, Some((at, step))));
                    back.push(Vec::new());
                    queue.push_back(to);
                    to
                }
            };
            if liveness {
                back[to].push(at);
            }
        }
    }
    for inv in &checks {
        if let Check::Reachable(goal) = inv.check
            && let Some(stuck) = stuck_state(model, &states, &back, goal)
        {
            return Outcome {
                states: states.len(),
                found: Some(Found {
                    invariant: inv.name,
                    why: "no step from this state can reach the goal any more".to_string(),
                    trace: trace(&states, stuck),
                }),
            };
        }
    }
    Outcome {
        states: states.len(),
        found: None,
    }
}

/// The first state (in the order of the search, so one with a shortest trace) from which no
/// state where `goal` holds can be reached, if there is one.
fn stuck_state<M: Model>(
    model: &M,
    states: &Reached<M>,
    back: &[Vec<usize>],
    goal: fn(&M, &M::State) -> bool,
) -> Option<usize> {
    let mut reaches = vec![false; states.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, (s, _)) in states.iter().enumerate() {
        if goal(model, s) {
            reaches[i] = true;
            queue.push_back(i);
        }
    }
    while let Some(at) = queue.pop_front() {
        for &from in &back[at] {
            if !reaches[from] {
                reaches[from] = true;
                queue.push_back(from);
            }
        }
    }
    reaches.iter().position(|r| !r)
}

/// The steps from an initial state to state `at`, then the state itself.
fn trace<S: Debug, T: Debug>(states: &[(S, Option<(usize, T)>)], at: usize) -> Vec<String> {
    let mut steps = Vec::new();
    let mut i = at;
    while let Some((from, step)) = &states[i].1 {
        steps.push(format!("{step:?}"));
        i = *from;
    }
    steps.reverse();
    steps.push(format!("state: {:?}", states[at].0));
    steps
}
