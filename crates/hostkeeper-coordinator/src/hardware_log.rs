//! Each host's hardware event log (`docs/design/host-health-alerts.md`, section 2): one log per
//! host, kept by the coordinator, with one schema on Linux and on a Mac. A record says what part of
//! the machine, how bad, from where, in a sentence a person reads, and the line the source printed.
//!
//! A record is keyed by its host and its `event_id`, and written by a conditional insert on that
//! key (`HardwareLog::write`): a record a daemon sends again, or that two coordinators receive,
//! is written once, and a record is never changed or removed. The log is kept in the database's
//! table `hardware_events` (migration `0033`, `crate::store::postgres::hardware_rows`), in the file
//! `<state-file>.hardware-events` beside the state file (`crate::hardware_files`), or, for a
//! coordinator with neither, in memory (`MemoryLog`).
//!
//! `Hardware.Log` ([`Coordinator::hardware_log`]) lists it newest first: by the time the source
//! gives, then by host id and `event_id`, both descending, each compared byte by byte, so every
//! store gives one order. A page token names the last record of a page as
//! `<time_unix>.<host id>.<event_id>` (a host id holds no `.`), and the next page starts after it.
//!
//! The coordinator writes a host's records with [`Coordinator::write_hardware_events`], from the
//! host's `HostHealth`. No heartbeat calls it yet: no daemon sends health until a coordinator
//! advertises `host-health-v1` ([`crate::host_health`]).
//!
//! A record's component, severity and state are kept as tokens (`memory`, `warning`, `asserted`):
//! the names of their values in `hostkeeper.agent.v1` without the prefix, in lower case, and
//! `unspecified` for a value this release does not know. A token this release does not know (a
//! later release's) is answered unspecified, and the record is shown by its `message`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_proto::health::{HealthError, MAX_EVENT_ID, check_health};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;
use serde::{Deserialize, Serialize};

use crate::inventory::{RecordIndex, is_serial};
use crate::operator::Caller;
use crate::service::Coordinator;

/// The most records one `Hardware.Log` answers: a `limit` of 0, or above it, is this.
pub const MAX_LOG_PAGE: usize = 256;

/// The prefix of the names of `HardwareComponent`'s values.
const COMPONENT: &str = "HARDWARE_COMPONENT_";
/// The prefix of the names of `HardwareSeverity`'s values.
const SEVERITY: &str = "HARDWARE_SEVERITY_";
/// The prefix of the names of `HardwareEventState`'s values.
const STATE: &str = "HARDWARE_EVENT_STATE_";

/// The severities, least bad first: those at or after one are at least as bad.
const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];

/// One record of a host's hardware event log, as every store keeps it
/// (`docs/design/host-health-alerts.md`, section 2.1). A file holds it as one JSON line; a field a
/// later release adds is ignored by this one, and a field this one adds and an earlier line lacks
/// reads as its default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// The host.
    pub(crate) host_id: String,
    /// The source and its own id: the record's key, with the host.
    pub(crate) event_id: String,
    /// The machine's hardware serial, from the host's inventory record; empty for none.
    #[serde(default)]
    pub(crate) machine_serial: String,
    /// When the source says it happened.
    pub(crate) time_unix: u64,
    /// When the coordinator first had it.
    pub(crate) observed_unix: u64,
    /// The boot it was read in; empty for none.
    #[serde(default)]
    pub(crate) boot_id: String,
    /// Whether the source says it happened before that boot.
    #[serde(default)]
    pub(crate) of_previous_boot: bool,
    /// The part of the machine, a token (`memory`).
    pub(crate) component: String,
    /// Which one (`DIMM D1`).
    #[serde(default)]
    pub(crate) subject: String,
    /// How bad, a token (`critical`).
    pub(crate) severity: String,
    /// Whether it happened, or a condition began or ended, a token (`asserted`).
    pub(crate) state: String,
    /// The source (`sel`).
    pub(crate) source: String,
    /// The normalized fault (`memory_uncorrectable`).
    pub(crate) code: String,
    /// One line a person reads.
    pub(crate) message: String,
    /// The source's own line.
    #[serde(default)]
    pub(crate) raw: String,
    /// The `event_id` of the record it is about; empty for none.
    #[serde(default)]
    pub(crate) about: String,
    /// For an operator's note, the operator; empty otherwise.
    #[serde(default)]
    pub(crate) by: String,
}

/// The token of the enum value `name` (`HARDWARE_COMPONENT_MEMORY`) under `prefix`: `memory`.
fn token_of(name: &str, prefix: &str) -> String {
    name.strip_prefix(prefix)
        .unwrap_or(name)
        .to_ascii_lowercase()
}

/// The name of the enum value whose token under `prefix` is `token`: what `from_str_name` reads.
fn name_of(token: &str, prefix: &str) -> String {
    format!("{prefix}{}", token.to_ascii_uppercase())
}

/// The token of the component `value`; `unspecified` for a value this release does not know.
fn component_token(value: i32) -> String {
    let known =
        agent::HardwareComponent::try_from(value).unwrap_or(agent::HardwareComponent::Unspecified);
    token_of(known.as_str_name(), COMPONENT)
}

/// The token of the severity `value`; `unspecified` for a value this release does not know.
fn severity_token(value: i32) -> String {
    let known =
        agent::HardwareSeverity::try_from(value).unwrap_or(agent::HardwareSeverity::Unspecified);
    token_of(known.as_str_name(), SEVERITY)
}

/// The token of the state `value`; `unspecified` for a value this release does not know.
fn state_token(value: i32) -> String {
    let known = agent::HardwareEventState::try_from(value)
        .unwrap_or(agent::HardwareEventState::Unspecified);
    token_of(known.as_str_name(), STATE)
}

impl Record {
    /// The record of `event`, as `host`, whose machine's serial is `serial`, read it in the boot
    /// `boot_id`, first had at `observed_unix`.
    pub(crate) fn of(
        host: &HostId,
        serial: &str,
        boot_id: &str,
        observed_unix: u64,
        event: &agent::HardwareEvent,
    ) -> Record {
        Record {
            host_id: host.as_str().to_owned(),
            event_id: event.event_id.clone(),
            machine_serial: serial.to_owned(),
            time_unix: event.time_unix,
            observed_unix,
            boot_id: boot_id.to_owned(),
            of_previous_boot: event.of_previous_boot,
            component: component_token(event.component),
            subject: event.subject.clone(),
            severity: severity_token(event.severity),
            state: state_token(event.state),
            source: event.source.clone(),
            code: event.code.clone(),
            message: event.message.clone(),
            raw: event.raw.clone(),
            about: event.about.clone(),
            by: String::new(),
        }
    }

    /// The record as `Hardware.Log` answers it: a token this release does not know is
    /// unspecified.
    pub(crate) fn to_proto(&self) -> proto::HardwareRecord {
        let component =
            agent::HardwareComponent::from_str_name(&name_of(&self.component, COMPONENT))
                .unwrap_or(agent::HardwareComponent::Unspecified);
        let severity = agent::HardwareSeverity::from_str_name(&name_of(&self.severity, SEVERITY))
            .unwrap_or(agent::HardwareSeverity::Unspecified);
        let state = agent::HardwareEventState::from_str_name(&name_of(&self.state, STATE))
            .unwrap_or(agent::HardwareEventState::Unspecified);
        proto::HardwareRecord {
            host_id: self.host_id.clone(),
            machine_serial: self.machine_serial.clone(),
            observed_unix: self.observed_unix,
            boot_id: self.boot_id.clone(),
            by: self.by.clone(),
            event: Some(agent::HardwareEvent {
                event_id: self.event_id.clone(),
                time_unix: self.time_unix,
                of_previous_boot: self.of_previous_boot,
                component: component.into(),
                subject: self.subject.clone(),
                severity: severity.into(),
                state: state.into(),
                source: self.source.clone(),
                code: self.code.clone(),
                message: self.message.clone(),
                raw: self.raw.clone(),
                about: self.about.clone(),
            }),
        }
    }

    /// Where the record stands in the log's order.
    pub(crate) fn place(&self) -> Place {
        Place {
            time_unix: self.time_unix,
            host_id: self.host_id.clone(),
            event_id: self.event_id.clone(),
        }
    }
}

/// Where a record stands in the log's order: the log lists the greatest first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Place {
    /// When the source says it happened.
    pub(crate) time_unix: u64,
    /// The host.
    pub(crate) host_id: String,
    /// The record's id.
    pub(crate) event_id: String,
}

impl Place {
    /// The page token that names this place.
    pub(crate) fn token(&self) -> String {
        format!("{}.{}.{}", self.time_unix, self.host_id, self.event_id)
    }

    /// The place `token` names, if it is a token this coordinator gives: decimal digits, a host
    /// id and an `event_id` of 1 to 64 bytes, joined by `.`.
    pub(crate) fn parse(token: &str) -> Option<Place> {
        let (time, rest) = token.split_once('.')?;
        let (host, event_id) = rest.split_once('.')?;
        let digits = !time.is_empty() && time.bytes().all(|b| b.is_ascii_digit());
        let time_unix = time.parse::<u64>().ok().filter(|_| digits)?;
        let host = host.parse::<HostId>().ok()?;
        let sized = (1..=MAX_EVENT_ID).contains(&event_id.len());
        sized.then(|| Place {
            time_unix,
            host_id: host.as_str().to_owned(),
            event_id: event_id.to_owned(),
        })
    }
}

/// What a page of the log asks for: the records that match every filter set, newest first, after
/// a place, at most `limit`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Query {
    /// Only this host's records.
    pub(crate) host_id: Option<String>,
    /// Only the records of the machine with this serial.
    pub(crate) serial: Option<String>,
    /// Only the records the source says happened at or after this time.
    pub(crate) since_unix: u64,
    /// Only the records of this component, a token.
    pub(crate) component: Option<String>,
    /// Only the records of these severities, tokens; every record when empty.
    pub(crate) severities: Vec<String>,
    /// Only the records after this place in the log's order (older, or equal in time and lower in
    /// host id and `event_id`).
    pub(crate) after: Option<Place>,
    /// The most records.
    pub(crate) limit: usize,
}

impl Query {
    /// Whether `record` matches every filter of the query, its place included.
    pub(crate) fn matches(&self, record: &Record) -> bool {
        self.host_id.as_ref().is_none_or(|h| *h == record.host_id)
            && self
                .serial
                .as_ref()
                .is_none_or(|s| *s == record.machine_serial)
            && record.time_unix >= self.since_unix
            && self
                .component
                .as_ref()
                .is_none_or(|c| *c == record.component)
            && (self.severities.is_empty() || self.severities.contains(&record.severity))
            && self
                .after
                .as_ref()
                .is_none_or(|after| record.place() < *after)
    }

    /// The page of `records` the query asks for, newest first.
    pub(crate) fn page_of<'a>(&self, records: impl Iterator<Item = &'a Record>) -> Vec<Record> {
        let mut page: Vec<&Record> = records.filter(|record| self.matches(record)).collect();
        page.sort_by_key(|record| std::cmp::Reverse(record.place()));
        page.into_iter().take(self.limit).cloned().collect()
    }
}

/// Where a coordinator keeps the hardware event log.
pub(crate) trait HardwareLog: Send + Sync {
    /// Writes each of `records` whose host and `event_id` the log does not hold yet, and leaves
    /// the one it holds as it is: durable when this returns Ok, all of them or none.
    ///
    /// # Errors
    /// Why they could not be written: none of them is.
    fn write(&self, records: &[Record]) -> Result<(), String>;

    /// The page `query` asks for, newest first.
    ///
    /// # Errors
    /// Why the log could not be read.
    fn page(&self, query: &Query) -> Result<Vec<Record>, String>;
}

/// The records of a log, and their keys.
#[derive(Debug, Default)]
pub(crate) struct Held {
    records: Vec<Record>,
    keys: BTreeSet<(String, String)>,
}

impl Held {
    /// The records of `records` whose key is held neither already nor earlier in `records`.
    pub(crate) fn new_of(&self, records: &[Record]) -> Vec<Record> {
        let mut keys = BTreeSet::new();
        records
            .iter()
            .filter(|record| {
                let key = (record.host_id.clone(), record.event_id.clone());
                !self.keys.contains(&key) && keys.insert(key)
            })
            .cloned()
            .collect()
    }

    /// Holds `record`, unless its key is held already.
    pub(crate) fn hold(&mut self, record: Record) {
        if self
            .keys
            .insert((record.host_id.clone(), record.event_id.clone()))
        {
            self.records.push(record);
        }
    }

    /// The page `query` asks for.
    pub(crate) fn page(&self, query: &Query) -> Vec<Record> {
        query.page_of(self.records.iter())
    }
}

/// The log in this process alone: a coordinator with neither a database nor a state file (a
/// test's).
#[derive(Debug, Default)]
pub(crate) struct MemoryLog(Mutex<Held>);

impl HardwareLog for MemoryLog {
    fn write(&self, records: &[Record]) -> Result<(), String> {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for record in held.new_of(records) {
            held.hold(record);
        }
        Ok(())
    }

    fn page(&self, query: &Query) -> Result<Vec<Record>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .page(query))
    }
}

/// Why a host's hardware records were not written.
#[derive(Debug, thiserror::Error)]
pub enum HardwareLogError {
    /// The host's health breaks a bound of `hostkeeper.agent.v1.HostHealth`: none of its records
    /// is written.
    #[error("the host's health is refused: {0}")]
    Refused(#[from] HealthError),
    /// The enrollment store could not be read for the host's inventory record, or the log could
    /// not be written: none of its records is written, and a daemon sends them again.
    #[error("the hardware event log could not be written: {0}")]
    Store(String),
}

/// The query of `request`; none when a filter or the page token is not one this coordinator
/// takes.
fn query_of(request: &proto::HardwareLogRequest) -> Option<Query> {
    let host_id = match &request.host_id {
        Some(host) => Some(host.parse::<HostId>().ok()?.as_str().to_owned()),
        None => None,
    };
    let serial = match &request.serial {
        Some(serial) if is_serial(serial) => Some(serial.clone()),
        Some(_) => return None,
        None => None,
    };
    let component = match agent::HardwareComponent::try_from(request.component).ok()? {
        agent::HardwareComponent::Unspecified => None,
        known => Some(token_of(known.as_str_name(), COMPONENT)),
    };
    let severities = match agent::HardwareSeverity::try_from(request.severity).ok()? {
        agent::HardwareSeverity::Unspecified => Vec::new(),
        known => {
            let least = token_of(known.as_str_name(), SEVERITY);
            SEVERITIES
                .iter()
                .skip_while(|token| **token != least)
                .map(|token| (*token).to_owned())
                .collect()
        }
    };
    let after = match request.page_token.as_str() {
        "" => None,
        token => Some(Place::parse(token)?),
    };
    let limit = match request.limit as usize {
        0 => MAX_LOG_PAGE,
        limit => limit.min(MAX_LOG_PAGE),
    };
    Some(Query {
        host_id,
        serial,
        since_unix: request.since_unix,
        component,
        severities,
        after,
        limit,
    })
}

impl Coordinator {
    /// The same coordinator keeping the hardware event log in `log` instead of in its memory.
    #[must_use]
    pub(crate) fn with_hardware_log(mut self, log: Arc<dyn HardwareLog>) -> Self {
        self.hardware = log;
        self
    }

    /// `Hardware.Log`: the records of the hardware event log that match every filter of
    /// `request`, newest first, after the record its page token names, at most its `limit`
    /// ([`MAX_LOG_PAGE`] for 0 or above it), and the token of the next page (the last record of
    /// this one; empty on the last page). Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a host id that is no node id, a serial an
    /// inventory record cannot hold, a component or severity this release does not know, a page
    /// token this coordinator does not give), `store_failed` (the log cannot be read).
    pub fn hardware_log(
        &self,
        caller: &Caller,
        request: &proto::HardwareLogRequest,
    ) -> Result<proto::HardwareLogResponse, proto::HardwareRefusal> {
        caller.operator()?;
        let query = query_of(request).ok_or(proto::HardwareRefusal::InvalidRequest)?;
        let limit = query.limit;
        let mut page = self
            .hardware
            .page(&Query {
                limit: limit + 1,
                ..query
            })
            .map_err(|why| {
                tracing::error!(%why, "the hardware event log could not be read");
                proto::HardwareRefusal::StoreFailed
            })?;
        let more = page.len() > limit;
        page.truncate(limit);
        let next_page_token = match page.last() {
            Some(last) if more => last.place().token(),
            _ => String::new(),
        };
        Ok(proto::HardwareLogResponse {
            records: page.iter().map(Record::to_proto).collect(),
            next_page_token,
        })
    }

    /// Writes the records of `health`, which `host` reported, to its hardware event log: each with
    /// the host, the serial of the inventory record its enrollment matches (empty when none does,
    /// or without enrollment), the boot of `health` and this coordinator's time now. A record the
    /// log holds already, by its host and `event_id`, is left as it is. Every record of `health`
    /// is in the log when this returns Ok.
    ///
    /// # Errors
    /// [`HardwareLogError::Refused`]: `health` breaks a bound
    /// ([`hostkeeper_proto::health::check_health`]). [`HardwareLogError::Store`]: the enrollment
    /// store or the log could not be read or written. Nothing is written either way.
    pub fn write_hardware_events(
        &self,
        host: &HostId,
        health: &agent::HostHealth,
    ) -> Result<(), HardwareLogError> {
        check_health(health)?;
        if health.events.is_empty() {
            return Ok(());
        }
        let serial = self.machine_serial(host)?;
        let observed = self.ops.unix_now();
        let records: Vec<Record> = health
            .events
            .iter()
            .map(|event| Record::of(host, &serial, &health.boot_id, observed, event))
            .collect();
        self.hardware
            .write(&records)
            .map_err(HardwareLogError::Store)
    }

    /// The serial of the inventory record `host`'s enrollment matches; empty when none does, or
    /// without enrollment.
    fn machine_serial(&self, host: &HostId) -> Result<String, HardwareLogError> {
        let enrolled = self
            .enrolled_host(host.as_str())
            .map_err(|why| HardwareLogError::Store(why.to_string()))?;
        let Some(enrolled) = enrolled else {
            return Ok(String::new());
        };
        Ok(self.ops.read(|state| {
            RecordIndex::new(&state.inventory)
                .matched(&enrolled.claims)
                .map(|record| record.serial.clone())
                .unwrap_or_default()
        }))
    }
}

#[cfg(test)]
#[path = "hardware_log_tests.rs"]
pub(crate) mod tests;
