//! Issuance from the store: one certificate per request id and round.
//!
//! The order of the writes is what makes it so. Under the store lock:
//!
//! 1. a serial is taken and reserved in the request ([`Records::take_serial`](super::records::Records::take_serial)), unless the
//!    request already holds one for this round;
//! 2. the certificate is signed with that serial;
//! 3. the certificate's record is written and the request marked issued, in one commit (the
//!    files write the certificate first).
//!
//! A crash before 3 leaves the reserved serial and no certificate: the retry signs again with
//! the same serial, and the certificate signed before the crash was never written or returned.
//! A crash in 3 after the certificate's file leaves that file: the retry returns its bytes
//! unchanged and marks the request. So however often a daemon polls and whatever fails between
//! approval and issuance, one certificate, with one serial, is ever returned for a request id
//! and round.

use super::records::{Record, pb};
use super::rows::corrupt;
use super::store::EnrollmentStore;
use super::{IssueError, RequestState};
use crate::ca::{SignRequest, Signer, Subject};
use crate::names::{HostId, KeyHash};

/// A host certificate the store issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedCertificate {
    pub request_id: KeyHash,
    pub round: u32,
    pub host_id: HostId,
    pub serial: u64,
    /// Unix seconds.
    pub not_after: u64,
    /// The certificate, DER: the same bytes on every call for one request id and round.
    pub certificate_der: Vec<u8>,
    /// The request became issued at this call: its certificate was signed now, or was signed
    /// before a crash and is recorded issued now. Decided under the store's lock, so one call
    /// per request and round says so.
    pub first: bool,
}

impl EnrollmentStore {
    /// The certificate of the approved request `request_id`, made by `signer` at the first
    /// call of its round, valid [`EnrollmentStore::host_validity_secs`] from then, and read back
    /// at every later one (a poll issues lazily).
    ///
    /// # Errors
    /// [`IssueError::NotApproved`] while the request waits for an operator, or after it expired
    /// (the poll answers "pending"); [`IssueError::Denied`] when the request or its host is denied;
    /// [`IssueError::UnknownRequest`]; [`IssueError::Sign`]; [`IssueError::Store`], also for an
    /// issued request whose certificate record is gone (nothing is signed again).
    pub fn issue(
        &self,
        signer: &dyn Signer,
        request_id: &KeyHash,
    ) -> Result<IssuedCertificate, IssueError> {
        let _held = self.lock()?;
        let mut row = self
            .load_request(request_id)?
            .ok_or(IssueError::UnknownRequest)?;
        if row.state == RequestState::Denied {
            return Err(IssueError::Denied);
        }
        // An expired request is not approved either, and never will be: its host polls "pending"
        // until it posts its key again, which starts the request over.
        if row.state.is_pending() || row.state == RequestState::Expired {
            return Err(IssueError::NotApproved(row.state));
        }
        let host_id = row
            .host_id()?
            .ok_or(corrupt(&row.path, "an approved request names no host id"))?;
        if self.denied(&host_id)? {
            return Err(IssueError::Denied);
        }
        let round = row.pb.round;
        if let Some(issued) = self
            .records()
            .issued(request_id)?
            .filter(|issued| issued.round == round)
        {
            let first = row.state != RequestState::Issued;
            if first {
                row.set_state(RequestState::Issued);
                self.commit(&[Record::Request(row.pb)])?;
            }
            return Ok(view(request_id, &host_id, issued, first));
        }
        if row.state == RequestState::Issued {
            let reason = "the request is issued but its certificate file is gone";
            return Err(corrupt(&row.path, reason).into());
        }
        if row.pb.serial == 0 {
            row.pb.serial = self.records().take_serial(Some(&row.pb))?;
        }
        let certificate = signer.sign(&SignRequest {
            subject: Subject::Host(&host_id),
            key: &row.key,
            validity_secs: self.host_validity_secs(),
            serial: row.pb.serial,
        })?;
        let issued = pb::Issued {
            schema: super::rows::SCHEMA,
            request_id: request_id.as_bytes().to_vec(),
            round,
            serial: certificate.serial,
            certificate_der: certificate.der,
            host_id: host_id.to_string(),
            not_after: certificate.not_after,
        };
        row.set_state(RequestState::Issued);
        self.commit(&[Record::Issued(issued.clone()), Record::Request(row.pb)])?;
        Ok(view(request_id, &host_id, issued, true))
    }
}

fn view(
    request_id: &KeyHash,
    host_id: &HostId,
    issued: pb::Issued,
    first: bool,
) -> IssuedCertificate {
    IssuedCertificate {
        request_id: *request_id,
        round: issued.round,
        host_id: host_id.clone(),
        serial: issued.serial,
        not_after: issued.not_after,
        certificate_der: issued.certificate_der,
        first,
    }
}
