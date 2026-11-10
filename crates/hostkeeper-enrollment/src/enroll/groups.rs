//! The host group of an enrolled host, as its host record keeps it (`Host.host_group` and
//! `Host.moving_from` of `store.proto`): who is in which group, and the write by which a host
//! changes group.
//!
//! A host is in the group its record names, or in none when the name is empty. While a move
//! runs the record also names the group the host moves from, even when that was no group (an
//! empty name); a host with no `moving_from` is not moving. A host written before host groups
//! reads as in no group.
//!
//! A write replaces the two fields of the host's record and nothing else, in one
//! [`super::records::Records::commit`]: the key, the role, the times and what the host runs stay
//! as they are, and so does the generation of the host's facts.

use super::records::Record;
use super::store::EnrollmentStore;
use super::{Membership, StoreError};
use crate::names::HostId;

impl EnrollmentStore {
    /// The group of `host`, and the group it moves from if it is moving; `None` for a host id
    /// never approved, or denied (retiring or retired).
    ///
    /// # Errors
    /// [`StoreError`] when the host record or the deny row cannot be read.
    pub fn membership(&self, host: &HostId) -> Result<Option<Membership>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        if self.denied(host)? {
            return Ok(None);
        }
        Ok(Some(Membership::of(&row.pb)))
    }

    /// The group of every enrolled host ([`Self::membership`]), by host id. Reads no request,
    /// so a count or a page of hosts is chosen without reading them whole.
    ///
    /// # Errors
    /// [`StoreError`] when a host record or deny row cannot be read.
    pub fn memberships(&self) -> Result<Vec<(HostId, Membership)>, StoreError> {
        let mut all = Vec::new();
        for host in self.host_ids()? {
            if let Some(membership) = self.membership(&host)? {
                all.push((host, membership));
            }
        }
        Ok(all)
    }

    /// Puts `host` in the group `to` records, moving from the group it names or not moving,
    /// and answers what the host's record held before, so that a caller whose other write fails
    /// can write it back. One commit of the host's record; decided under the store's lock.
    ///
    /// # Errors
    /// [`StoreError::UnknownHost`] for a host id never approved, or denied; [`StoreError`] when
    /// a record cannot be read or written.
    pub fn set_membership(&self, host: &HostId, to: Membership) -> Result<Membership, StoreError> {
        let _held = self.lock()?;
        let unknown = || StoreError::UnknownHost(host.clone());
        let row = self.load_host(host)?.ok_or_else(unknown)?;
        if self.denied(host)? {
            return Err(unknown());
        }
        let before = Membership::of(&row.pb);
        let mut pb = row.pb;
        pb.host_group = to.host_group;
        pb.moving_from = to.moving_from;
        self.commit(&[Record::Host(pb)])?;
        Ok(before)
    }
}
