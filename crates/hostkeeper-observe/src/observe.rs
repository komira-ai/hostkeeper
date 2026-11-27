//! The observer: one consistent snapshot of a service from the service manager, the running
//! process and the health URL (design section 5.8). Pure logic over the ports; it never sleeps,
//! takes no lock of the daemon's and writes nothing but its own digest cache.
//!
//! One round, in order:
//!
//! 1. No `current`: [`ServiceState::Absent`], and nothing is asked. No `[units] run`, and no
//!    units given for the service ([`Observer::with_run_of`]): [`ServiceState::Unmonitored`]; the
//!    service manager and the process are not asked, and a health URL, if the manifest has one,
//!    is probed once (a `{port}` in it has no instance to be filled from, and is `url_invalid`).
//!    A service whose manifest runs no unit but that was given units is observed through those,
//!    as if its manifest named them (the daemon's own service, whose job the host's installer
//!    places, not its package).
//! 2. Read every run unit. Every one `NotFound`: `Absent`. Any `Activating` or `Deactivating`:
//!    `Transitioning`, even beside an active unit (a hand-over under way is reported as one).
//!    Else any `Active`: `Live` (a standby instance that is inactive by design does not make a
//!    live service stopped). Else `Stopped`.
//! 3. Live only, the version: from the step record, always. The main process of the first
//!    active unit is identified (pid, start time), its executable's digest taken (or found in the
//!    cache under pid, start time, size and modification time), and the process identified
//!    again; if the two identities differ (a restart, or the pid reused) the version part is
//!    discarded as `Unknown`. A digest equal to the sealed one confirms the version; a different
//!    one is `version_mismatch` (a wrapper, or a binary that is not the release's). No main
//!    process, no recorded version: `Unknown`. No sealed digest, or the check switched off: the
//!    recorded version, unconfirmed. The digest never names a version.
//! 4. Live only, the health: one probe per active run instance, `{port}` filled from the number
//!    after the instance's `@`; healthy only if every probe answers 2xx. The first probe that
//!    does not, in run order, gives the reason and, when it answered, the first bytes of its body
//!    ([`Observation::health_body`]).
//! 5. Read every run unit again. If any unit's activity or main pid changed, the round is
//!    discarded as `unobserved`.
//!
//! The restarts of each run unit ([`Observation::restarts`]) are those of the last read of the
//! unit set that succeeded, even in a round discarded after it: a unit in a restart loop changes
//! its main pid between the two reads, and its count must still reach the heartbeat.
//!
//! Any port error makes the round `unobserved` with a reason (`manager_error`, `proc_error`,
//! `health_error`); it is never read as absent, stopped or unhealthy. The one exception is a
//! health URL of an active unit that does not answer: the service is up and not answering, which
//! is `unhealthy` with `unreachable`.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::UnitName;

use crate::error::ObserveError;
use crate::health::MAX_BODY_BYTES;
use crate::ports::{
    ActiveState, Deadline, ExeStat, ExpectedExe, HealthClient, HealthReply, ProcessIdentity,
    ProcessProbe, ServiceManager, UnitRestarts, UnitState,
};
use crate::url::HealthUrl;

/// The placeholder of the health URL that each instance's port replaces.
pub const PORT_PLACEHOLDER: &str = "{port}";

/// What the manifest of `current` says the observer needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current {
    /// `[units] run`.
    pub run: Vec<String>,
    /// `[health] url`, if any.
    pub health_url: Option<String>,
}

impl Current {
    /// The part of a loaded manifest the observer reads.
    pub fn from_manifest(manifest: &Manifest) -> Current {
        Current {
            run: manifest.run.clone(),
            health_url: manifest.health_url.clone(),
        }
    }
}

/// One service to observe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceInput {
    /// The service's name.
    pub service: String,
    /// The manifest of `current`; `None` when the service has no `current`.
    pub current: Option<Current>,
    /// The version the step record says is installed, if it says one.
    pub recorded_version: Option<String>,
}

/// Why a round was discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnobservedReason {
    /// The service manager could not be asked.
    ManagerError,
    /// The main process could not be read.
    ProcError,
    /// A health probe could not be made.
    HealthError,
    /// A unit's activity or main pid changed between the two reads of the unit set.
    ChangedDuringRound,
}

impl UnobservedReason {
    /// The reason as a stable token.
    pub fn token(self) -> &'static str {
        match self {
            UnobservedReason::ManagerError => "manager_error",
            UnobservedReason::ProcError => "proc_error",
            UnobservedReason::HealthError => "health_error",
            UnobservedReason::ChangedDuringRound => "changed_during_round",
        }
    }
}

/// The service's state in one round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// No `current`, or the manager knows none of the run units.
    Absent,
    /// Every run unit is inactive, failed or unknown to the manager.
    Stopped,
    /// A run unit is starting or stopping.
    Transitioning,
    /// The round could not be observed; `detail` says what failed, for the operator.
    Unobserved {
        reason: UnobservedReason,
        detail: String,
    },
    /// The manifest has no run units: there is nothing to observe.
    Unmonitored,
    /// At least one run unit is active.
    Live,
}

impl ServiceState {
    /// The state as a stable token.
    pub fn token(&self) -> &'static str {
        match self {
            ServiceState::Absent => "absent",
            ServiceState::Stopped => "stopped",
            ServiceState::Transitioning => "transitioning",
            ServiceState::Unobserved { .. } => "unobserved",
            ServiceState::Unmonitored => "unmonitored",
            ServiceState::Live => "live",
        }
    }
}

/// What the round says of the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionView {
    /// The version the step record names; `confirmed` when the running executable's digest is
    /// the sealed one, `false` when there was nothing to compare with or the check is off.
    Recorded { version: String, confirmed: bool },
    /// The running executable is not the one sealed for the recorded version.
    Mismatch {
        recorded: String,
        observed_note: String,
    },
    /// Live, but the version cannot be told: no main process, no recorded version, or the
    /// process changed while it was read.
    Unknown,
    /// Not live, or not observed.
    NotLive,
}

/// Why a probed service is unhealthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnhealthyReason {
    /// A status outside 2xx (and outside 3xx).
    StatusNon2xx,
    /// A 3xx: a redirect, not followed.
    Redirect,
    /// An active instance's URL did not answer.
    Unreachable,
    /// The URL cannot be made for an instance: `{port}` with no port number after its `@`, or
    /// a URL [`HealthUrl`] refuses.
    UrlInvalid,
}

impl UnhealthyReason {
    /// The reason as a stable token.
    pub fn token(self) -> &'static str {
        match self {
            UnhealthyReason::StatusNon2xx => "status_non_2xx",
            UnhealthyReason::Redirect => "redirect",
            UnhealthyReason::Unreachable => "unreachable",
            UnhealthyReason::UrlInvalid => "url_invalid",
        }
    }
}

/// What the round says of the health URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthView {
    /// Every active instance answered 2xx.
    Healthy,
    /// An instance did not; the reason of the first, in run order.
    Unhealthy { reason: UnhealthyReason },
    /// No `[health]` section, or the service is neither live nor unmonitored.
    NotProbed,
}

/// The restarts of one run unit, as the service manager counted them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunUnitRestarts {
    /// The unit, as `[units] run` names it.
    pub unit: String,
    pub restarts: UnitRestarts,
}

/// One consistent snapshot of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub service: String,
    pub state: ServiceState,
    pub version: VersionView,
    pub health: HealthView,
    /// The version the step record names, as given.
    pub recorded: Option<String>,
    /// The first bytes of the body of the health URL's answer that made the service unhealthy
    /// (a status that is not 2xx), as text: invalid UTF-8 replaced, every control character a
    /// space, white space trimmed at both ends, at most [`MAX_BODY_BYTES`] bytes cut at a
    /// character's boundary. `None` when the health is not unhealthy by a status, or the body is
    /// empty.
    pub health_body: Option<String>,
    /// The restarts of the run units the manager counts restarts of, in run order, from the
    /// round's last read of the unit set that succeeded; empty when no read did, the service has
    /// no run unit, or the manager keeps no counter.
    pub restarts: Vec<RunUnitRestarts>,
}

impl Observation {
    /// The one token that says why the service is not plainly live, healthy and confirmed, or
    /// `None` when it is. Not live: the state's token (`absent`, `stopped`, `transitioning`,
    /// `unobserved`, `unmonitored`), except that an unmonitored service whose health URL failed
    /// gives the health reason. Live: `version_mismatch`, then the health reason, then
    /// `version_unknown` or `version_unconfirmed`.
    pub fn reason_token(&self) -> Option<&'static str> {
        if self.state != ServiceState::Live {
            return match (&self.state, &self.health) {
                (ServiceState::Unmonitored, HealthView::Unhealthy { reason }) => {
                    Some(reason.token())
                }
                (state, _) => Some(state.token()),
            };
        }
        match (&self.version, &self.health) {
            (VersionView::Mismatch { .. }, _) => Some("version_mismatch"),
            (_, HealthView::Unhealthy { reason }) => Some(reason.token()),
            (VersionView::Unknown, _) => Some("version_unknown"),
            (
                VersionView::Recorded {
                    confirmed: false, ..
                },
                _,
            ) => Some("version_unconfirmed"),
            _ => None,
        }
    }
}

/// What the observer is allowed to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObserveOptions {
    /// Confirm the recorded version by hashing the running executable.
    pub confirm_exe_hash: bool,
}

impl Default for ObserveOptions {
    fn default() -> ObserveOptions {
        ObserveOptions {
            confirm_exe_hash: true,
        }
    }
}

/// The ports one observer reads through.
pub struct Ports {
    pub manager: Box<dyn ServiceManager>,
    pub process: Box<dyn ProcessProbe>,
    pub health: Box<dyn HealthClient>,
    pub expected: Box<dyn ExpectedExe>,
}

/// What a digest in the cache was taken of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExeKey {
    identity: ProcessIdentity,
    stat: ExeStat,
}

/// A round's verdict, or why it was discarded.
type Round<T> = Result<T, (UnobservedReason, String)>;

/// What a round that was not discarded found.
struct Found {
    state: ServiceState,
    version: VersionView,
    health: HealthView,
    body: Option<String>,
}

/// Why one probe was not healthy, and the body of its answer when it gave one.
struct Failure {
    reason: UnhealthyReason,
    body: Option<String>,
}

/// The first bytes of a body as [`Observation::health_body`] holds them.
fn body_text(body: &[u8]) -> Option<String> {
    let text: String = String::from_utf8_lossy(body)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    let text = text[..text.floor_char_boundary(MAX_BODY_BYTES)].trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Observes services; one per daemon, shared by every caller.
pub struct Observer {
    ports: Ports,
    options: ObserveOptions,
    /// Per service, the digest of its last confirmed main executable and what it was taken of.
    cache: Mutex<BTreeMap<String, (ExeKey, [u8; 32])>>,
    /// Per service, the units it runs as when its manifest's `[units] run` names none.
    run_of: BTreeMap<String, Vec<String>>,
}

/// A port error as the reason of a discarded round.
fn because(reason: UnobservedReason) -> impl Fn(ObserveError) -> (UnobservedReason, String) {
    move |e| (reason, e.to_string())
}

/// The port of `service`'s instance `unit`: the digits after its `@`, before the unit type's
/// suffix, 1 to 65535, read by the same rule the manifest loader checks `{port}` with.
fn instance_port(service: &str, unit: &str) -> Option<u16> {
    UnitName::of(service, unit, false)
        .ok()
        .and_then(|name| name.port())
}

/// The state of a run set from one read of its units (never empty).
fn aggregate(units: &[UnitState]) -> ServiceState {
    let any = |wanted: &[ActiveState]| units.iter().any(|u| wanted.contains(&u.active));
    if units.iter().all(|u| u.active == ActiveState::NotFound) {
        ServiceState::Absent
    } else if any(&[ActiveState::Activating, ActiveState::Deactivating]) {
        ServiceState::Transitioning
    } else if any(&[ActiveState::Active]) {
        ServiceState::Live
    } else {
        ServiceState::Stopped
    }
}

/// The health view of a probe set's first failure, and its body: healthy when there is none.
fn judged(first_failure: Option<Failure>) -> (HealthView, Option<String>) {
    match first_failure {
        None => (HealthView::Healthy, None),
        Some(Failure { reason, body }) => (HealthView::Unhealthy { reason }, body),
    }
}

/// What a status code says of health: `None` for 2xx.
fn judge(status: u16) -> Option<UnhealthyReason> {
    match status {
        200..=299 => None,
        300..=399 => Some(UnhealthyReason::Redirect),
        _ => Some(UnhealthyReason::StatusNon2xx),
    }
}

impl Observer {
    /// An observer over `ports`.
    pub fn new(ports: Ports, options: ObserveOptions) -> Observer {
        Observer {
            ports,
            options,
            cache: Mutex::new(BTreeMap::new()),
            run_of: BTreeMap::new(),
        }
    }

    /// The same observer, observing `service` through `units` whenever the manifest of its
    /// `current` names no `[units] run`, exactly as if it named them; a manifest that names any
    /// is read as it is. Given again for the same service, the last `units` stand; given none,
    /// the service is unmonitored as before.
    #[must_use]
    pub fn with_run_of(mut self, service: &str, units: Vec<String>) -> Observer {
        self.run_of.insert(service.to_owned(), units);
        self
    }

    /// Observes `svc` once. Each health probe may wait as long as `deadline` allows.
    pub fn observe(&self, svc: &ServiceInput, deadline: Deadline) -> Observation {
        let mut read = Vec::new();
        let found = self.round(svc, deadline, &mut read);
        let Found {
            state,
            version,
            health,
            body,
        } = found.unwrap_or_else(|(reason, detail)| Found {
            state: ServiceState::Unobserved { reason, detail },
            version: VersionView::NotLive,
            health: HealthView::NotProbed,
            body: None,
        });
        Observation {
            service: svc.service.clone(),
            state,
            version,
            health,
            recorded: svc.recorded_version.clone(),
            health_body: body,
            restarts: read,
        }
    }

    /// One round of `svc`; `read` ends holding the restarts of its last read of the unit set
    /// that succeeded.
    fn round(
        &self,
        svc: &ServiceInput,
        deadline: Deadline,
        read: &mut Vec<RunUnitRestarts>,
    ) -> Round<Found> {
        let quiet = |state| {
            Ok(Found {
                state,
                version: VersionView::NotLive,
                health: HealthView::NotProbed,
                body: None,
            })
        };
        let Some(current) = &svc.current else {
            return quiet(ServiceState::Absent);
        };
        let run = match (current.run.is_empty(), self.run_of.get(&svc.service)) {
            (true, Some(units)) => units.as_slice(),
            _ => current.run.as_slice(),
        };
        if run.is_empty() {
            let (health, body) = match &current.health_url {
                Some(template) => {
                    let failure = self.probe(&svc.service, template, None, deadline)?;
                    judged(failure)
                }
                None => (HealthView::NotProbed, None),
            };
            return Ok(Found {
                state: ServiceState::Unmonitored,
                version: VersionView::NotLive,
                health,
                body,
            });
        }
        let before = self.read_units(run, read)?;
        let state = aggregate(&before);
        let (version, (health, body)) = if state == ServiceState::Live {
            let version = self.version(svc, &before)?;
            (version, self.health(svc, current, run, &before, deadline)?)
        } else {
            (VersionView::NotLive, (HealthView::NotProbed, None))
        };
        let after = self.read_units(run, read)?;
        let changed = run
            .iter()
            .zip(before.iter().zip(&after))
            .find(|(_, (b, a))| (b.active != a.active) | (b.main_pid != a.main_pid));
        if let Some((unit, (b, a))) = changed {
            return Err((
                UnobservedReason::ChangedDuringRound,
                format!(
                    "{unit} changed while it was observed: {:?} pid {:?}, then {:?} pid {:?}",
                    b.active, b.main_pid, a.active, a.main_pid
                ),
            ));
        }
        Ok(Found {
            state,
            version,
            health,
            body,
        })
    }

    /// One read of every unit of `run`; when it succeeds, `read` holds its restarts.
    fn read_units(&self, run: &[String], read: &mut Vec<RunUnitRestarts>) -> Round<Vec<UnitState>> {
        let units = run
            .iter()
            .map(|unit| self.ports.manager.unit_state(unit))
            .collect::<Result<Vec<_>, _>>()
            .map_err(because(UnobservedReason::ManagerError))?;
        *read = run
            .iter()
            .zip(&units)
            .filter_map(|(unit, state)| {
                let restarts = state.restarts.clone()?;
                Some(RunUnitRestarts {
                    unit: unit.clone(),
                    restarts,
                })
            })
            .collect();
        Ok(units)
    }

    /// The version part of a live round.
    fn version(&self, svc: &ServiceInput, units: &[UnitState]) -> Round<VersionView> {
        let Some(recorded) = &svc.recorded_version else {
            return Ok(VersionView::Unknown);
        };
        let main = units
            .iter()
            .find(|u| u.active == ActiveState::Active)
            .and_then(|u| u.main_pid);
        let Some(pid) = main else {
            return Ok(VersionView::Unknown);
        };
        let unconfirmed = VersionView::Recorded {
            version: recorded.clone(),
            confirmed: false,
        };
        if !self.options.confirm_exe_hash {
            return Ok(unconfirmed);
        }
        let Some(sealed) = self.ports.expected.digest(&svc.service, recorded) else {
            return Ok(unconfirmed);
        };
        let proc_error = because(UnobservedReason::ProcError);
        let process = &self.ports.process;
        let first = process.identity(pid).map_err(&proc_error)?;
        let key = ExeKey {
            identity: first,
            stat: process.exe_stat(pid).map_err(&proc_error)?,
        };
        let cached = self.cached(&svc.service, &key);
        let digest = match cached {
            Some(digest) => digest,
            None => process.exe_digest(pid).map_err(&proc_error)?,
        };
        let second = process.identity(pid).map_err(&proc_error)?;
        if first != second {
            return Ok(VersionView::Unknown);
        }
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(svc.service.clone(), (key, digest));
        Ok(if digest == sealed {
            VersionView::Recorded {
                version: recorded.clone(),
                confirmed: true,
            }
        } else {
            VersionView::Mismatch {
                recorded: recorded.clone(),
                observed_note: format!(
                    "the running executable's SHA-256 is {}, not the sealed {}",
                    hex::encode(digest),
                    hex::encode(sealed)
                ),
            }
        })
    }

    /// The cached digest of `service`'s executable, if it was taken of `key`.
    fn cached(&self, service: &str, key: &ExeKey) -> Option<[u8; 32]> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .get(service)
            .filter(|(seen, _)| seen == key)
            .map(|(_, digest)| *digest)
    }

    /// The health part of a live round over the run units `run`, whose states are `units`, and
    /// the body of the first failure.
    fn health(
        &self,
        svc: &ServiceInput,
        current: &Current,
        run: &[String],
        units: &[UnitState],
        deadline: Deadline,
    ) -> Round<(HealthView, Option<String>)> {
        let Some(template) = &current.health_url else {
            return Ok((HealthView::NotProbed, None));
        };
        let mut first_failure = None;
        for (unit, state) in run.iter().zip(units) {
            if state.active == ActiveState::Active {
                let failure = self.probe(&svc.service, template, Some(unit), deadline)?;
                first_failure = first_failure.or(failure);
            }
        }
        Ok(judged(first_failure))
    }

    /// One probe of the instance `unit`, or of the service with no run unit: `None` when it is
    /// healthy. With no unit, a `{port}` has nothing to be filled from and is invalid.
    fn probe(
        &self,
        service: &str,
        template: &str,
        unit: Option<&str>,
        deadline: Deadline,
    ) -> Round<Option<Failure>> {
        let invalid = Failure {
            reason: UnhealthyReason::UrlInvalid,
            body: None,
        };
        let text = if template.contains(PORT_PLACEHOLDER) {
            let Some(port) = unit.and_then(|unit| instance_port(service, unit)) else {
                return Ok(Some(invalid));
            };
            template.replace(PORT_PLACEHOLDER, &port.to_string())
        } else {
            template.to_owned()
        };
        let Ok(url) = HealthUrl::parse(&text) else {
            return Ok(Some(invalid));
        };
        match self.ports.health.get(&url, deadline) {
            Ok(HealthReply { status, body, .. }) => Ok(judge(status).map(|reason| Failure {
                reason,
                body: body_text(&body),
            })),
            Err(ObserveError::Unreachable { .. }) => Ok(Some(Failure {
                reason: UnhealthyReason::Unreachable,
                body: None,
            })),
            Err(other) => Err((UnobservedReason::HealthError, format!("{service}: {other}"))),
        }
    }
}
