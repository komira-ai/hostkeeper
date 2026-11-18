//! The coordinator's `Console` calls of `hostkeeper.operator.v1` (`docs/design/console-broker.md`):
//! a host's serial console, power and boot device through its BMC, or a Mac's screen.
//!
//! The service is declared, served and authenticated, and gives no console yet: the console
//! operators file (decision D9) is what grants access, and this coordinator reads none, so every
//! operator is refused `no_console_access`, the refusal the design gives when there is no such
//! file. A CI token is `forbidden` and no token `unauthorized`, before the request is looked at,
//! as for every operator call. A refusal of an operator who is not in the operators file is not
//! audited (section 12), so these calls write nothing and contact no device.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use crate::operator::Caller;
use crate::service::Coordinator;

#[cfg(test)]
mod tests;

impl Coordinator {
    /// `Console/Open`: refused, as every call of the service is ([`Self::console_access`]).
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_open(
        &self,
        caller: &Caller,
        _request: &proto::OpenConsoleRequest,
    ) -> Result<proto::OpenConsoleResponse, Refusal> {
        Err(Self::console_access(caller))
    }

    /// `Console/Power`: refused, as every call of the service is ([`Self::console_access`]).
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_power(
        &self,
        caller: &Caller,
        _request: &proto::ConsolePowerRequest,
    ) -> Result<proto::ConsolePowerResponse, Refusal> {
        Err(Self::console_access(caller))
    }

    /// `Console/SetBootDevice`: refused, as every call of the service is
    /// ([`Self::console_access`]).
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_set_boot_device(
        &self,
        caller: &Caller,
        _request: &proto::SetBootDeviceRequest,
    ) -> Result<proto::SetBootDeviceResponse, Refusal> {
        Err(Self::console_access(caller))
    }

    /// `Console/List`: refused, as every call of the service is ([`Self::console_access`]).
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_list(
        &self,
        caller: &Caller,
        _request: &proto::ListConsoleSessionsRequest,
    ) -> Result<proto::ListConsoleSessionsResponse, Refusal> {
        Err(Self::console_access(caller))
    }

    /// `Console/Close`: refused, as every call of the service is ([`Self::console_access`]).
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_close(
        &self,
        caller: &Caller,
        _request: &proto::CloseConsoleRequest,
    ) -> Result<proto::CloseConsoleResponse, Refusal> {
        Err(Self::console_access(caller))
    }

    /// Why `caller` gets no console: a caller that is no operator is refused as for every
    /// operator call, and an operator because this coordinator has no console operators file.
    fn console_access(caller: &Caller) -> Refusal {
        match caller.operator() {
            Ok(_) => Refusal::NoConsoleAccess,
            Err(denied) => denied.into(),
        }
    }
}
