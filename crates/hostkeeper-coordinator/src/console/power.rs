//! `Console/Power` and `Console/SetBootDevice` (`docs/design/console-broker.md`, sections 11.3,
//! 11.4 and 12): a Linux server's power and boot device through its BMC.
//!
//! Each call is checked in this order: the caller and the operators file (not audited), the
//! request (not audited), the entry's `power`, the host and its BMC's addresses; then the BMC's
//! lock is taken, so that commands to one BMC run one after the other; then the audit line is
//! written (`power_action` with the action, `boot_device_set` with the device and its options);
//! then the BMC is called ([`hostkeeper_console::bmc::Bmc`]) at each of its addresses in turn
//! until one answers ([`super::address`]). The answer says at which address, and where it was
//! found. A refusal after the first two is audited as `console_denied`.

use hostkeeper_console::bmc::PowerState;
use hostkeeper_console::bmc::{Bmc, BmcError, BootDevice, BootFlags, BootRequest, PowerAction};
use hostkeeper_console::operators::Need;
use hostkeeper_console::reason::audit_reason;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;
use hostkeeper_wire::HostId;

use super::address::through_addresses;
use super::{denied, host_id, required_reason, serialized};
use crate::audit_actions::AuditOnly;
use crate::operator::Caller;
use crate::ops::AuditBy;
use crate::service::Coordinator;

impl Coordinator {
    /// `Console/Power`: reads a Linux server's power and sends the action, as the module says.
    ///
    /// # Errors
    /// The caller's refusal, `no_console_access`, `invalid_request`, `reason_required`,
    /// `reason_too_long`, `power_not_permitted`, the host's ([`Self::console_device`]),
    /// `audit_unavailable`, or the BMC's (`device_unreachable`, `bmc_busy`, `bmc_auth_failed`,
    /// `address_mismatch`, `bmc_failed`).
    pub(crate) fn console_power(
        &self,
        caller: &Caller,
        request: &proto::ConsolePowerRequest,
    ) -> Result<proto::ConsolePowerResponse, Refusal> {
        let by = self.console_operator(caller, Need::Console)?;
        let host = host_id(&request.host_id)?;
        let action = power_action(request.action)?;
        let reason = required_reason(&request.reason)?;
        let refuse = |cause| self.console_denied(by, &host, "power", &reason, cause);
        let details = [("action", action.as_str())];
        let answer = self.through_bmc(by, &host, AuditOnly::PowerAction, &reason, &details, {
            move |bmc| bmc.power(action)
        });
        let (answer, address) = answer.map_err(|cause| match cause {
            Failure::Logged(refusal) => refusal,
            Failure::Denied(refusal) => refuse(refusal),
        })?;
        Ok(proto::ConsolePowerResponse {
            state: match answer.state {
                PowerState::On => proto::PowerState::On,
                PowerState::Off => proto::PowerState::Off,
            }
            .into(),
            sent: proto_action(answer.sent).into(),
            address: Some(address),
        })
    }

    /// `Console/SetBootDevice`: sets a Linux server's boot device and reads the flags back, as
    /// the module says.
    ///
    /// # Errors
    /// As [`Self::console_power`].
    pub(crate) fn console_set_boot_device(
        &self,
        caller: &Caller,
        request: &proto::SetBootDeviceRequest,
    ) -> Result<proto::SetBootDeviceResponse, Refusal> {
        let by = self.console_operator(caller, Need::Console)?;
        let host = host_id(&request.host_id)?;
        let device = boot_device(request.device)?;
        let reason = required_reason(&request.reason)?;
        let wanted = BootRequest {
            device,
            persistent: request.persistent,
            efi: request.efi,
        };
        let details = [
            ("device", device.as_str()),
            (
                "persistent",
                if wanted.persistent { "true" } else { "false" },
            ),
            ("efi", if wanted.efi { "true" } else { "false" }),
        ];
        let refuse = |cause| self.console_denied(by, &host, "set_boot_device", &reason, cause);
        let flags = self.through_bmc(by, &host, AuditOnly::BootDeviceSet, &reason, &details, {
            move |bmc| bmc.set_boot_device(wanted)
        });
        let (flags, address): (BootFlags, _) = flags.map_err(|cause| match cause {
            Failure::Logged(refusal) => refusal,
            Failure::Denied(refusal) => refuse(refusal),
        })?;
        Ok(proto::SetBootDeviceResponse {
            device: flags
                .device
                .map_or(proto::BootDevice::Unspecified, proto_device)
                .into(),
            persistent: flags.persistent,
            efi: flags.efi,
            address: Some(address),
        })
    }

    /// Runs `call` on the BMC of `host` for `by`, after the entry's `power`, the host's device
    /// and the audit line of `action` with the operator's `reason` and `details`, holding the
    /// BMC's lock from before the line to the end of the call; at each of the BMC's addresses in
    /// turn until one answers, which the answer names.
    fn through_bmc<T>(
        &self,
        by: &(impl AuditBy + ?Sized),
        host: &HostId,
        action: AuditOnly,
        reason: &str,
        details: &[(&str, &str)],
        call: impl Fn(&Bmc<'_>) -> Result<T, BmcError>,
    ) -> Result<(T, proto::ConsoleAddress), Failure> {
        let grant = self
            .console
            .files
            .grant(by.by_name(), Need::Power)
            .map_err(|why| Failure::Denied(denied(why)))?;
        let device = self
            .console_device(host, &grant.targets, true)
            .map_err(Failure::Denied)?;
        let mac = device.mac;
        let gate = self.console.bmc(mac);
        let _serial = serialized(&self.console, &gate);
        self.console_audit(
            action,
            by,
            Some(host.as_str()),
            &audit_reason(reason, details),
        )
        .map_err(Failure::Logged)?;
        let console = &self.console;
        let answered = through_addresses(
            host.as_str(),
            device.addresses(),
            |address| console.bmc_at(address, &grant.credential, mac).check(),
            |address| call(&console.bmc_at(address, &grant.credential, mac)),
        );
        answered
            .map(|(answer, found)| (answer, found.to_proto()))
            .map_err(|why| Failure::Denied(refusal_of(host, why)))
    }
}

/// Why a call through a BMC was refused: audited already, or still to be audited as denied.
enum Failure {
    /// `audit_unavailable`: no line could be written, so none is.
    Logged(Refusal),
    /// A refusal the caller audits as `console_denied`.
    Denied(Refusal),
}

/// The refusal of a BMC's error; a failure to run the tool or read its answer is logged with its
/// step, and none holds the tool's text.
fn refusal_of(host: &HostId, why: BmcError) -> Refusal {
    match why {
        BmcError::Unreachable => Refusal::DeviceUnreachable,
        // A power call opens no payload; were a BMC to say so, it is a BMC with no room.
        BmcError::Busy | BmcError::SolBusy => Refusal::BmcBusy,
        BmcError::AuthFailed => Refusal::BmcAuthFailed,
        BmcError::AddressMismatch => Refusal::AddressMismatch,
        BmcError::Failed(step) => {
            tracing::warn!(host_id = %host, ?step, "the BMC tool failed or its answer was not read");
            Refusal::BmcFailed
        }
    }
}

/// The action of a request.
///
/// # Errors
/// `invalid_request`: unspecified, or a number this release does not know.
fn power_action(value: i32) -> Result<PowerAction, Refusal> {
    match proto::PowerAction::try_from(value) {
        Ok(proto::PowerAction::Status) => Ok(PowerAction::Status),
        Ok(proto::PowerAction::On) => Ok(PowerAction::On),
        Ok(proto::PowerAction::Off) => Ok(PowerAction::Off),
        Ok(proto::PowerAction::Soft) => Ok(PowerAction::Soft),
        Ok(proto::PowerAction::Cycle) => Ok(PowerAction::Cycle),
        Ok(proto::PowerAction::Reset) => Ok(PowerAction::Reset),
        Ok(proto::PowerAction::Unspecified) | Err(_) => Err(Refusal::InvalidRequest),
    }
}

/// The proto's value of `action`.
fn proto_action(action: PowerAction) -> proto::PowerAction {
    match action {
        PowerAction::Status => proto::PowerAction::Status,
        PowerAction::On => proto::PowerAction::On,
        PowerAction::Off => proto::PowerAction::Off,
        PowerAction::Soft => proto::PowerAction::Soft,
        PowerAction::Cycle => proto::PowerAction::Cycle,
        PowerAction::Reset => proto::PowerAction::Reset,
    }
}

/// The device of a request.
///
/// # Errors
/// `invalid_request`: unspecified, or a number this release does not know.
fn boot_device(value: i32) -> Result<BootDevice, Refusal> {
    match proto::BootDevice::try_from(value) {
        Ok(proto::BootDevice::Pxe) => Ok(BootDevice::Pxe),
        Ok(proto::BootDevice::Disk) => Ok(BootDevice::Disk),
        Ok(proto::BootDevice::Bios) => Ok(BootDevice::Bios),
        Ok(proto::BootDevice::Unspecified) | Err(_) => Err(Refusal::InvalidRequest),
    }
}

/// The proto's value of `device`.
fn proto_device(device: BootDevice) -> proto::BootDevice {
    match device {
        BootDevice::Pxe => proto::BootDevice::Pxe,
        BootDevice::Disk => proto::BootDevice::Disk,
        BootDevice::Bios => proto::BootDevice::Bios,
    }
}
