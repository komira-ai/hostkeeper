//! Issuance from the store: one certificate per request id and round.
//!
//! The order of the writes is what makes it so. Under the store lock:
//!
//! 1. a serial is reserved in the request row (the counter first, then the row), unless the
//!    row already holds one for this round;
//! 2. the certificate is signed with that serial and written to `issued/<request id>.pb`;
//! 3. the request is marked issued.
//!
//! A crash before 2 leaves the reserved serial and no file: the retry signs again with the
//! same serial, and the certificate signed before the crash was never written or returned. A
//! crash after 2 leaves the file: the retry returns its bytes unchanged and finishes 3. So
//! however often a daemon polls and whatever fails between approval and issuance, one
//! certificate, with one serial, is ever returned for a request id and round.

use super::files::{corrupt, pb};
use super::store::EnrollmentStore;
use super::{IssueError, RequestState};
use crate::ca::{HOST_VALIDITY_SECS, SignRequest, Signer, Subject};
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
    /// call of its round and read back at every later one (a poll issues lazily).
    ///
    /// # Errors
    /// [`IssueError::NotApproved`] while the request waits for an operator (the poll answers
    /// "pending"); [`IssueError::Denied`] when the request or its host is denied;
    /// [`IssueError::UnknownRequest`]; [`IssueError::Sign`]; [`IssueError::Store`], also for an
    /// issued request whose certificate file is gone (nothing is signed again).
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
        if row.state.is_pending() {
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
            .load_issued(request_id)?
            .filter(|issued| issued.round == round)
        {
            let first = row.state != RequestState::Issued;
            if first {
                row.set_state(RequestState::Issued);
                self.write_request(&row)?;
            }
            return Ok(view(request_id, &host_id, issued, first));
        }
        if row.state == RequestState::Issued {
            let reason = "the request is issued but its certificate file is gone";
            return Err(corrupt(&row.path, reason).into());
        }
        if row.pb.serial == 0 {
            row.pb.serial = self.next_serial()?;
            self.write_request(&row)?;
        }
        let certificate = signer.sign(&SignRequest {
            subject: Subject::Host(&host_id),
            key: &row.key,
            validity_secs: HOST_VALIDITY_SECS,
            serial: row.pb.serial,
        })?;
        let issued = pb::Issued {
            schema: super::files::SCHEMA,
            request_id: request_id.as_bytes().to_vec(),
            round,
            serial: certificate.serial,
            certificate_der: certificate.der,
            host_id: host_id.to_string(),
            not_after: certificate.not_after,
        };
        self.write_issued(&issued, request_id)?;
        row.set_state(RequestState::Issued);
        self.write_request(&row)?;
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
