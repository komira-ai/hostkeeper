//! A measurement plan: one TOML file that says what is measured, on which hosts, how many
//! repetitions, what is recorded, the steps the runner takes in each repetition, the numbers the
//! analysis computes from the records, and what the report states from them.
//!
//! [`Plan::parse`] reads a plan and checks it whole: every name it uses is declared, every event
//! a measure reads is one its step records, and every slot of a statement is a number a measure
//! gives. A plan that passes can be run and analysed without a missing name at either end.

use std::collections::BTreeSet;

use serde::Deserialize;
use thiserror::Error;

use crate::template::{self, Slot, TemplateError, id_ok, param_name_ok};

/// The step id the runner records the start and end of each repetition under.
pub const REPETITION: &str = "repetition";

/// A measurement plan.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// 1 to 32 bytes of `[a-z0-9-]`; the plan file is `<id>.toml`.
    pub id: String,
    pub title: String,
    /// What is measured, in prose.
    pub measures: String,
    pub hosts: Hosts,
    /// How many times the steps are run, one after another.
    pub repetitions: u32,
    /// What is recorded, in prose.
    pub records: String,
    #[serde(default, rename = "param")]
    pub params: Vec<Param>,
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
    #[serde(rename = "measure")]
    pub measurements: Vec<Measure>,
    /// What the report states from the numbers: sentences with `{measure.stat}` slots.
    pub states: Vec<String>,
}

/// The hosts of a plan.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hosts {
    /// Which hosts, in prose.
    pub says: String,
    pub pick: Pick,
    /// The service whose hosts are picked, for [`Pick::Service`].
    #[serde(default)]
    pub service: Option<String>,
    /// How many hosts the pick must give; the run refuses another number.
    #[serde(default)]
    pub exactly: Option<u32>,
}

/// How the hosts of a repetition are found, from `hkctl hosts list` at its start.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Pick {
    /// Every host whose services (as the coordinator lists them) name the service.
    Service,
    /// Every host the coordinator lists as live.
    Live,
    /// None: the plan's hosts are found by its steps (a new host enrolling).
    None,
}

/// A parameter the operator gives on the command line.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    pub says: String,
    /// The value differs per repetition (`--param name@k=value`), so each repetition needs one.
    #[serde(default)]
    pub per_repetition: bool,
}

/// One step of a repetition.
#[derive(Clone, Debug, Deserialize)]
#[serde(from = "RawStep")]
pub struct Step {
    pub id: String,
    pub action: Action,
}

/// What a step does.
#[derive(Clone, Debug)]
pub enum Action {
    /// Runs `hkctl <args>`, with `--request <file>` holding `request` when it is set. With
    /// `audit`, the coordinator's audit entry of that action the call wrote stamps the event.
    Call {
        args: Vec<String>,
        request: Option<serde_json::Value>,
        audit: Option<String>,
    },
    /// Prints an instruction to the operator and waits for a line on standard input.
    Operator { says: String },
    /// Polls the coordinator until a condition holds, recording what it sees on the way.
    Watch(Watch),
}

/// A step as the file spells it: `do` names the action, beside the step's id.
#[derive(Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case", deny_unknown_fields)]
enum RawStep {
    Call {
        id: String,
        args: Vec<String>,
        #[serde(default)]
        request: Option<serde_json::Value>,
        #[serde(default)]
        audit: Option<String>,
    },
    Operator {
        id: String,
        says: String,
    },
    Watch {
        id: String,
        until: Until,
        poll_secs: u64,
        timeout_secs: u64,
        #[serde(default)]
        on_timeout: OnTimeout,
    },
}

impl From<RawStep> for Step {
    fn from(raw: RawStep) -> Self {
        match raw {
            RawStep::Call {
                id,
                args,
                request,
                audit,
            } => Self {
                id,
                action: Action::Call {
                    args,
                    request,
                    audit,
                },
            },
            RawStep::Operator { id, says } => Self {
                id,
                action: Action::Operator { says },
            },
            RawStep::Watch {
                id,
                until,
                poll_secs,
                timeout_secs,
                on_timeout,
            } => Self {
                id,
                action: Action::Watch(Watch {
                    until,
                    poll_secs,
                    timeout_secs,
                    on_timeout,
                }),
            },
        }
    }
}

/// A watch: the condition, how often it polls and for how long.
#[derive(Clone, Debug)]
pub struct Watch {
    pub until: Until,
    pub poll_secs: u64,
    pub timeout_secs: u64,
    /// What a timeout does: end the repetition (the default), or go on to the next step.
    pub on_timeout: OnTimeout,
}

/// What a watch waits for.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "what", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Until {
    /// Every host of the repetition has updated `service` to `version`: the coordinator lists it
    /// installed, with a last update to it that succeeded (and healthy, with `healthy`).
    Updated {
        service: String,
        version: String,
        #[serde(default)]
        healthy: bool,
    },
    /// Every host of the repetition reports `service` with this health.
    Health { service: String, health: Health },
    /// Every host of the repetition is live.
    Live,
    /// The coordinator went away and came back: a failed call or a host no longer listed live,
    /// then every host of the repetition listed live again with a newer heartbeat.
    Back,
    /// An audit entry of one of `actions` (and by `by_name`, when set) after the last one seen.
    Audit {
        actions: Vec<String>,
        #[serde(default)]
        by_name: Option<String>,
    },
    /// The host the last audit entry named has a first heartbeat and a first serving stamp.
    Beating,
    /// A Mac the MDM gate did not list when the watch started has enrolled.
    MdmEnrolled,
}

/// A service's health as the coordinator lists it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Health {
    Healthy,
    Unhealthy,
}

impl Health {
    /// The gateway's name of the value.
    pub fn gateway(self) -> &'static str {
        match self {
            Self::Healthy => "HEALTH_HEALTHY",
            Self::Unhealthy => "HEALTH_UNHEALTHY",
        }
    }
}

/// What a watch's timeout does.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OnTimeout {
    #[default]
    EndRepetition,
    Continue,
}

/// A number the analysis computes in each repetition.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "RawMeasure")]
pub struct Measure {
    pub id: String,
    /// What the number is, in one line.
    pub says: String,
    pub unit: String,
    pub kind: Kind,
}

/// How a measure is computed from the events of a repetition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The time from the event `from` to the event `to` (`step/event`), in seconds.
    Span {
        from: String,
        to: String,
        pick: Choose,
    },
    /// How many `of` events there were.
    Count { of: String },
    /// Per host, the longest time between two `of` events of that host, in seconds.
    Gap { of: String },
}

/// A measure as the file spells it: `kind` names how it is computed, beside its id.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum RawMeasure {
    Span {
        id: String,
        says: String,
        unit: String,
        from: String,
        to: String,
        pick: Choose,
    },
    Count {
        id: String,
        says: String,
        unit: String,
        of: String,
    },
    Gap {
        id: String,
        says: String,
        unit: String,
        of: String,
    },
}

impl From<RawMeasure> for Measure {
    fn from(raw: RawMeasure) -> Self {
        match raw {
            RawMeasure::Span {
                id,
                says,
                unit,
                from,
                to,
                pick,
            } => Self {
                id,
                says,
                unit,
                kind: Kind::Span { from, to, pick },
            },
            RawMeasure::Count { id, says, unit, of } => Self {
                id,
                says,
                unit,
                kind: Kind::Count { of },
            },
            RawMeasure::Gap { id, says, unit, of } => Self {
                id,
                says,
                unit,
                kind: Kind::Gap { of },
            },
        }
    }
}

/// Which `to` events a span reads.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Choose {
    /// The earliest: one number per repetition.
    First,
    /// The latest: one number per repetition.
    Last,
    /// Each: one number per host, from the `from` event of that host when there is one.
    Each,
}

/// The statistics a statement may name: `{measure.n}` and the rest.
pub const STATS: [&str; 5] = ["n", "missing", "min", "median", "max"];

/// Why a plan is refused.
#[derive(Debug, Error)]
pub enum PlanError {
    #[error("the plan is not TOML of the plan's shape: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{what} {id:?} is not 1 to 32 bytes of [a-z0-9-] starting with a letter")]
    BadId { what: &'static str, id: String },
    #[error("{what} {id:?} is declared twice")]
    Twice { what: &'static str, id: String },
    #[error("{what} is empty")]
    Empty { what: String },
    #[error("{what} holds a tab or a line break: {text:?}")]
    NotOneLine { what: String, text: String },
    #[error("the plan has no repetitions")]
    NoRepetitions,
    #[error("the hosts: {0}")]
    Hosts(&'static str),
    #[error("step {step:?}: {why}")]
    Step { step: String, why: String },
    #[error("step {step:?}: {source}")]
    Template {
        step: String,
        #[source]
        source: TemplateError,
    },
    #[error("measure {measure:?}: {why}")]
    Measure { measure: String, why: String },
    #[error("statement {index}: {why}")]
    Statement { index: usize, why: String },
}

impl Plan {
    /// Reads and checks a plan.
    pub fn parse(text: &str) -> Result<Self, PlanError> {
        let plan: Self = toml::from_str(text)?;
        plan.check()?;
        Ok(plan)
    }

    /// The events a step records; [`REPETITION`] records `started` and `ended`.
    pub fn events_of(&self, step: &str) -> Option<&'static [&'static str]> {
        if step == REPETITION {
            return Some(&["started", "ended"]);
        }
        let step = self.steps.iter().find(|s| s.id == step)?;
        Some(match &step.action {
            Action::Call { .. } => &["called", "call-failed"],
            Action::Operator { .. } => &["done"],
            Action::Watch(w) => match w.until {
                Until::Updated { .. } => &["ordered", "updated", "call-failed", "timed-out"],
                Until::Health { .. } => &["health", "call-failed", "timed-out"],
                Until::Live => &["live", "call-failed", "timed-out"],
                Until::Back => &[
                    "restart-seen",
                    "call-failed",
                    "answered",
                    "heartbeat",
                    "back",
                    "timed-out",
                ],
                Until::Audit { .. } => &["seen", "call-failed", "timed-out"],
                Until::Beating => &[
                    "first-heartbeat",
                    "first-serving",
                    "call-failed",
                    "timed-out",
                ],
                Until::MdmEnrolled => &["enrolled", "call-failed", "timed-out"],
            },
        })
    }

    /// Whether a repetition reads the audit: a call stamped by its entry, or an audit watch.
    pub fn reads_audit(&self) -> bool {
        self.steps.iter().any(|s| match &s.action {
            Action::Call { audit, .. } => audit.is_some(),
            Action::Watch(w) => matches!(w.until, Until::Audit { .. }),
            Action::Operator { .. } => false,
        })
    }

    fn check(&self) -> Result<(), PlanError> {
        if !id_ok(&self.id, 32) {
            return Err(bad_id("the plan id", &self.id));
        }
        for (what, text) in [
            ("the title", &self.title),
            ("measures", &self.measures),
            ("records", &self.records),
            ("the hosts' says", &self.hosts.says),
        ] {
            prose(what, text)?;
        }
        one_line("the title", &self.title)?;
        if self.repetitions == 0 {
            return Err(PlanError::NoRepetitions);
        }
        self.check_hosts()?;
        let params = self.check_params()?;
        self.check_steps(&params)?;
        self.check_measures()?;
        self.check_states()
    }

    fn check_hosts(&self) -> Result<(), PlanError> {
        match (self.hosts.pick, &self.hosts.service) {
            (Pick::Service, None) => Err(PlanError::Hosts("pick service names no service")),
            (Pick::Live | Pick::None, Some(_)) => Err(PlanError::Hosts(
                "a service is named for a pick of no service",
            )),
            (Pick::None, None) if self.hosts.exactly.is_some() => {
                Err(PlanError::Hosts("a pick of none has no number of hosts"))
            }
            _ if self.hosts.exactly == Some(0) => {
                Err(PlanError::Hosts("exactly zero hosts measure nothing"))
            }
            _ => Ok(()),
        }
    }

    fn check_params(&self) -> Result<BTreeSet<&str>, PlanError> {
        let mut names = BTreeSet::new();
        for param in &self.params {
            if !param_name_ok(&param.name) || matches!(param.name.as_str(), "host" | "rep") {
                return Err(bad_id("parameter", &param.name));
            }
            if !names.insert(param.name.as_str()) {
                return Err(twice("parameter", &param.name));
            }
            prose(&format!("parameter {:?}", param.name), &param.says)?;
        }
        Ok(names)
    }

    fn check_steps(&self, params: &BTreeSet<&str>) -> Result<(), PlanError> {
        if self.steps.is_empty() {
            return Err(PlanError::Empty {
                what: "the steps".to_owned(),
            });
        }
        let mut ids = BTreeSet::new();
        // A host is known from the start with exactly one host, else only after an audit watch.
        let mut host_known = self.hosts.exactly == Some(1);
        for step in &self.steps {
            if !id_ok(&step.id, 32) || step.id == REPETITION {
                return Err(bad_id("step", &step.id));
            }
            if !ids.insert(step.id.as_str()) {
                return Err(twice("step", &step.id));
            }
            let fail = |why: &str| PlanError::Step {
                step: step.id.clone(),
                why: why.to_owned(),
            };
            for text in step_texts(&step.action) {
                for name in template::slots(text).map_err(|source| PlanError::Template {
                    step: step.id.clone(),
                    source,
                })? {
                    let slot = Slot::of(&name)
                        .ok_or_else(|| fail(&format!("{{{name}}} is not a slot of a step")))?;
                    if let Some(param) = slot.param().filter(|p| !params.contains(p)) {
                        return Err(fail(&format!("{{{name}}} names no parameter {param:?}")));
                    }
                    if slot == Slot::Host && !host_known {
                        return Err(fail("{host} before the plan knows its host"));
                    }
                }
            }
            match &step.action {
                Action::Call { args, audit, .. } => {
                    if args.is_empty() {
                        return Err(fail("a call with no arguments"));
                    }
                    if audit.as_deref().is_some_and(|a| !action_ok(a)) {
                        return Err(fail("its audit action is not 1 to 64 bytes of [a-z_]"));
                    }
                }
                Action::Operator { says } => prose(&format!("step {:?}", step.id), says)?,
                Action::Watch(w) => {
                    if w.poll_secs == 0 || w.timeout_secs < w.poll_secs {
                        return Err(fail(
                            "a watch polls at least every second, within its timeout",
                        ));
                    }
                    match &w.until {
                        Until::Updated { .. }
                        | Until::Health { .. }
                        | Until::Live
                        | Until::Back
                            if self.hosts.pick == Pick::None =>
                        {
                            return Err(fail("a watch of the plan's hosts in a plan with none"));
                        }
                        Until::Audit { actions, .. } => {
                            if actions.is_empty() || actions.iter().any(|a| a.is_empty()) {
                                return Err(fail("an audit watch with no action"));
                            }
                            host_known = true;
                        }
                        Until::Beating if !host_known => {
                            return Err(fail("a beating watch before an audit watch"));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn check_measures(&self) -> Result<(), PlanError> {
        if self.measurements.is_empty() {
            return Err(PlanError::Empty {
                what: "the measures".to_owned(),
            });
        }
        let mut ids = BTreeSet::new();
        for m in &self.measurements {
            if !id_ok(&m.id, 32) {
                return Err(bad_id("measure", &m.id));
            }
            if !ids.insert(m.id.as_str()) {
                return Err(twice("measure", &m.id));
            }
            let what = format!("measure {:?}", m.id);
            prose(&what, &m.says)?;
            one_line(&what, &m.says)?;
            prose(&format!("{what}'s unit"), &m.unit)?;
            if m.unit.contains(' ') {
                return Err(PlanError::Measure {
                    measure: m.id.clone(),
                    why: "its unit is more than one word".to_owned(),
                });
            }
            let refs: Vec<&String> = match &m.kind {
                Kind::Span { from, to, .. } => vec![from, to],
                Kind::Count { of } | Kind::Gap { of } => vec![of],
            };
            for r in refs {
                self.check_ref(&m.id, r)?;
            }
        }
        Ok(())
    }

    fn check_ref(&self, measure: &str, r: &str) -> Result<(), PlanError> {
        let fail = |why: String| PlanError::Measure {
            measure: measure.to_owned(),
            why,
        };
        let (step, event) = r
            .split_once('/')
            .ok_or_else(|| fail(format!("{r:?} is not step/event")))?;
        let events = self
            .events_of(step)
            .ok_or_else(|| fail(format!("{r:?}: no step {step:?}")))?;
        if events.contains(&event) {
            Ok(())
        } else {
            Err(fail(format!(
                "{r:?}: step {step:?} records no event {event:?}"
            )))
        }
    }

    fn check_states(&self) -> Result<(), PlanError> {
        if self.states.is_empty() {
            return Err(PlanError::Empty {
                what: "the statements".to_owned(),
            });
        }
        for (index, text) in self.states.iter().enumerate() {
            let fail = |why: String| PlanError::Statement { index, why };
            one_line(&format!("statement {index}"), text)?;
            for slot in template::slots(text).map_err(|e| fail(e.to_string()))? {
                if slot == "plan" || slot == "run" {
                    continue;
                }
                let known = slot.split_once('.').is_some_and(|(id, stat)| {
                    STATS.contains(&stat) && self.measurements.iter().any(|m| m.id == id)
                });
                if !known {
                    return Err(fail(format!("{{{slot}}} is no measure's statistic")));
                }
            }
        }
        Ok(())
    }
}

/// The texts of a step whose slots the runner fills.
pub fn step_texts(action: &Action) -> Vec<&str> {
    match action {
        Action::Call { args, request, .. } => {
            let mut out: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Some(r) = request {
                json_strings(r, &mut out);
            }
            out
        }
        Action::Operator { says } => vec![says],
        Action::Watch(w) => match &w.until {
            Until::Updated {
                service, version, ..
            } => vec![service, version],
            Until::Health { service, .. } => vec![service],
            Until::Live
            | Until::Back
            | Until::Audit { .. }
            | Until::Beating
            | Until::MdmEnrolled => {
                vec![]
            }
        },
    }
}

fn json_strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(items) => items.iter().for_each(|v| json_strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| json_strings(v, out)),
        _ => {}
    }
}

/// An audit action as `hkctl audit list --action` takes it.
fn action_ok(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= 64
        && action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

fn prose(what: &str, text: &str) -> Result<(), PlanError> {
    if text.trim().is_empty() {
        return Err(PlanError::Empty {
            what: what.to_owned(),
        });
    }
    if text.contains('\t') {
        return Err(PlanError::NotOneLine {
            what: what.to_owned(),
            text: text.to_owned(),
        });
    }
    Ok(())
}

fn one_line(what: &str, text: &str) -> Result<(), PlanError> {
    if text.contains(['\n', '\r', '\t']) {
        return Err(PlanError::NotOneLine {
            what: what.to_owned(),
            text: text.to_owned(),
        });
    }
    Ok(())
}

fn bad_id(what: &'static str, id: &str) -> PlanError {
    PlanError::BadId {
        what,
        id: id.to_owned(),
    }
}

fn twice(what: &'static str, id: &str) -> PlanError {
    PlanError::Twice {
        what,
        id: id.to_owned(),
    }
}
