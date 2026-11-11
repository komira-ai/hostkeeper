//! Who a Mac is approved as (design 4.6): the two sources the Mac path has, and the conditions
//! each holds a Mac to before its nonce is looked at.
//!
//! **The allowlist** (`--mac-serial-allowlist`) is the override, and wins for the serials it
//! lists: its entry gives the host id, group and services, and a group the coordinator does not
//! hold is refused as an operator's approval would be. The Mac must be enrolled and supervised,
//! and enrolled through ADE only when the coordinator is set to require it
//! ([`Enrollment::with_mac_require_dep`]).
//!
//! **Our ABM** approves every other serial, when the coordinator has an ABM side
//! ([`Enrollment::abm_by`]), and only when all of these hold, each declined with its own token:
//!
//! 1. the serial is in the ABM table and not marked removed (`serial_not_in_abm`);
//! 2. the gate says the Mac is enrolled (`device_not_enrolled`), supervised
//!    (`device_not_supervised`) and enrolled through ADE (`device_not_via_dep`; an unanswered
//!    `SecurityInfo` is not a yes);
//! 3. a group resolves (`no_group`): the group of the serial's inventory record, else the setting
//!    `default_group`, and the coordinator holds a group of that name. A coordinator that holds no
//!    host groups resolves none, so there the allowlist is the only source.
//!
//! The host id is the inventory record's `host_id`, else the setting `host_id_prefix` and the
//! serial in lower case. A Mac of our ABM is assigned no service: its group says what it runs.
//! The ABM table or the settings failing to read leaves the request pending, refused
//! `store_failed`; a failing gate is `device_state_unavailable`.
//!
//! [`Enrollment::confirm_in_abm`] is the one question an operator's confirmation of a waiting MDM
//! enrollment asks again: the table is read at the confirmation, so a serial ABM dropped since
//! the Mac asked gets no challenge.

use std::sync::{Arc, PoisonError};

use hostkeeper_enrollment::names::{HostId, Role};
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use super::Enrollment;
use super::abm_port::{AbmDevices, AbmUnavailable};
use super::device_state::DeviceStatus;
use super::groups::group_in;
use super::mac_nonce::{MacAllowEntry, MacDecline, MacOutcome};
use hostkeeper_enrollment::names::KeyHash;

/// What the conditions of a Mac found: who it is approved as, the group a coordinator that holds
/// groups places it in, and the gate's answer.
pub(super) type Found = (MacAllowEntry, Option<String>, DeviceStatus);

impl Enrollment {
    /// From now on, approves a Mac of `abm` that the allowlist does not list ([`Self::abm_found`]).
    /// Called when a coordinator that has an ABM side takes this enrollment.
    pub(crate) fn abm_by(&self, abm: Arc<dyn AbmDevices>) {
        *self.abm.lock().unwrap_or_else(PoisonError::into_inner) = Some(abm);
    }

    /// The ABM side, if the coordinator has one.
    fn abm(&self) -> Option<Arc<dyn AbmDevices>> {
        self.abm
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The Mac `serial` of the allowlist `entry`.
    pub(super) fn listed(
        &self,
        request_id: &KeyHash,
        serial: &str,
        entry: &MacAllowEntry,
    ) -> Result<Found, MacOutcome> {
        let host_group = match self.held_groups() {
            None => None,
            Some(_) if !entry.services.is_empty() => {
                return Err(self.mac_refused(
                    request_id,
                    Some(&entry.host_id),
                    EnrollmentRefusal::ServicesRetired,
                ));
            }
            Some(held) => match group_in(entry.role.as_str(), "", &held) {
                Ok(group) => group.map(|group| group.to_string()),
                Err(refusal) => {
                    return Err(self.mac_refused(request_id, Some(&entry.host_id), refusal));
                }
            },
        };
        let status = self.device_status(request_id, serial, self.mac_require_dep)?;
        Ok((entry.clone(), host_group, status))
    }

    /// The Mac `serial`, which the allowlist does not list: approved from our ABM when it can be.
    pub(super) fn abm_found(
        &self,
        request_id: &KeyHash,
        serial: &str,
    ) -> Result<Found, MacOutcome> {
        let Some(abm) = self.abm() else {
            return Err(self.decline(request_id, serial, MacDecline::SerialNotAllowed));
        };
        let unread = |why: AbmUnavailable| {
            tracing::error!(%why, "the ABM side could not be read: a Mac stays pending");
            self.mac_refused(request_id, None, EnrollmentRefusal::StoreFailed)
        };
        if !abm.serial_in_abm(serial).map_err(unread)? {
            return Err(self.decline(request_id, serial, MacDecline::SerialNotInAbm));
        }
        let status = self.device_status(request_id, serial, true)?;
        let naming = abm.naming().map_err(unread)?;
        let record = self.ops().and_then(|ops| {
            ops.read(|state| {
                state
                    .inventory
                    .iter()
                    .find(|record| {
                        !record.serial.is_empty() && record.serial.eq_ignore_ascii_case(serial)
                    })
                    .cloned()
            })
        });
        let named = record
            .as_ref()
            .map(|record| record.group())
            .filter(|group| !group.is_empty());
        let group = named
            .map(str::to_owned)
            .or(naming.default_group)
            .unwrap_or_default();
        let resolved = self
            .held_groups()
            .and_then(|held| group_in(&group, "", &held).ok().flatten())
            .and_then(|name| Role::parse(name.as_str()).ok().map(|role| (name, role)));
        let Some((name, role)) = resolved else {
            return Err(self.decline(request_id, serial, MacDecline::NoGroup));
        };
        let host = match &record {
            Some(record) => record.host_id.clone(),
            None => format!("{}{}", naming.host_id_prefix, serial.to_ascii_lowercase()),
        };
        let Ok(host_id) = HostId::parse(&host) else {
            return Err(self.mac_refused(request_id, None, EnrollmentRefusal::InvalidHostId));
        };
        let entry = MacAllowEntry {
            host_id,
            role,
            services: Vec::new(),
        };
        Ok((entry, Some(name.to_string()), status))
    }

    /// The gate's answer about `serial`, when it says the Mac is enrolled, supervised and, when
    /// `via_dep`, enrolled through ADE.
    fn device_status(
        &self,
        request_id: &KeyHash,
        serial: &str,
        via_dep: bool,
    ) -> Result<DeviceStatus, MacOutcome> {
        let decline = |why| Err(self.decline(request_id, serial, why));
        match self.mac.device_state.status(serial) {
            Err(why) => {
                tracing::warn!(%why, "the device-state gate gave no answer");
                decline(MacDecline::DeviceStateUnavailable)
            }
            Ok(status) if !status.enrolled => decline(MacDecline::DeviceNotEnrolled),
            Ok(status) if !status.supervised => decline(MacDecline::DeviceNotSupervised),
            Ok(status) if via_dep && status.enrolled_via_dep != Some(true) => {
                decline(MacDecline::DeviceNotViaDep)
            }
            Ok(status) => Ok(status),
        }
    }

    /// Whether `serial` is in our ABM now, read again: `Ok(())` when it is; otherwise the decline
    /// to answer with, `serial_not_in_abm`. An operator's confirmation of a waiting MDM enrollment
    /// asks this at the confirmation, so a serial ABM removed since the Mac asked gets no
    /// challenge.
    ///
    /// # Errors
    /// [`ConfirmRefusal::Declined`] with `serial_not_in_abm` when the serial is not in the table
    /// (also when the coordinator has no ABM side); [`ConfirmRefusal::Unread`] when the table
    /// cannot be read.
    pub fn confirm_in_abm(&self, serial: &str) -> Result<(), ConfirmRefusal> {
        let in_abm = match self.abm() {
            None => false,
            Some(abm) => abm.serial_in_abm(serial).map_err(|why| {
                tracing::error!(%why, "the ABM table could not be read for a confirmation");
                ConfirmRefusal::Unread
            })?,
        };
        if in_abm {
            Ok(())
        } else {
            Err(ConfirmRefusal::Declined(MacDecline::SerialNotInAbm))
        }
    }
}

/// Why a confirmation was not given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmRefusal {
    /// The serial is not in our ABM now.
    Declined(MacDecline),
    /// The table could not be read.
    Unread,
}
