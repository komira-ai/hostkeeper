//! `Mdm.ListDevices` of `hostkeeper.operator.v1`, as a plain function over its messages, as
//! the other operator calls are ([`crate::operator`]), and the coordinator's hold of the MDM
//! policy gate ([`crate::mdm`]).
//!
//! It takes the operator token only: a valid CI token is `forbidden`, and no token or any other
//! `unauthorized`, before the request is looked at. It reads the gate's records and asks no
//! device and not NanoHUB; nothing is audited, since nothing changes. A coordinator without
//! the gate answers no device, with `gate` false. Each row says where the device is in applying
//! its host group's enrollment baseline ([`hostkeeper_mdm_gate::baseline`]), from the records
//! and the baseline file alone.

use std::sync::Arc;

use hostkeeper_mdm_gate::{BaselineState, BaselineStatus, DeviceRecord, EnrollmentId, Phase};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::mdm::Mdm;
use crate::operator::Caller;
use crate::service::Coordinator;

/// The most devices one page holds, and the page of a `limit` of 0.
pub const MAX_DEVICE_PAGE: usize = 256;

impl Coordinator {
    /// The same coordinator, running the MDM policy gate `mdm`.
    #[must_use]
    pub fn with_mdm(mut self, mdm: Option<Mdm>) -> Self {
        self.mdm = mdm.map(Arc::new);
        self
    }

    /// The MDM policy gate, if the coordinator runs it.
    #[must_use]
    pub fn mdm(&self) -> Option<&Arc<Mdm>> {
        self.mdm.as_ref()
    }

    /// `Mdm.ListDevices`: the gate's device records, by enrollment id, a page at a time.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no enrollment id).
    pub fn mdm_list_devices(
        &self,
        caller: &Caller,
        request: &proto::ListDevicesRequest,
    ) -> Result<proto::ListDevicesResponse, MdmRefusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(EnrollmentId::parse(token).ok_or(MdmRefusal::InvalidRequest)?),
        };
        let Some(mdm) = self.mdm() else {
            return Ok(proto::ListDevicesResponse::default());
        };
        let limit = match request.limit as usize {
            0 => MAX_DEVICE_PAGE,
            limit => limit.min(MAX_DEVICE_PAGE),
        };
        // The gate lists its records by enrollment id.
        let mut records = mdm.gate.records();
        records.retain(|record| {
            after
                .as_ref()
                .is_none_or(|after| record.enrollment_id > *after)
        });
        let more = records.len() > limit;
        records.truncate(limit);
        let next_page_token = match records.last() {
            Some(last) if more => last.enrollment_id.as_str().to_owned(),
            _ => String::new(),
        };
        Ok(proto::ListDevicesResponse {
            devices: records
                .iter()
                .map(|record| device_proto(record, mdm.gate.baseline_status(record)))
                .collect(),
            next_page_token,
            gate: true,
        })
    }
}

fn phase_proto(phase: Phase) -> proto::MdmPhase {
    match phase {
        Phase::Authenticated => proto::MdmPhase::Authenticated,
        Phase::Enrolled => proto::MdmPhase::Enrolled,
        Phase::CheckedOut => proto::MdmPhase::CheckedOut,
    }
}

fn baseline_proto(state: BaselineState) -> proto::MdmBaselineState {
    match state {
        BaselineState::Pending => proto::MdmBaselineState::Pending,
        BaselineState::Applied => proto::MdmBaselineState::Applied,
        BaselineState::Failed => proto::MdmBaselineState::Failed,
    }
}

/// The answer's row of `record`, whose baseline the gate says is `baseline`.
fn device_proto(record: &DeviceRecord, baseline: Option<BaselineStatus>) -> proto::MdmDevice {
    let (baseline_state, baseline_reason) = baseline.map_or_else(
        || (proto::MdmBaselineState::Unspecified, String::new()),
        |status| (baseline_proto(status.state), status.reason),
    );
    proto::MdmDevice {
        enrollment_id: record.enrollment_id.as_str().to_owned(),
        udid: record.udid.clone(),
        serial: record.serial.clone().unwrap_or_default(),
        authenticate_serial: record.authenticate_serial.clone().unwrap_or_default(),
        phase: phase_proto(record.phase).into(),
        enrollments: record.enrollment,
        supervised: record.supervised,
        enrolled_via_dep: record.enrolled_via_dep,
        user_approved: record.user_approved,
        enrolled_since_unix: record.enrolled_since_unix,
        last_checkin_unix: record.last_checkin_unix,
        checked_out_unix: record.checked_out_unix,
        device_information_unix: record.device_information_unix,
        security_info_unix: record.security_info_unix,
        unanswered: record
            .sent
            .iter()
            .map(|sent| proto::MdmCommand {
                request_type: sent.request_type.clone(),
                command_uuid: sent.uuid.as_str().to_owned(),
                sent_unix: sent.sent_unix,
            })
            .collect(),
        last_error: record.last_error.clone().unwrap_or_default(),
        install_sends: record.install.as_ref().map_or(0, |install| install.sends),
        install_last_sent_unix: record
            .install
            .as_ref()
            .map(|install| install.last.sent_unix),
        install_sha256: record
            .install
            .as_ref()
            .map(|install| install.sha256.as_str().to_owned())
            .unwrap_or_default(),
        baseline_state: baseline_state.into(),
        baseline_reason,
    }
}

#[cfg(test)]
#[path = "operator_mdm_tests.rs"]
mod tests;
