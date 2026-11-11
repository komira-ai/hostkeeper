//! The explorer on toy models whose answer is known.

use hostkeeper_mdm_model::explore::{Model, Violation, explore};

/// A counter that steps by one or two up to `limit`, judged by `bad`.
struct Counter {
    starts: Vec<u32>,
    limit: u32,
    bad: u32,
    step_bad: u32,
}

impl Model for Counter {
    type State = u32;
    type Action = u32;

    fn init(&self) -> Vec<u32> {
        self.starts.clone()
    }

    fn actions(&self, s: &u32) -> Vec<u32> {
        if *s >= self.limit {
            Vec::new()
        } else {
            vec![1, 2]
        }
    }

    fn step(&self, s: &u32, a: &u32) -> Result<u32, Violation> {
        if s + a == self.step_bad {
            return Err(Violation::new("step", format!("stepped onto {}", s + a)));
        }
        Ok(s + a)
    }

    fn check(&self, s: &u32) -> Result<(), Violation> {
        if *s == self.bad {
            return Err(Violation::new("state", format!("reached {s}")));
        }
        Ok(())
    }
}

fn counter(limit: u32, bad: u32, step_bad: u32) -> Counter {
    Counter {
        starts: vec![0, 0],
        limit,
        bad,
        step_bad,
    }
}

#[test]
fn a_model_with_no_violation_is_explored_to_the_end_and_each_state_counted_once() {
    let r = explore(&counter(5, 99, 99), 100);
    assert!(r.trace.is_none());
    assert!(!r.truncated);
    // 0..=6 (a step of two from 4 or 5 passes the limit), with the duplicated start state once.
    assert_eq!(r.states, 7);
}

#[test]
fn the_first_violation_is_returned_with_a_shortest_path() {
    let r = explore(&counter(20, 6, 99), 100);
    let trace = r.trace.expect("6 is reachable");
    assert_eq!(trace.violation.invariant, "state");
    assert_eq!(trace.steps, ["2", "2", "2"]);
}

#[test]
fn a_step_that_is_itself_the_violation_ends_the_search_with_the_path_and_the_step() {
    let r = explore(&counter(20, 99, 3), 100);
    let trace = r.trace.expect("a step onto 3 is enabled");
    assert_eq!(trace.violation.invariant, "step");
    assert_eq!(trace.violation.detail, "stepped onto 3");
    assert_eq!(trace.steps, ["1", "2"]);
}

#[test]
fn a_start_state_that_breaks_an_invariant_has_an_empty_path() {
    let r = explore(&counter(5, 0, 99), 100);
    let trace = r.trace.expect("0 is bad");
    assert!(trace.steps.is_empty());
}

#[test]
fn a_search_cut_by_the_state_limit_says_so() {
    let r = explore(&counter(50, 99, 99), 10);
    assert!(r.truncated);
    assert_eq!(r.states, 10);
    assert!(r.trace.is_none());
}
