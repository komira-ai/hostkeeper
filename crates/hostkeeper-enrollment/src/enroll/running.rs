//! What a host's daemon said it runs, kept with the host's enrollment: the daemon's name and
//! version and each service with its version and installer, so the coordinator can answer for a
//! host it has not heard from since it started ([`EnrollmentStore::note_running`]).

use std::sync::PoisonError;

use super::StoreError;
use super::records::pb;
use super::store::EnrollmentStore;
use crate::names::HostId;

/// One service a host runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningService {
    pub service: String,
    pub version: String,
    /// The installer it runs with; empty when not known.
    pub installer: String,
}

/// What a host's daemon said it runs: the daemon, and each service that is not absent, by
/// service name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostRunning {
    pub agent_name: String,
    pub agent_version: String,
    pub services: Vec<RunningService>,
}

impl HostRunning {
    /// The record of this, taken from a heartbeat accepted at `at_unix`.
    fn to_pb(&self, at_unix: u64) -> pb::Running {
        pb::Running {
            agent_name: self.agent_name.clone(),
            agent_version: self.agent_version.clone(),
            services: self
                .services
                .iter()
                .map(|s| pb::RunningService {
                    service: s.service.clone(),
                    version: s.version.clone(),
                    installer: s.installer.clone(),
                })
                .collect(),
            at_unix,
        }
    }

    /// This, with the installer of each service whose installer is empty (not known) taken from
    /// the same service in `kept`, when that has one.
    fn keeping_installers_of(&self, kept: Option<&HostRunning>) -> HostRunning {
        let mut said = self.clone();
        for one in said.services.iter_mut().filter(|s| s.installer.is_empty()) {
            let before = kept
                .into_iter()
                .flat_map(|kept| &kept.services)
                .find(|k| k.service == one.service);
            if let Some(before) = before {
                one.installer.clone_from(&before.installer);
            }
        }
        said
    }

    /// What `record` holds.
    pub(super) fn of_pb(record: &pb::Running) -> HostRunning {
        HostRunning {
            agent_name: record.agent_name.clone(),
            agent_version: record.agent_version.clone(),
            services: record
                .services
                .iter()
                .map(|s| RunningService {
                    service: s.service.clone(),
                    version: s.version.clone(),
                    installer: s.installer.clone(),
                })
                .collect(),
        }
    }
}

impl EnrollmentStore {
    /// The heartbeat path's report that a heartbeat of `host` it accepted at `now` said the host
    /// runs `running`. The host's record is written when `running` differs from what it holds
    /// (the daemon or any service, its version or installer), with `now`; when only the time
    /// differs nothing is written, so the time the record keeps is that of the heartbeat that
    /// last changed it. An approval writes the host record without it, so a host approved again
    /// records its next heartbeat. A host id never approved has no record and nothing is
    /// recorded.
    ///
    /// A service whose installer in `running` is empty (not known) keeps the installer the record
    /// holds for the same service, else it stays empty.
    ///
    /// Decided under the store's lock, as [`Self::note_heartbeat`] is. A host whose record is
    /// known to hold `running` already (this store wrote or read it) is answered from memory,
    /// without the lock or a read; an approval of the host forgets it.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read or written.
    pub fn note_running(
        &self,
        host: &HostId,
        running: &HostRunning,
        now: u64,
    ) -> Result<(), StoreError> {
        let known = || self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if known().get(host) == Some(running) {
            return Ok(());
        }
        let _held = self.lock()?;
        let Some(row) = self.load_host(host)? else {
            return Ok(());
        };
        let kept = row.pb.running.as_ref().map(HostRunning::of_pb);
        let said = running.keeping_installers_of(kept.as_ref());
        if kept.as_ref() != Some(&said) {
            self.records().set_running(&row.pb, said.to_pb(now))?;
        }
        known().insert(host.clone(), running.clone());
        Ok(())
    }
}
