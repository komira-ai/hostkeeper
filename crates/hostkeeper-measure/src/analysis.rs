//! The analysis: a run's records to the numbers of its plan's measures. Pure: it reads no clock,
//! no file and no environment, and the same records give the same numbers.
//!
//! A time is the difference of two stamps of one clock, never of two clocks:
//! - the coordinator's, when both events carry its stamp: whole seconds, either an audit entry's
//!   time or the last heartbeat of a host the coordinator listed at the poll that saw the event,
//!   which can be later than the heartbeat that reported the change by up to one poll interval;
//! - otherwise the runner's, whose stamp of an event seen by a poll is up to one poll interval
//!   late.
//!
//! A repetition that did not record what a measure needs adds to its `missing` count, and no
//! number is made up for it. A `last` span is the time to the last of all its events, so a
//! repetition that ended early (its watch timed out, a call failed) gives none: it is missing.
//! Only repetitions that started are counted.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::plan::{Choose, Kind, Measure, Plan, REPETITION};
use crate::record::{Record, RunInfo};

/// The clock a value was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Clock {
    Coordinator,
    Runner,
}

impl Clock {
    pub fn name(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Runner => "runner",
        }
    }
}

/// One value of a measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub rep: u32,
    pub host: Option<String>,
    pub value: f64,
    /// `None` for a count, which reads no clock.
    pub clock: Option<Clock>,
}

/// The least, the median and the greatest of a measure's values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

/// A measure's values over the run.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub measure: Measure,
    pub values: Vec<Value>,
    /// The values a repetition should have given and did not.
    pub missing: u32,
}

impl Measured {
    /// The statistics, `None` with no value.
    pub fn stats(&self) -> Option<Stats> {
        let mut v: Vec<f64> = self.values.iter().map(|v| v.value).collect();
        v.sort_by(f64::total_cmp);
        let n = v.len();
        let median = match n {
            0 => return None,
            _ if n % 2 == 1 => v[n / 2],
            _ => (v[n / 2 - 1] + v[n / 2]) / 2.0,
        };
        Some(Stats {
            min: v[0],
            median,
            max: v[n - 1],
        })
    }

    /// The clocks the values were read from, in order.
    pub fn clocks(&self) -> Vec<Clock> {
        let set: BTreeSet<Clock> = self.values.iter().filter_map(|v| v.clock).collect();
        set.into_iter().collect()
    }
}

/// A run's numbers.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub plan: Plan,
    pub run: String,
    /// The repetitions that started, and of them those that ended complete.
    pub started: u32,
    pub complete: u32,
    /// Each repetition that ended early, and why.
    pub ended: Vec<(u32, String)>,
    pub results: Vec<Measured>,
}

/// Why records cannot be analysed with a plan.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnalysisError {
    #[error("the run is of plan {run:?}, not {plan:?}")]
    OtherPlan { run: String, plan: String },
    #[error("record {index}: repetition {rep} is not one of the run's 1 to {reps}")]
    Repetition { index: usize, rep: u32, reps: u32 },
    #[error("record {index}: {step}/{event} is no event of the plan")]
    Event {
        index: usize,
        step: String,
        event: String,
    },
}

/// The numbers of `plan`'s measures in a run's records.
pub fn analyze(plan: &Plan, info: &RunInfo, records: &[Record]) -> Result<Analysis, AnalysisError> {
    if info.plan != plan.id {
        return Err(AnalysisError::OtherPlan {
            run: info.plan.clone(),
            plan: plan.id.clone(),
        });
    }
    for (index, r) in records.iter().enumerate() {
        if r.rep == 0 || r.rep > info.repetitions {
            return Err(AnalysisError::Repetition {
                index,
                rep: r.rep,
                reps: info.repetitions,
            });
        }
        if !plan
            .events_of(&r.step)
            .is_some_and(|events| events.contains(&r.event.as_str()))
        {
            return Err(AnalysisError::Event {
                index,
                step: r.step.clone(),
                event: r.event.clone(),
            });
        }
    }
    let is = |r: &Record, step: &str, event: &str| r.step == step && r.event == event;
    let started: Vec<u32> = records
        .iter()
        .filter(|r| is(r, REPETITION, "started"))
        .map(|r| r.rep)
        .collect();
    let mut ended = Vec::new();
    let mut complete = 0;
    for r in records.iter().filter(|r| is(r, REPETITION, "ended")) {
        match r.detail.get("why") {
            Some(why) => ended.push((r.rep, why.clone())),
            None => complete += 1,
        }
    }
    let results = plan
        .measurements
        .iter()
        .map(|m| measure(m, &started, &ended, records))
        .collect();
    Ok(Analysis {
        plan: plan.clone(),
        run: info.run.clone(),
        started: u32::try_from(started.len()).unwrap_or(u32::MAX),
        complete,
        ended,
        results,
    })
}

/// The events of `reference` (`step/event`) in repetition `rep`, in record order.
fn events<'r>(records: &'r [Record], rep: u32, reference: &str) -> Vec<&'r Record> {
    let (step, event) = reference.split_once('/').unwrap_or((reference, ""));
    records
        .iter()
        .filter(|r| r.rep == rep && r.step == step && r.event == event)
        .collect()
}

/// The stamp of `r` by `clock`, in milliseconds.
fn at(r: &Record, clock: Clock) -> u64 {
    match clock {
        Clock::Coordinator => r.coordinator_unix.unwrap_or(0).saturating_mul(1000),
        Clock::Runner => r.runner_ms,
    }
}

/// The clock both ends of a time can be read from.
fn common(records: &[&Record]) -> Clock {
    if records.iter().all(|r| r.coordinator_unix.is_some()) {
        Clock::Coordinator
    } else {
        Clock::Runner
    }
}

fn seconds(from: u64, to: u64) -> f64 {
    // Milliseconds of a run are far below 2^53, so the conversion is exact.
    let (from, to) = (from as f64, to as f64);
    (to - from) / 1000.0
}

fn measure(m: &Measure, started: &[u32], ended: &[(u32, String)], records: &[Record]) -> Measured {
    let mut values = Vec::new();
    let mut missing = 0;
    for &rep in started {
        match &m.kind {
            Kind::Span { from, to, pick } => {
                let froms = events(records, rep, from);
                let tos = events(records, rep, to);
                let partial = *pick == Choose::Last && ended.iter().any(|(r, _)| *r == rep);
                if tos.is_empty() || froms.is_empty() || partial {
                    missing += 1;
                    continue;
                }
                if *pick == Choose::Each {
                    for t in tos {
                        let by_host = froms.iter().any(|f| f.host.is_some());
                        let f = if by_host {
                            froms.iter().find(|f| f.host == t.host)
                        } else {
                            froms.first()
                        };
                        let Some(f) = f else {
                            missing += 1;
                            continue;
                        };
                        let clock = common(&[f, t]);
                        values.push(Value {
                            rep,
                            host: t.host.clone(),
                            value: seconds(at(f, clock), at(t, clock)),
                            clock: Some(clock),
                        });
                    }
                    continue;
                }
                let all: Vec<&Record> = froms.iter().chain(tos.iter()).copied().collect();
                let clock = common(&all);
                let start = froms.iter().map(|f| at(f, clock)).min().unwrap_or(0);
                let ends = tos.iter().map(|t| at(t, clock));
                let end = if *pick == Choose::First {
                    ends.min()
                } else {
                    ends.max()
                };
                values.push(Value {
                    rep,
                    host: None,
                    value: seconds(start, end.unwrap_or(0)),
                    clock: Some(clock),
                });
            }
            Kind::Count { of } => {
                let n = events(records, rep, of).len();
                values.push(Value {
                    rep,
                    host: None,
                    value: f64::from(u32::try_from(n).unwrap_or(u32::MAX)),
                    clock: None,
                });
            }
            Kind::Gap { of } => {
                let mut by_host: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
                for r in events(records, rep, of) {
                    if let (Some(h), Some(c)) = (&r.host, r.coordinator_unix) {
                        by_host.entry(h).or_default().push(c);
                    }
                }
                if by_host.is_empty() {
                    missing += 1;
                }
                for (host, mut stamps) in by_host {
                    stamps.sort_unstable();
                    let gap = stamps.windows(2).map(|w| w[1] - w[0]).max();
                    match gap {
                        Some(gap) => values.push(Value {
                            rep,
                            host: Some(host.to_owned()),
                            value: seconds(0, gap.saturating_mul(1000)),
                            clock: Some(Clock::Coordinator),
                        }),
                        None => missing += 1,
                    }
                }
            }
        }
    }
    Measured {
        measure: m.clone(),
        values,
        missing,
    }
}
