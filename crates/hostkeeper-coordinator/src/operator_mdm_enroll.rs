//! `Mdm.ListEnrollmentRequests` and `Mdm.ConfirmEnrollment` of `hostkeeper.operator.v1`
//! (`docs/design/coordinator-mdm.md`, 4.2): the operator's side of the fail-closed rule of
//! `/enroll` ([`crate::mdm_enroll`]).
//!
//! A Mac whose device blob could not be verified waits in the list. The operator confirms the one
//! they see enrolling, and the ABM table is read again at that moment: a serial it does not list is
//! refused, so a confirmation never admits a Mac that is not ours. Confirming is a session-only
//! write, audited under the user's name before it is made; listing changes nothing and takes the
//! operator's token too.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::audit_actions;
use crate::mdm_enroll::Tunables;
use crate::operator::Caller;
use crate::service::Coordinator;

impl Coordinator {
    /// `Mdm.ListEnrollmentRequests`: the requests that wait for an operator, by serial, each with
    /// whether the ABM table lists the serial now.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed` (the ABM table could not be read).
    pub fn mdm_list_enrollment_requests(
        &self,
        caller: &Caller,
    ) -> Result<proto::ListEnrollmentRequestsResponse, MdmRefusal> {
        caller.operator()?;
        let Some(server) = self.mdm_server() else {
            return Ok(proto::ListEnrollmentRequestsResponse::default());
        };
        let mut requests = Vec::new();
        for (serial, held) in server.enroll.pending.list() {
            requests.push(proto::EnrollmentRequest {
                in_abm: server.serial_in_abm(&serial).map_err(|why| {
                    tracing::error!(%why, "the ABM table could not be read");
                    MdmRefusal::StoreFailed
                })?,
                serial,
                udid: held.udid,
                product: held.product,
                reason: held.reason,
                first_unix: held.first_unix,
                last_unix: held.last_unix,
                count: held.count,
                confirmed_until_unix: held.confirmed_until,
            });
        }
        Ok(proto::ListEnrollmentRequestsResponse { requests })
    }

    /// `Mdm.ConfirmEnrollment`: confirms the waiting request of a serial, once the ABM table lists
    /// it.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (not a serial),
    /// `enrollment_unavailable` (no MDM listener), `no_pending_enrollment`, `serial_not_in_abm`,
    /// `audit_unavailable`, `store_failed`.
    pub fn mdm_confirm_enrollment(
        &self,
        caller: &Caller,
        request: &proto::ConfirmEnrollmentRequest,
    ) -> Result<proto::ConfirmEnrollmentResponse, MdmRefusal> {
        let by = caller.operator()?;
        let serial = request.serial.as_str();
        if !crate::mdm_enroll::enroll::is_serial(serial) {
            return Err(MdmRefusal::InvalidRequest);
        }
        let server = self.mdm_server().ok_or(MdmRefusal::EnrollmentUnavailable)?;
        if !server
            .enroll
            .pending
            .list()
            .iter()
            .any(|(waiting, _)| waiting == serial)
        {
            return Err(MdmRefusal::NoPendingEnrollment);
        }
        // The ABM table is read at the confirmation, not only when the request came.
        let listed = server.serial_in_abm(serial).map_err(|why| {
            tracing::error!(%why, "the ABM table could not be read");
            MdmRefusal::StoreFailed
        })?;
        if !listed {
            self.audit_setup(
                by,
                audit_actions::MDM_ENROLL_CONFIRMED,
                Some(serial),
                format!("refused: {}", MdmRefusal::SerialNotInAbm.as_str_name()),
            )?;
            return Err(MdmRefusal::SerialNotInAbm);
        }
        let ttl = Tunables::read(&self.mdm_setup).ok_or(MdmRefusal::StoreFailed)?;
        let until = server.now().saturating_add(i64::from(ttl.challenge_ttl_s));
        self.audit_setup(
            by,
            audit_actions::MDM_ENROLL_CONFIRMED,
            Some(serial),
            format!(
                "the next post of this serial and UDID is given a challenge until unix {until}"
            ),
        )?;
        server
            .enroll
            .pending
            .confirm(serial, until)
            .ok_or(MdmRefusal::NoPendingEnrollment)?;
        Ok(proto::ConfirmEnrollmentResponse {
            serial: serial.to_owned(),
            expires_unix: until,
        })
    }
}
