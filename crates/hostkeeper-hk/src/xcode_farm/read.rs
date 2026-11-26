//! What the farm check calls and reads, through [`crate::call::ask`] (the transport every call of
//! `hk` uses, one POST of the JSON the gateway takes): `Maintenance.Schedule`, `Maintenance.Get`
//! and `Maintenance.Cancel`, `Hosts.Get` for the probe a Mac reports, and `Xcode.Remove`. The
//! answers are the gateway's JSON; a field that is absent is empty (the gateway leaves a default
//! out), a number of 64 bits is a string or a number, an enum is its name, and a field this does
//! not read is ignored.

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::args::Globals;
use crate::call::{Refused, ask};
use crate::{Flow, Stop, World};

pub(super) const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";
pub(super) const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
pub(super) const CANCEL: &str = "/hostkeeper.operator.v1.Maintenance/Cancel";
pub(super) const HOST: &str = "/hostkeeper.operator.v1.Hosts/Get";
pub(super) const REMOVE: &str = "/hostkeeper.operator.v1.Xcode/Remove";

/// The state of a run that has done every host's turn.
pub(super) const RUN_DONE: &str = "MAINTENANCE_STATE_DONE";
/// The state of a run an operator cancelled.
pub(super) const RUN_CANCELLED: &str = "MAINTENANCE_STATE_CANCELLED";
/// The state of a run whose window ended with hosts not reached.
pub(super) const RUN_WINDOW_ENDED: &str = "MAINTENANCE_STATE_WINDOW_ENDED";

/// A maintenance run, as far as the check reads it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Run {
    pub(super) id: String,
    pub(super) state: String,
    /// The SHA-256 of the archive the run's probes check; empty for a run that makes its reads.
    pub(super) xcode_sha256: String,
}

/// One host's turn of a run.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Turn {
    pub(super) host_id: String,
    pub(super) step: String,
    pub(super) skipped_because: String,
    pub(super) reason: String,
}

/// What `Maintenance.Schedule` answers.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Scheduled {
    pub(super) run: Run,
}

/// What `Maintenance.Get` answers: the run and each host's turn of it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Got {
    pub(super) run: Run,
    pub(super) hosts: Vec<Turn>,
}

/// One step of a probe.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Step {
    pub(super) name: String,
    /// Absent when the program did not exit by itself, or the step runs none.
    pub(super) exit_code: Option<i32>,
    pub(super) output: String,
    pub(super) cut: bool,
}

/// What a Mac's last probe read.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Probe {
    pub(super) run_id: String,
    #[serde(deserialize_with = "number")]
    pub(super) probed_unix: u64,
    pub(super) steps: Vec<Step>,
}

impl Probe {
    /// The step named `name`.
    pub(super) fn step(&self, name: &str) -> Option<&Step> {
        self.steps.iter().find(|step| step.name == name)
    }
}

/// What `Hosts.Get` answers, as far as the probe in the host's maintenance report.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Hosted {
    host: Host,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Host {
    maintenance: Report,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Report {
    probe: Option<Probe>,
}

impl Hosted {
    /// The probe the host reports, when it reports one.
    pub(super) fn into_probe(self) -> Option<Probe> {
        self.host.maintenance.probe
    }
}

/// A 64-bit number the gateway writes as a string, or as a number.
fn number<'de, D: Deserializer<'de>>(from: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    match Either::deserialize(from)? {
        Either::Number(n) => Ok(n),
        Either::Text(text) => text.parse().map_err(serde::de::Error::custom),
    }
}

/// What a refusal says: its reason and its code.
pub(super) fn said(refused: &Refused) -> String {
    format!(
        "{} ({})",
        String::from_utf8_lossy(&refused.reason),
        String::from_utf8_lossy(&refused.code)
    )
}

/// What a stopped call says.
pub(super) fn why(stop: &Stop) -> String {
    match stop {
        Stop::Usage(text) | Stop::Fail(text) => String::from_utf8_lossy(text).into_owned(),
        Stop::Exit(code) => format!("it exited {code}"),
    }
}

/// The call `name` of `path` with the JSON `body`, its answer read as a `T`; or the refusal. A call
/// that got no answer, or one that is not the gateway's, is a failure.
pub(super) fn call<T: DeserializeOwned>(
    w: &mut World<'_>,
    globals: &Globals,
    name: &str,
    path: &str,
    body: &Value,
) -> Flow<Result<T, Refused>> {
    match ask(w, globals, path, body.to_string().as_bytes())? {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Ok).map_err(|error| {
            Stop::fail(format!(
                "the answer of {name} is not the coordinator's JSON ({error})"
            ))
        }),
        Err(refused) => Ok(Err(refused)),
    }
}

/// A call whose answer is not read: `Ok` when the coordinator took it, else why not.
fn done(
    w: &mut World<'_>,
    globals: &Globals,
    name: &str,
    path: &str,
    body: &Value,
) -> Result<(), String> {
    match call::<IgnoredAny>(w, globals, name, path, body) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(refused)) => Err(format!("{name} was refused: {}", said(&refused))),
        Err(stop) => Err(why(&stop)),
    }
}

/// `Maintenance.Cancel` of the run `run`, with `reason`.
pub(super) fn cancel(
    w: &mut World<'_>,
    globals: &Globals,
    run: &str,
    reason: &str,
) -> Result<(), String> {
    done(
        w,
        globals,
        "Maintenance.Cancel",
        CANCEL,
        &json!({"runId": run, "reason": reason}),
    )
}

/// `Xcode.Remove` of the archive `sha256`, with `reason`.
pub(crate) fn remove(
    w: &mut World<'_>,
    globals: &Globals,
    sha256: &str,
    reason: &str,
) -> Result<(), String> {
    done(
        w,
        globals,
        "Xcode.Remove",
        REMOVE,
        &json!({"sha256": sha256, "reason": reason}),
    )
}
