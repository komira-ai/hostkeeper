//! The model `drain`: a start-first update of one task behind the edge, and the drain delay of
//! design section 11, item 2.
//!
//! The update replaces one task, the old one, by a new one; the other N - 1 tasks of the service
//! serve throughout (N is the number of tasks in steady state). Time is a counter the model
//! advances one second per step: a step is one second of the system, with every choice that
//! second allows taken in turn. No wall clock is read.
//!
//! The edge's view of each task: whether DNS lists it, whether its active check marks it healthy,
//! the run of check results against that mark, and the second of the check interval it is at. A
//! check every `INTERVAL` seconds asks `/healthz`: 200 counts as a pass, 503 or a refused
//! connection as a failure; `HEALTHY_THRESHOLD` passes in a row mark a task healthy and
//! `UNHEALTHY_THRESHOLD` failures in a row mark it unhealthy. The edge sends connections to its
//! healthy tasks; when fewer than half the tasks it knows are healthy it is in panic mode and
//! sends them to every task it knows (Envoy's default panic threshold, 50%).
//!
//! The new task starts, and at some second it is ready: its listeners accept and `/healthz`
//! answers 200. Docker's healthcheck passes at some second after that, and Swarm sends the old
//! task SIGTERM. From SIGTERM the old task answers `/healthz` 503 and keeps accepting for
//! `--drain-delay-secs`, then it refuses new connections and exits, and DNS drops it. As the
//! design assumes: DNS lists the new task no earlier than when it is ready and at the latest one
//! refresh (`DNS_REFRESH`, one second) after SIGTERM, and the edge marks a task it has just learned unhealthy until it passes
//! `HEALTHY_THRESHOLD` checks. Each of those seconds, and the second of the interval each check
//! starts at, is a choice of the model, so every timing is searched.
//!
//! The locked protocol uses the smallest delay the deploy check admits: it must be greater than
//! `DNS_REFRESH + HEALTHY_THRESHOLD x INTERVAL + INTERVAL` = 7, so 8.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "drain",
    bound: "one task of N replaced start-first; edge check every 2 s, 2 passes to mark healthy, \
            2 failures to mark unhealthy, DNS refresh 1 s, panic below half healthy; \
            --drain-delay-secs 8, the smallest the deploy check admits; every second of \
            readiness, healthcheck, DNS and check phase searched",
    invariants: &["no_connection_is_refused_at_the_edge_during_an_update"],
    cases: &[
        VariantCase {
            variant: "old_task_stops_accepting_at_sigterm",
            breaks: "no_connection_is_refused_at_the_edge_during_an_update",
            fails_from: Some(1),
        },
        VariantCase {
            variant: "drain_delay_shorter_than_the_edge_needs_to_mark_the_new_task_healthy",
            breaks: "no_connection_is_refused_at_the_edge_during_an_update",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Drain::new(n, variant), only)
}

const INTERVAL: u8 = 2;
const HEALTHY_THRESHOLD: u8 = 2;
const UNHEALTHY_THRESHOLD: u8 = 2;
const DNS_REFRESH: u8 = 1;
/// The smallest `--drain-delay-secs` above `DNS_REFRESH + HEALTHY_THRESHOLD x INTERVAL +
/// INTERVAL`.
const DRAIN_DELAY: u8 = DNS_REFRESH + HEALTHY_THRESHOLD * INTERVAL + INTERVAL + 1;
/// A delay shorter than the edge can take to mark the new task healthy.
const SHORT_DRAIN_DELAY: u8 = 3;

/// The old task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Old {
    Serving,
    /// SIGTERM received: 503 on `/healthz`, still accepting, for this many more seconds.
    Draining(u8),
    /// Refuses new connections and exits; DNS still lists it until its next refresh.
    Closed,
    /// Exited and dropped by DNS.
    Gone,
}

/// The new task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum New {
    Starting,
    /// Listening, `/healthz` 200.
    Ready,
}

/// The edge's view of one task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct View {
    listed: bool,
    healthy: bool,
    /// Check results in a row against the current mark.
    against: u8,
    /// Seconds until the next check.
    next_check: u8,
}

impl View {
    /// One check with result `pass`.
    fn checked(mut self, pass: bool) -> View {
        if pass == self.healthy {
            self.against = 0;
        } else {
            self.against += 1;
            let needed = if pass {
                HEALTHY_THRESHOLD
            } else {
                UNHEALTHY_THRESHOLD
            };
            if self.against >= needed {
                self.healthy = pass;
                self.against = 0;
            }
        }
        self
    }

    /// One second: a check when it is due.
    fn second(self, pass: bool) -> View {
        if !self.listed {
            return self;
        }
        if self.next_check == 0 {
            View {
                next_check: INTERVAL - 1,
                ..self.checked(pass)
            }
        } else {
            View {
                next_check: self.next_check - 1,
                ..self
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    old: Old,
    new: New,
    old_view: View,
    new_view: View,
    /// Seconds since SIGTERM, up to `DNS_REFRESH` (DNS must list the new task by then).
    since_sigterm: Option<u8>,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub struct Step {
    ready: bool,
    sigterm: bool,
    listed: bool,
    new_check_phase: u8,
}

pub struct Drain {
    /// The tasks that serve throughout: always listed, healthy and accepting.
    others: usize,
    delay: u8,
    /// Also check `the_edge_is_never_in_panic_mode`, which the locked protocol does not keep:
    /// [`panic_mode_is_reached`] searches for the state that breaks it.
    probe_panic: bool,
}

/// Whether the edge can be in panic mode at some second of the update, with the locked
/// protocol at `n` tasks: the search finds a state that breaks `the_edge_is_never_in_panic_mode`.
pub fn panic_mode_is_reached(n: usize) -> bool {
    let model = Drain {
        probe_panic: true,
        ..Drain::new(n, None)
    };
    explore(&model, Some("the_edge_is_never_in_panic_mode"))
        .found
        .is_some()
}

/// The tasks the edge lists and the tasks it marks healthy, and whether it is in panic mode
/// (fewer than half of the listed tasks healthy).
fn edge_counts(m: &Drain, s: &State) -> (usize, usize, bool) {
    let views = [s.old_view, s.new_view];
    let known = m.others + views.iter().filter(|v| v.listed).count();
    let healthy = m.others + views.iter().filter(|v| v.listed && v.healthy).count();
    (known, healthy, healthy * 2 < known)
}

impl Drain {
    fn new(n: usize, variant: Option<&str>) -> Drain {
        let delay = match variant {
            None => DRAIN_DELAY,
            Some("old_task_stops_accepting_at_sigterm") => 0,
            Some("drain_delay_shorter_than_the_edge_needs_to_mark_the_new_task_healthy") => {
                SHORT_DRAIN_DELAY
            }
            Some(other) => panic!("the model drain has no variant {other}"),
        };
        Drain {
            others: n - 1,
            delay,
            probe_panic: false,
        }
    }

    /// The old task after SIGTERM.
    fn signalled(&self) -> Old {
        if self.delay == 0 {
            Old::Closed
        } else {
            Old::Draining(self.delay)
        }
    }
}

impl Model for Drain {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        (0..INTERVAL)
            .map(|next_check| State {
                old: Old::Serving,
                new: New::Starting,
                old_view: View {
                    listed: true,
                    healthy: true,
                    against: 0,
                    next_check,
                },
                new_view: View {
                    listed: false,
                    healthy: false,
                    against: 0,
                    next_check: 0,
                },
                since_sigterm: None,
            })
            .collect()
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        if s.old == Old::Gone && s.new_view.healthy {
            return;
        }
        let readies: &[bool] = match s.new {
            New::Starting => &[false, true],
            New::Ready => &[true],
        };
        for &ready in readies {
            let sigterms: &[bool] = if ready && s.old == Old::Serving {
                &[false, true]
            } else {
                &[false]
            };
            for &sigterm in sigterms {
                // The second this step makes is `since_sigterm + 1` after SIGTERM.
                let must_list = s.since_sigterm.is_some_and(|t| t + 1 >= DNS_REFRESH);
                let lists: &[bool] = if !ready || s.new_view.listed {
                    &[false]
                } else if must_list {
                    &[true]
                } else {
                    &[false, true]
                };
                for &listed in lists {
                    let phases: Vec<u8> = if listed {
                        (0..INTERVAL).collect()
                    } else {
                        vec![0]
                    };
                    for new_check_phase in phases {
                        let mut t = s.clone();
                        if ready {
                            t.new = New::Ready;
                        }
                        if listed {
                            t.new_view.listed = true;
                            t.new_view.next_check = new_check_phase;
                        }
                        t.old_view = t.old_view.second(matches!(t.old, Old::Serving));
                        t.new_view = t.new_view.second(t.new == New::Ready);
                        t.old = match t.old {
                            Old::Serving if sigterm => self.signalled(),
                            Old::Draining(1) => Old::Closed,
                            Old::Draining(k) => Old::Draining(k - 1),
                            Old::Closed => {
                                t.old_view.listed = false;
                                Old::Gone
                            }
                            other => other,
                        };
                        t.since_sigterm = if sigterm {
                            Some(0)
                        } else {
                            s.since_sigterm.map(|k| (k + 1).min(DNS_REFRESH))
                        };
                        out.push((
                            Step {
                                ready,
                                sigterm,
                                listed,
                                new_check_phase,
                            },
                            t,
                        ));
                    }
                }
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        let mut invariants = vec![Invariant {
            name: "no_connection_is_refused_at_the_edge_during_an_update",
            check: Check::Always(|m: &Drain, s: &State| {
                let old_accepts = matches!(s.old, Old::Serving | Old::Draining(_));
                let new_accepts = s.new == New::Ready;
                let listed = [
                    (s.old_view, old_accepts, "old"),
                    (s.new_view, new_accepts, "new"),
                ];
                let (_, _, panic) = edge_counts(m, s);
                listed
                    .iter()
                    .filter(|(v, _, _)| v.listed && (panic || v.healthy))
                    .find(|(_, accepts, _)| !accepts)
                    .map(|(_, _, which)| {
                        format!(
                            "the edge sends connections to the {which} task, which refuses them{}",
                            if panic { " (panic mode)" } else { "" }
                        )
                    })
            }),
        }];
        if self.probe_panic {
            invariants.push(Invariant {
                name: "the_edge_is_never_in_panic_mode",
                check: Check::Always(|m: &Drain, s: &State| {
                    let (known, healthy, panic) = edge_counts(m, s);
                    panic.then(|| format!("{healthy} of {known} listed tasks healthy"))
                }),
            });
        }
        invariants
    }
}
