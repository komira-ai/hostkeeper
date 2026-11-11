//! A breadth-first explorer: every state reachable from the start states, the first violation
//! found with the shortest list of steps that reaches it.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// A rule of the protocol that a state or a step broke: the invariant's name (the name of the
/// test that holds it) and what was seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub invariant: &'static str,
    pub detail: String,
}

impl Violation {
    pub fn new(invariant: &'static str, detail: impl Into<String>) -> Violation {
        Violation {
            invariant,
            detail: detail.into(),
        }
    }
}

/// A transition system. `step` returns `Err` when taking the step is itself a violation (a
/// command delivered to a superseded identity, say); `check` judges a state on its own.
pub trait Model {
    type State: Clone + Eq + Hash + Debug;
    type Action: Clone + Debug;

    fn init(&self) -> Vec<Self::State>;
    fn actions(&self, state: &Self::State) -> Vec<Self::Action>;
    fn step(&self, state: &Self::State, action: &Self::Action) -> Result<Self::State, Violation>;
    fn check(&self, state: &Self::State) -> Result<(), Violation>;
}

/// The steps from a start state to a violation, each shown as `Debug` text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    pub violation: Violation,
    pub steps: Vec<String>,
}

/// What one exploration saw. `truncated` is true when `max_states` stopped it before the graph
/// was exhausted, in which case an empty `trace` proves nothing.
#[derive(Debug)]
pub struct Report {
    pub states: usize,
    pub transitions: usize,
    pub truncated: bool,
    pub trace: Option<Trace>,
}

/// Explore the model, stopping at the first violation or after `max_states` distinct states.
pub fn explore<M: Model>(model: &M, max_states: usize) -> Report {
    let mut states: Vec<Node<M::State>> = Vec::new();
    let mut index: HashMap<M::State, usize> = HashMap::new();
    let mut report = Report {
        states: 0,
        transitions: 0,
        truncated: false,
        trace: None,
    };
    for s in model.init() {
        if let Err(v) = model.check(&s) {
            report.trace = Some(Trace {
                violation: v,
                steps: Vec::new(),
            });
            return report;
        }
        if !index.contains_key(&s) {
            index.insert(s.clone(), states.len());
            states.push((s, None));
        }
    }
    let mut next = 0;
    while next < states.len() {
        let state = states[next].0.clone();
        for action in model.actions(&state) {
            report.transitions += 1;
            let result = model
                .step(&state, &action)
                .and_then(|n| model.check(&n).map(|()| n));
            let new = match result {
                Ok(new) => new,
                Err(v) => {
                    let mut steps = path(&states, next);
                    steps.push(format!("{action:?}"));
                    report.trace = Some(Trace {
                        violation: v,
                        steps,
                    });
                    report.states = states.len();
                    return report;
                }
            };
            if index.contains_key(&new) {
                continue;
            }
            if states.len() >= max_states {
                report.truncated = true;
                continue;
            }
            index.insert(new.clone(), states.len());
            states.push((new, Some((next, format!("{action:?}")))));
        }
        next += 1;
    }
    report.states = states.len();
    report
}

/// A state and the step from its parent (the parent's index, the action shown).
type Node<S> = (S, Option<(usize, String)>);

fn path<S>(states: &[Node<S>], mut at: usize) -> Vec<String> {
    let mut steps = Vec::new();
    while let Some((parent, label)) = &states[at].1 {
        steps.push(label.clone());
        at = *parent;
    }
    steps.reverse();
    steps
}
