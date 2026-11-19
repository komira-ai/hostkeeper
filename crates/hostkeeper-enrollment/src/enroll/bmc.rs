//! The BMC a host's daemon reported in its heartbeats (`HeartbeatRequest.bmc`), kept with what
//! the host runs in its host record ([`EnrollmentStore::note_bmc`]), so that every coordinator
//! sharing the store finds a BMC's last known address, a host that stopped heartbeating
//! included.

use std::net::IpAddr;
use std::sync::PoisonError;

/// How long a BMC this store wrote or read is answered from memory, in seconds: past it, the
/// next report reads the host record again, so a record another writer changed (another
/// coordinator, or a release before this one, which drops the BMC when it rewrites what the host
/// runs) is written again within ten minutes.
pub const BMC_REMEMBERED_SECS: u64 = 600;

use super::StoreError;
use super::records::pb;
use super::store::EnrollmentStore;
use crate::names::{HostId, MacAddress};

/// A host's BMC, as its daemon reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostBmc {
    /// The BMC's IP address.
    pub ip: IpAddr,
    /// The BMC's MAC.
    pub mac: MacAddress,
}

/// A host's BMC as its host record keeps it: the report, and when it was first accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptBmc {
    /// The report.
    pub bmc: HostBmc,
    /// When the coordinator accepted the heartbeat that first reported it, unix seconds.
    pub at_unix: u64,
}

impl KeptBmc {
    /// What `record` holds; `None` when its address or MAC does not read as one (a record this
    /// release did not write), which the next report writes over.
    pub(super) fn of_pb(record: &pb::RunningBmc) -> Option<KeptBmc> {
        Some(KeptBmc {
            bmc: HostBmc {
                ip: record.ip.parse().ok()?,
                mac: MacAddress::parse(&record.mac).ok()?,
            },
            at_unix: record.at_unix,
        })
    }

    fn to_pb(self) -> pb::RunningBmc {
        pb::RunningBmc {
            ip: self.bmc.ip.to_string(),
            mac: self.bmc.mac.to_string(),
            at_unix: self.at_unix,
        }
    }
}

impl EnrollmentStore {
    /// The heartbeat path's report that a heartbeat of `host` it accepted at `now` carried the
    /// host's BMC `bmc`. It is written into the record of what the host runs when it differs from
    /// the BMC that record holds, with `now`; when only the time differs nothing is written, so
    /// the time kept is that of the heartbeat that first reported this address and MAC. A host
    /// with no record of what it runs yet (no heartbeat recorded since its approval) and a host id
    /// never approved record nothing: the next heartbeat's report tries again.
    ///
    /// Decided as [`Self::note_heartbeat`] is. A host whose record this store wrote or read with
    /// `bmc` less than [`BMC_REMEMBERED_SECS`] before `now` is answered from memory, without the
    /// lock or a read; an approval of the host by this store forgets it.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read or written.
    pub fn note_bmc(&self, host: &HostId, bmc: HostBmc, now: u64) -> Result<(), StoreError> {
        let known = || self.bmcs.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = |(held, at): &(HostBmc, u64)| {
            *held == bmc && now >= *at && now - at < BMC_REMEMBERED_SECS
        };
        if known().get(host).is_some_and(fresh) {
            return Ok(());
        }
        self.decided(|| {
            let Some(row) = self.load_host(host)? else {
                return Ok(());
            };
            let Some(running) = &row.pb.running else {
                return Ok(());
            };
            let kept = running.bmc.as_ref().and_then(KeptBmc::of_pb);
            if kept.map(|kept| kept.bmc) != Some(bmc) {
                let record = pb::Running {
                    bmc: Some(KeptBmc { bmc, at_unix: now }.to_pb()),
                    ..running.clone()
                };
                self.records().set_running(&row.pb, record)?;
            }
            known().insert(host.clone(), (bmc, now));
            Ok(())
        })
    }
}
