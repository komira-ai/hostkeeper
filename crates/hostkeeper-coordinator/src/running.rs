//! What each host runs, as its daemon said in its last heartbeat (packages-native.md, section
//! 10.6): the daemon's name and version, and each service that is not absent with its version
//! and the installer it runs from. `Hosts.List` answers it as `Host.running`.
//!
//! The coordinator keeps the last record of each host it heard ([`Kept`]), replaced by every
//! heartbeat, and with enrollment keeps it with the host's enrollment too
//! ([`hostkeeper_enrollment::enroll::EnrollmentStore::note_running`], written when what the host
//! runs changes), so a host not heard since the coordinator started is listed with what it ran
//! when last heard.
//!
//! A service's installer is:
//! - `hostkeeper-archive` for a daemon that did not announce [`TARGET_FEATURE`]: it runs every
//!   service from archives;
//! - for one that did, and whose installers the coordinator has read over gRPC since it started
//!   ([`crate::picks::Picks::listed`]), the one it named for the service, else
//!   `hostkeeper-archive`;
//! - for one that did and whose installers were not read (it beats over the JSON route), the one
//!   the host's last record held for the service, else empty: not known. The enrollment keeps an
//!   installer it holds for the same service when a record says it is not known.
//!
//! The records are in memory under their own lock, taken after the fleet's and holding nothing
//! else.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_enrollment::enroll::{EnrolledHost, HostRunning, RunningService};
use hostkeeper_enrollment::names::HostId as StoreHostId;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_release::ARCHIVE_INSTALLER;
use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat};

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::picks::TARGET_FEATURE;
use crate::service::Coordinator;

/// The last record of what each host heard runs, and when it was taken (unix seconds).
#[derive(Debug, Default)]
pub(crate) struct Kept {
    seen: Mutex<BTreeMap<HostId, (HostRunning, u64)>>,
}

impl Kept {
    /// Replaces the record of `id`.
    fn note(&self, id: &HostId, running: HostRunning, unix: u64) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), (running, unix));
    }

    /// The record of `id` and when it was taken, if the host was heard.
    pub(crate) fn of(&self, id: &HostId) -> Option<(HostRunning, u64)> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Forgets `id`: the host was released ([`Fleet::release_if_final`]).
    pub(crate) fn forget(&self, id: &HostId) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

/// What `beat` of `id` says the host runs: its daemon, and the legacy service (when `legacy`
/// names one) and each of `announced` that is not absent, by name. `before` is the record the
/// host had.
fn said(
    fleet: &Fleet,
    id: &HostId,
    beat: &Heartbeat,
    announced: &[ServiceBeat],
    legacy: Option<&str>,
    before: Option<&HostRunning>,
) -> HostRunning {
    let target = beat.agent_features.iter().any(|f| f == TARGET_FEATURE);
    let read = fleet.picks.listed(id).is_some();
    // A daemon that names no installer for a service runs it from archives.
    let installer = |name: &str| -> String {
        if !target {
            ARCHIVE_INSTALLER.to_owned()
        } else if read {
            fleet
                .picks
                .running(id, name)
                .unwrap_or(ARCHIVE_INSTALLER)
                .to_owned()
        } else {
            before
                .into_iter()
                .flat_map(|before| &before.services)
                .find(|one| one.service == name)
                .map(|one| one.installer.clone())
                .unwrap_or_default()
        }
    };
    let top = legacy.map(|name| (name, beat.version.as_str(), beat.health));
    let listed = announced
        .iter()
        .map(|one| (one.name.as_str(), one.version.as_str(), one.health));
    let mut services: Vec<RunningService> = top
        .into_iter()
        .chain(listed)
        .filter(|(_, _, health)| *health != Health::Absent)
        .map(|(name, version, _)| RunningService {
            service: name.to_owned(),
            version: version.to_owned(),
            installer: installer(name),
        })
        .collect();
    services.sort_by(|a, b| a.service.cmp(&b.service));
    HostRunning {
        agent_name: beat
            .agent
            .as_ref()
            .map(|a| a.name.as_str().to_owned())
            .unwrap_or_default(),
        agent_version: beat
            .agent
            .as_ref()
            .map(|a| a.version.as_str().to_owned())
            .unwrap_or_default(),
        services,
    }
}

/// `running` as the message, a service whose installer is empty taking the one `kept` holds for
/// it, when it holds one.
fn services_of(running: &HostRunning, kept: Option<&HostRunning>) -> Vec<proto::RunningService> {
    let kept = kept
        .map(|kept| kept.services.as_slice())
        .unwrap_or_default();
    running
        .services
        .iter()
        .map(|one| {
            let known = kept.iter().find(|k| k.service == one.service);
            let installer = match known {
                Some(known) if one.installer.is_empty() => known.installer.clone(),
                _ => one.installer.clone(),
            };
            proto::RunningService {
                service: one.service.clone(),
                version: one.version.clone(),
                installer,
            }
        })
        .collect()
}

/// The host `enrolled` is, as `Hosts.List` shows a host the coordinator has not heard from since
/// it started: not live, with its daemon and what it ran as its enrollment kept them.
pub(crate) fn unheard(enrolled: &EnrolledHost) -> proto::Host {
    let mut host = proto::Host {
        node_id: enrolled.host_id.to_string(),
        role: enrolled.role.as_str().to_owned(),
        ..proto::Host::default()
    };
    if let Some(running) = &enrolled.running {
        host.heartbeat = Some(agent::HeartbeatRequest {
            agent: agent_of(running),
            ..agent::HeartbeatRequest::default()
        });
        host.running = services_of(running, None);
        host.running_unix = enrolled.running_unix;
    }
    host
}

/// The daemon `running` names; `None` when its heartbeat named none.
fn agent_of(running: &HostRunning) -> Option<agent::AgentInfo> {
    Some(running.agent_name.as_str())
        .filter(|name| !name.is_empty())
        .map(|name| agent::AgentInfo {
            name: name.to_owned(),
            version: running.agent_version.clone(),
        })
}

impl Coordinator {
    /// Notes what the heartbeat `beat` of `who` (`announced` its services, `legacy` the legacy
    /// service its top level is) says the host runs: it replaces the record kept for the host,
    /// and with enrollment, when `keep` is set (the host is ordered), it is kept with the host's
    /// enrollment: a duplicate host's heartbeats come from two daemons, and a retiring host's
    /// enrollment is not written for. `fleet` is the lock the caller holds, for the installers
    /// the host's daemon said over gRPC.
    pub(crate) fn note_running(
        &self,
        fleet: &Fleet,
        who: &Identity,
        beat: &Heartbeat,
        announced: &[ServiceBeat],
        legacy: Option<&str>,
        keep: bool,
    ) {
        let before = self.running.of(&who.host_id).map(|(before, _)| before);
        let running = said(
            fleet,
            &who.host_id,
            beat,
            announced,
            legacy,
            before.as_ref(),
        );
        if let Some(enrollment) = self.enrollment().filter(|_| keep) {
            enrollment.note_running(&who.host_id, &running);
        }
        self.running
            .note(&who.host_id, running, self.ops.unix_now());
    }

    /// Fills in `host`, a host heard since the coordinator started, with what it runs: the record
    /// kept from its heartbeats, else what `enrolled` kept; a service whose installer is not
    /// known takes the one `enrolled` kept for it.
    pub(crate) fn show_running(
        &self,
        host: &mut proto::Host,
        id: &HostId,
        enrolled: Option<&EnrolledHost>,
    ) {
        let kept = enrolled.and_then(|e| e.running.as_ref());
        match self.running.of(id) {
            Some((running, unix)) => {
                host.running = services_of(&running, kept);
                host.running_unix = Some(unix);
            }
            None => {
                if let Some(kept) = kept {
                    host.running = services_of(kept, None);
                    host.running_unix = enrolled.and_then(|e| e.running_unix);
                }
            }
        }
    }

    /// The hosts of a page of `Hosts.List`: `page` holds each id with the host heard since the
    /// start (if it was), `enrolled` what the enrollment holds of each. A heard host takes what
    /// it runs ([`Self::show_running`]); a host not heard is the one its enrollment kept
    /// ([`unheard`]), and is left out when the enrollment holds none (it was denied since the ids
    /// were read). Answers the hosts with their enrollment.
    pub(crate) fn page_hosts(
        &self,
        page: Vec<(HostId, Option<proto::Host>)>,
        enrolled: Vec<Option<EnrolledHost>>,
    ) -> (Vec<proto::Host>, Vec<Option<EnrolledHost>>) {
        let mut hosts = Vec::with_capacity(page.len());
        let mut kept = Vec::with_capacity(page.len());
        for ((id, heard), enrolled) in page.into_iter().zip(enrolled) {
            let host = match (heard, &enrolled) {
                (Some(mut host), enrolled) => {
                    self.show_running(&mut host, &id, enrolled.as_ref());
                    host
                }
                (None, Some(enrolled)) => unheard(enrolled),
                (None, None) => continue,
            };
            hosts.push(host);
            kept.push(enrolled);
        }
        (hosts, kept)
    }

    /// The host ids of the hosts enrolled and not denied, by id; none without enrollment.
    ///
    /// # Errors
    /// The enrollment store's error.
    pub(crate) fn enrolled_ids(
        &self,
    ) -> Result<Vec<HostId>, hostkeeper_enrollment::enroll::StoreError> {
        let Some(enrollment) = self.enrollment() else {
            return Ok(Vec::new());
        };
        Ok(enrollment
            .store()
            .enrolled_host_ids()?
            .iter()
            .filter_map(|id: &StoreHostId| id.as_str().parse::<HostId>().ok())
            .collect())
    }
}
