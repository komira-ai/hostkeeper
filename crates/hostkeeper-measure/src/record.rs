//! The raw records of a run: `records.jsonl`, one JSON object per line and per event, in the
//! order the runner saw the events, and `run.json`, which names the plan and the run.
//!
//! Every event has `runner_ms`, the runner's clock (unix milliseconds) when it saw the event, and
//! `coordinator_unix` when the coordinator stamped the event itself: an audit entry's `atUnix`,
//! or a host's heartbeat, enrollment or serving stamp. A host is named `host-<n>`, numbered in the
//! order the run first saw it; the node ids stay in the run directory's `hosts.tsv`, which is not
//! a result and is never committed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One event of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The repetition, from 1.
    pub rep: u32,
    /// The step's id, or `repetition` for the start and end of a repetition.
    pub step: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub runner_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub detail: BTreeMap<String, String>,
}

/// `run.json`: which plan was run, under which run id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunInfo {
    pub plan: String,
    pub run: String,
    pub repetitions: u32,
}

/// A line of `records.jsonl` that is not a record.
#[derive(Debug, Error)]
#[error("records line {line}: {source}")]
pub struct RecordError {
    pub line: usize,
    #[source]
    pub source: serde_json::Error,
}

impl Record {
    /// The record as one line of `records.jsonl`, with its line break.
    pub fn line(&self) -> String {
        let mut line = serde_json::Value::from(self).to_string();
        line.push('\n');
        line
    }
}

impl From<&Record> for serde_json::Value {
    fn from(r: &Record) -> Self {
        let mut map = serde_json::Map::new();
        map.insert("rep".into(), r.rep.into());
        map.insert("step".into(), r.step.clone().into());
        map.insert("event".into(), r.event.clone().into());
        if let Some(h) = &r.host {
            map.insert("host".into(), h.clone().into());
        }
        map.insert("runner_ms".into(), r.runner_ms.into());
        if let Some(c) = r.coordinator_unix {
            map.insert("coordinator_unix".into(), c.into());
        }
        if !r.detail.is_empty() {
            let detail = r
                .detail
                .iter()
                .map(|(k, v)| (k.clone(), Self::from(v.clone())))
                .collect();
            map.insert("detail".into(), Self::Object(detail));
        }
        Self::Object(map)
    }
}

/// Reads `records.jsonl`; blank lines are skipped.
pub fn parse(text: &str) -> Result<Vec<Record>, RecordError> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|source| RecordError {
                line: i + 1,
                source,
            })
        })
        .collect()
}
