//! The verdicts the coordinator has stored, in `<state-file>.verdicts` (design sections 5.4 and
//! 8.2), and the acks of a heartbeat answer that rest on them.
//!
//! A host resends its newest verdict per service until the coordinator acknowledges it by
//! service, order id and try ([`VerdictAck`], only to an agent that announced [`FEATURE`]).
//! A daemon then stops sending it, so the coordinator keeps it for the host: per service and
//! host, the last verdict it acknowledged (`standing`), and, per service, the verdicts it counts
//! toward an automatic rollback ([`crate::verdicts`], `counted`). Both are written to the file,
//! and the write has returned, before an ack of the verdict is in an answer: a verdict is
//! acknowledged only once it is durable, and an answer whose write failed acknowledges none
//! (the host sends the verdict again and the write is tried again at its next heartbeat).
//!
//! A heartbeat of such an agent whose service entry carries no `last_update` is read as
//! carrying the standing verdict of the host ([`fill`]): every rule that reads a host's report
//! (the gate's `failed_here`, the halt, the bake, the end of an order, the counting) reads what
//! it read while the daemon resent it, and after a restart too, so a restarted coordinator does
//! not order a host the version it failed again, on a new try 1 that holds the fleet's slot.
//! A standing verdict about another version than the one a host runs, once that is the version
//! wanted, is superseded instead: not read, and forgotten (the host reached the version wanted
//! by an order whose own verdict it never sent, as a self-update's binary exits before it
//! reports). A verdict the agent reports replaces the standing one; one with no order id or no try cannot
//! be acknowledged, so it is not stored and ends the standing one. An agent that did not
//! announce the feature is answered and read exactly as before.
//!
//! The file is JSON, written whole (a temporary file, fsync, rename, a directory fsync) under
//! the fleet's lock that every heartbeat is decided under, and only when what it holds changes:
//! a verdict resent changes nothing and writes nothing. It is read when the coordinator starts;
//! a key it does not know is skipped, so a later build may add one, and a value that does not
//! read (a host id, a report) stops the start, naming the file. A file that is not there is
//! none stored: a coordinator of the release before never wrote one, and ignores this one.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Heartbeat, UpdateResult, VerdictAck};
use serde::{Deserialize, Serialize};

use crate::desired::{StoreError, io_at};
use crate::fleet::Fleet;
use crate::store::{BackendError, Commit, Fake, Files, Shared};

/// The feature an agent announces to be acknowledged its verdicts.
pub const FEATURE: &str = hostkeeper_wire::wire::VERDICT_ACKS_FEATURE;

/// The file beside the state file `state`: `<state>.verdicts`.
#[must_use]
pub fn verdicts_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".verdicts");
    PathBuf::from(path)
}

/// Why the file of verdicts was refused, or not written.
#[derive(Debug, thiserror::Error)]
pub enum VerdictsError {
    /// The file could not be read or written.
    #[error(transparent)]
    Io(#[from] StoreError),
    /// The file holds a value this build cannot read.
    #[error(
        "{path}: {why}; the coordinator does not guess which verdicts it acknowledged, so fix or remove the file to start"
    )]
    Invalid {
        /// The file.
        path: PathBuf,
        /// Why.
        why: String,
    },
}

/// What the file holds: by service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Held {
    #[serde(default)]
    services: BTreeMap<String, Kept>,
}

/// The verdicts kept of one service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    /// The last verdict acknowledged to each host, by host id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    standing: BTreeMap<String, UpdateResult>,
    /// The verdicts counted toward an automatic rollback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    counted: Vec<Counted>,
}

impl Kept {
    fn is_empty(&self) -> bool {
        self.standing.is_empty() && self.counted.is_empty()
    }
}

/// One verdict counted: the host and its report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    #[serde(with = "host_text")]
    node_id: HostId,
    report: UpdateResult,
}

/// A host id as the text of its id.
mod host_text {
    use hostkeeper_wire::HostId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(id: &HostId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(id.as_str())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HostId, D::Error> {
        use serde::de::Error;
        let text = String::deserialize(d)?;
        text.parse()
            .map_err(|e| D::Error::custom(format!("host {text:?}: {e}")))
    }
}

/// The verdicts stored, and the backend a change of them is committed to ([`crate::store`]).
pub struct VerdictFile {
    held: Mutex<Held>,
    backend: Shared,
}

impl std::fmt::Debug for VerdictFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerdictFile").finish_non_exhaustive()
    }
}

/// What one heartbeat said of a service's verdict: the service, and the host's `last_update`
/// for it as the heartbeat carried it (before [`fill`]).
pub(crate) type Heard = (String, Option<UpdateResult>);

impl VerdictFile {
    /// A store that keeps the verdicts in memory only, for a coordinator with no state file:
    /// each write is taken as done.
    #[must_use]
    pub fn volatile() -> Self {
        Self::over(Arc::new(Fake::new()), Held::default())
    }

    /// The verdicts stored in the file at `path`, none if it is not there; changes are written
    /// to it.
    ///
    /// # Errors
    /// [`VerdictsError`]: the file cannot be read, or holds a value this build cannot read.
    pub fn open(path: &Path) -> Result<Self, VerdictsError> {
        let files = Files::default().with_verdicts(path);
        let held = files.load_verdicts()?;
        Ok(Self::over(Arc::new(files), held))
    }

    /// The verdicts `held`, as loaded from `backend`, to which their changes are committed.
    pub(crate) fn over(backend: Shared, held: Held) -> Self {
        Self {
            held: Mutex::new(held),
            backend,
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The last verdict about `service` acknowledged to `host`, if one stands.
    pub(crate) fn standing(&self, service: &str, host: &HostId) -> Option<UpdateResult> {
        self.held()
            .services
            .get(service)
            .and_then(|kept| kept.standing.get(host.as_str()))
            .cloned()
    }

    /// Forgets the verdict about `service` that stands for `host`, if one does: a later
    /// heartbeat of the host showed it superseded ([`fill`], which names only a verdict that
    /// stands). Written before it is gone from memory.
    ///
    /// # Errors
    /// [`VerdictsError::Io`]: the file was not written; nothing changed in memory.
    pub(crate) fn supersede(&self, service: &str, host: &HostId) -> Result<(), VerdictsError> {
        let mut held = self.held();
        let mut next = held.clone();
        if let Some(kept) = next.services.get_mut(service) {
            kept.standing.remove(host.as_str());
        }
        next.services.retain(|_, kept| !kept.is_empty());
        (self.write)(&next)?;
        *held = next;
        Ok(())
    }

    /// Every verdict counted: the service, the host and its report.
    pub(crate) fn counted(&self) -> Vec<(String, HostId, UpdateResult)> {
        self.held()
            .services
            .iter()
            .flat_map(|(service, kept)| {
                kept.counted
                    .iter()
                    .map(|one| (service.clone(), one.node_id.clone(), one.report.clone()))
            })
            .collect()
    }

    /// Stores what a heartbeat of `host` said: each verdict `heard` that can be acknowledged
    /// stands for the host in place of the one before, one that cannot ends it, and a service
    /// the host said nothing of keeps its own; `counted` replaces the verdicts counted of each
    /// service it names. Written before it is in memory, and only when it changes something.
    ///
    /// # Errors
    /// [`VerdictsError::Io`]: the file was not written; nothing changed in memory.
    pub(crate) fn record(
        &self,
        host: &HostId,
        heard: &[Heard],
        counted: &[(String, Vec<(HostId, UpdateResult)>)],
    ) -> Result<(), VerdictsError> {
        let mut held = self.held();
        if !changes(&held, host, heard, counted) {
            return Ok(());
        }
        let mut next = held.clone();
        for (service, last) in heard {
            let kept = next.services.entry(service.clone()).or_default();
            match last.as_ref().filter(|report| ackable(report)) {
                Some(report) => {
                    kept.standing
                        .insert(host.as_str().to_owned(), report.clone());
                }
                None if last.is_some() => {
                    kept.standing.remove(host.as_str());
                }
                None => {}
            }
        }
        for (service, verdicts) in counted {
            next.services.entry(service.clone()).or_default().counted = verdicts
                .iter()
                .map(|(id, report)| Counted {
                    node_id: id.clone(),
                    report: report.clone(),
                })
                .collect();
        }
        next.services.retain(|_, kept| !kept.is_empty());
        let commit = Commit {
            verdicts: Some((&held, &next)),
            ..Commit::default()
        };
        self.backend.commit(&commit).map_err(|why| match why {
            BackendError::Verdicts(why) => why,
            other => VerdictsError::Io(other.into_store()),
        })?;
        *held = next;
        Ok(())
    }
}

/// Whether [`VerdictFile::record`] of `heard` and `counted` from `host` changes `held`: looked
/// up entry by entry, so a heartbeat that changes nothing costs no copy of the store.
fn changes(
    held: &Held,
    host: &HostId,
    heard: &[Heard],
    counted: &[(String, Vec<(HostId, UpdateResult)>)],
) -> bool {
    let standing = |service: &str| {
        held.services
            .get(service)
            .and_then(|kept| kept.standing.get(host.as_str()))
    };
    let stands = heard.iter().any(|(service, last)| match last {
        Some(report) if ackable(report) => standing(service) != Some(report),
        Some(_) => standing(service).is_some(),
        None => false,
    });
    let counts = counted.iter().any(|(service, verdicts)| {
        let now = held
            .services
            .get(service)
            .map_or(&[][..], |kept| kept.counted.as_slice());
        now.len() != verdicts.len()
            || now
                .iter()
                .zip(verdicts)
                .any(|(one, (id, report))| one.node_id != *id || one.report != *report)
    });
    stands || counts
}

/// Whether `report` names its order and try, which an ack needs.
fn ackable(report: &UpdateResult) -> bool {
    report.order_id.is_some() && report.order_try.is_some()
}

/// The text of the file at `path` holding `held`.
pub(crate) fn text_of(path: &Path, held: &Held) -> Result<Vec<u8>, StoreError> {
    let mut text = serde_json::to_vec_pretty(held)
        .map_err(io::Error::from)
        .map_err(io_at(path))?;
    text.push(b'\n');
    Ok(text)
}

/// What the file at `path`, whose bytes are `bytes`, holds.
pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<Held, VerdictsError> {
    let invalid = |why: String| VerdictsError::Invalid {
        path: path.to_owned(),
        why,
    };
    let held: Held = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    for (service, kept) in &held.services {
        for host in kept.standing.keys() {
            host.parse::<HostId>()
                .map_err(|e| invalid(format!("{service}: host {host:?}: {e}")))?;
        }
    }
    Ok(held)
}

/// Whether the agent that sent `beat` announced [`FEATURE`].
pub(crate) fn asks(beat: &Heartbeat) -> bool {
    beat.agent_features.iter().any(|name| name == FEATURE)
}

/// `beat` of `host`, an agent that announced [`FEATURE`], with the standing verdict of each
/// service whose entry carries none, the legacy service's (`legacy`, the top-level fields)
/// among them; what the heartbeat itself said of each; and the services whose standing verdict
/// the heartbeat superseded, which the caller forgets ([`VerdictFile::supersede`]).
///
/// A standing verdict is superseded, and not read, when the host runs the service at the
/// version `wanted` gives for it (the desired version) and the verdict is about another
/// version: the host reached the version wanted since, by an order whose own verdict it never
/// sent (a self-update, whose binary exits before it reports), and the older verdict is not its
/// report any more. Read in its place, a failure of an older target would count against the
/// host (its update slot, the report) for as long as it ran the version wanted. A verdict about
/// the version the host runs, and every verdict of a host that does not run the version wanted
/// (a failure it fell back from, which the gate reads so that it does not order that version
/// again), stand.
pub(crate) fn fill(
    file: &VerdictFile,
    legacy: Option<&str>,
    host: &HostId,
    beat: &mut Heartbeat,
    wanted: &dyn Fn(&str) -> Option<Version>,
) -> (Vec<Heard>, Vec<String>) {
    let mut heard = Vec::new();
    let mut superseded = Vec::new();
    let mut standing = |name: &str, runs: &Version| {
        let kept = file.standing(name, host)?;
        let reached = wanted(name).is_some_and(|want| want == *runs) && kept.target != *runs;
        if reached {
            superseded.push(name.to_owned());
            return None;
        }
        Some(kept)
    };
    if let Some(name) = legacy {
        heard.push((name.to_owned(), beat.last_update.clone()));
        if beat.last_update.is_none() {
            beat.last_update = standing(name, &beat.version);
        }
    }
    for one in beat.services.iter_mut().flatten() {
        heard.push((one.name.clone(), one.last_update.clone()));
        if one.last_update.is_none() {
            one.last_update = standing(&one.name, &one.version);
        }
    }
    (heard, superseded)
}

/// Stores what the heartbeat of `host` said (`heard`, of the services it is counted or kept
/// in) and the verdicts the fleet counts of each service of `names`, and answers the acks:
/// each verdict heard that is the one standing for the host once the write returned. When the
/// write fails, it is logged and nothing is acknowledged.
pub(crate) fn acknowledge(
    file: &VerdictFile,
    fleet: &Fleet,
    host: &HostId,
    heard: &[Heard],
    names: &[&str],
) -> Vec<VerdictAck> {
    let counted: Vec<_> = names
        .iter()
        .map(|name| ((*name).to_owned(), fleet.counted_of(name)))
        .collect();
    if let Err(e) = file.record(host, heard, &counted) {
        tracing::error!(node_id = %host, error = %e, "verdicts not written: none acknowledged");
        return Vec::new();
    }
    let mut acks: Vec<VerdictAck> = Vec::new();
    for (service, last) in heard {
        let Some(report) = last else {
            continue;
        };
        let (Some(order_id), Some(order_try)) = (report.order_id.clone(), report.order_try) else {
            continue;
        };
        let ack = VerdictAck {
            service: service.clone(),
            order_id,
            order_try,
        };
        // A service the heartbeat names twice (the legacy one in `services` too) is
        // acknowledged once: an agent refuses an answer that lists an ack twice.
        if file.standing(service, host).as_ref() == Some(report) && !acks.contains(&ack) {
            acks.push(ack);
        }
    }
    acks
}

/// The acks of an answer as `hostkeeper.agent.v1` says them (`HeartbeatResponse.acks`).
#[must_use]
pub fn to_proto(acks: &[VerdictAck]) -> Vec<agent::VerdictAck> {
    acks.iter()
        .map(|ack| agent::VerdictAck {
            service: ack.service.clone(),
            order_id: ack.order_id.as_str().to_owned(),
            order_try: ack.order_try,
        })
        .collect()
}
