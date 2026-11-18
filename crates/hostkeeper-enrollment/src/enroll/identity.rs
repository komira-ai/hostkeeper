//! A host's approval for service identity (docs/design/service-identity.md, section 3).
//!
//! The coordinator signs a certificate for a service on a host only when the host's *assurance*
//! allows it ([`Assurance`]): `operator` when an operator approved the host's key by hand,
//! `auto` when the coordinator did, by an inventory record ([`INVENTORY_APPROVER`]). An auto host
//! gets certificates only after an operator approved its current key for identity
//! ([`EnrollmentStore::approve_identity`]), which is kept in the host's record
//! (`Host.identity_approval`) and counts only while it names the key the host holds: an approval
//! of a host writes that record anew without it, so a new key needs a new approval.
//!
//! The assurance is not stored. It is read from the approval of the request whose id is the
//! host's key hash: [`INVENTORY_APPROVER`] is `auto`, anything else `operator`; a host whose
//! request or approval is not found is `auto`, so a missing record never allows more.

use super::StoreError;
use super::records::pb;
use super::rows::corrupt;
use super::store::EnrollmentStore;
use crate::names::{HostId, KeyHash};

/// The approver an approval made by an inventory record names: the coordinator itself.
pub const INVENTORY_APPROVER: &str = "inventory";

/// How a host's current key was approved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assurance {
    /// An operator approved the key by hand.
    Operator,
    /// The coordinator approved the key by an inventory record, or the approval is not found.
    Auto,
}

/// An operator's approval of a host's key for service identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityApproval {
    /// The key approved.
    pub key_hash: KeyHash,
    /// Who approved it.
    pub approver: String,
    /// When, in unix seconds.
    pub approved_unix: u64,
}

/// What the store holds of a host's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostIdentityState {
    /// How the host's current key was approved.
    pub assurance: Assurance,
    /// The host's current key.
    pub key_hash: KeyHash,
    /// The approval of the host's current key for identity; `None` when there is none, or when
    /// the approval held is of another key.
    pub approval: Option<IdentityApproval>,
    /// Whether the host is denied (retiring or retired).
    pub denied: bool,
}

/// What [`EnrollmentStore::approve_identity`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityApproved {
    /// The approval was written.
    Approved(IdentityApproval),
    /// The key was approved already; nothing was written. The approval in force.
    AlreadyApproved(IdentityApproval),
    /// The host's assurance is `operator`, which needs no approval; nothing was written.
    OperatorAssured,
}

/// Why an approval for identity was refused.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// No host of this id was ever approved.
    #[error("no host {0} was approved")]
    UnknownHost(HostId),
    /// The key named is not the host's current key.
    #[error("the key named is not the host's current key")]
    KeyMismatch,
    /// The host is denied.
    #[error("the host is denied")]
    Denied,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl EnrollmentStore {
    /// Approves `key_hash`, which must be the key `host` holds now, for service identity, by
    /// `approver` at `now`. Refused for a host never approved ([`IdentityError::UnknownHost`]),
    /// for another key ([`IdentityError::KeyMismatch`]) and for a denied host
    /// ([`IdentityError::Denied`]), in that order. A host whose assurance is `operator`, and a
    /// key approved already, write nothing. Decided under the store's lock.
    ///
    /// # Errors
    /// [`IdentityError`], as above; [`IdentityError::Store`] when a record cannot be read or
    /// written.
    pub fn approve_identity(
        &self,
        host: &HostId,
        key_hash: &KeyHash,
        approver: &str,
        now: u64,
    ) -> Result<IdentityApproved, IdentityError> {
        let _held = self.lock()?;
        let row = self
            .load_host(host)?
            .ok_or_else(|| IdentityError::UnknownHost(host.clone()))?;
        if !row.key.equals_in_constant_time(key_hash) {
            return Err(IdentityError::KeyMismatch);
        }
        if self.denied(host)? {
            return Err(IdentityError::Denied);
        }
        if self.assurance_of(&row.key)? == Assurance::Operator {
            return Ok(IdentityApproved::OperatorAssured);
        }
        if let Some(held) = self.approval_of(host, &row)? {
            return Ok(IdentityApproved::AlreadyApproved(held));
        }
        let approval = IdentityApproval {
            key_hash: row.key,
            approver: approver.to_owned(),
            approved_unix: now,
        };
        self.records().set_identity_approval(
            &row.pb,
            pb::IdentityApproval {
                key_hash: approval.key_hash.as_bytes().to_vec(),
                approver: approval.approver.clone(),
                approved_unix: now,
            },
        )?;
        Ok(IdentityApproved::Approved(approval))
    }

    /// The identity state of `host`; `None` for a host never approved.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read.
    pub fn identity_state(&self, host: &HostId) -> Result<Option<HostIdentityState>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        Ok(Some(HostIdentityState {
            assurance: self.assurance_of(&row.key)?,
            key_hash: row.key,
            approval: self.approval_of(host, &row)?,
            denied: self.denied(host)?,
        }))
    }

    /// Whether the coordinator may sign certificates for services on `host`: it was approved, is
    /// not denied, and its assurance is `operator` or its current key is approved for identity.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read.
    pub fn identity_allowed(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.identity_state(host)?.is_some_and(|state| {
            !state.denied && (state.assurance == Assurance::Operator || state.approval.is_some())
        }))
    }

    /// The hosts with a deny row that is retiring or retired, sorted: not those whose deny row
    /// an approval with `replace` cleared.
    ///
    /// # Errors
    /// [`StoreError`] when a deny row cannot be listed or read.
    pub fn denied_hosts(&self) -> Result<Vec<HostId>, StoreError> {
        let mut denied = Vec::new();
        for host in self.records().deny_ids()? {
            if self.denied(&host)? {
                denied.push(host);
            }
        }
        denied.sort();
        Ok(denied)
    }

    /// How `host`'s key `key` was approved: [`Assurance::Auto`] when the approval of the request
    /// of that key names [`INVENTORY_APPROVER`], and when there is no such request or approval;
    /// [`Assurance::Operator`] otherwise.
    fn assurance_of(&self, key: &KeyHash) -> Result<Assurance, StoreError> {
        let approver = self
            .load_request(key)?
            .and_then(|row| row.pb.approved)
            .map(|approval| approval.approver);
        Ok(match approver {
            Some(approver) if approver != INVENTORY_APPROVER => Assurance::Operator,
            _ => Assurance::Auto,
        })
    }

    /// The approval for identity `row` holds, when it names the host's key.
    fn approval_of(
        &self,
        host: &HostId,
        row: &super::rows::HostRow,
    ) -> Result<Option<IdentityApproval>, StoreError> {
        let Some(held) = &row.pb.identity_approval else {
            return Ok(None);
        };
        let key_hash = KeyHash::from_bytes(&held.key_hash).ok_or_else(|| {
            corrupt(
                &self.host_path(host),
                "the identity approval's key is not 32 bytes",
            )
        })?;
        Ok(key_hash
            .equals_in_constant_time(&row.key)
            .then(|| IdentityApproval {
                key_hash,
                approver: held.approver.clone(),
                approved_unix: held.approved_unix,
            }))
    }
}
