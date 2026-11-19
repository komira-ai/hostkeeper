//! Why a console stream closed after its upgrade (`docs/design/console-broker.md`, section 12):
//! the close code and the reason token of the WebSocket's close frame. A browser's WebSocket
//! cannot read an HTTP status once the upgrade is answered, so every failure after it is one of
//! these, and the same token is the cause of the session's `console_close` audit line.
//!
//! The codes are in the range RFC 6455 leaves to applications (4000 to 4999). 4001 to 4012 are
//! the design's; 4013 is a BMC tool that could not be run or whose answer could not be read, the
//! refusal `bmc_failed` of the unary calls. A code is never renumbered or re-meant; a new cause
//! takes a new code.

use crate::bmc::BmcError;

/// Why a console stream closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// 4001: the device did not answer at the host's address, or its stream ended.
    DeviceUnreachable,
    /// 4002: another holder has the BMC's Serial over LAN payload, or took it from this session.
    BmcSolBusy,
    /// 4003: the BMC has no free session.
    BmcBusy,
    /// 4004: the BMC refused the coordinator's BMC credential.
    BmcAuthFailed,
    /// 4005: no byte crossed the stream either way for `--console-idle-s`.
    Idle,
    /// 4006: the stream was open for `--console-total-s`.
    TotalLimit,
    /// 4007: an operator closed the session (`Console/Close`).
    ClosedByOperator,
    /// 4008: the token of the operator who opened the session is gone, or the console operators
    /// file no longer names it.
    TokenRevoked,
    /// 4009: the BMC at the host's address reports another MAC than the host's record.
    AddressMismatch,
    /// 4010: the coordinator is stopping.
    CoordinatorStopping,
    /// 4011: the Mac refused the connection: Screen Sharing is off.
    MacScreenSharingOff,
    /// 4012: the Mac did not answer: asleep, off, or not at its address.
    MacUnreachableOrAsleep,
    /// 4013: the BMC tool could not be run, or its answer could not be read.
    BmcFailed,
}

impl Close {
    /// Every cause, in the order of its code.
    pub const ALL: [Close; 13] = [
        Close::DeviceUnreachable,
        Close::BmcSolBusy,
        Close::BmcBusy,
        Close::BmcAuthFailed,
        Close::Idle,
        Close::TotalLimit,
        Close::ClosedByOperator,
        Close::TokenRevoked,
        Close::AddressMismatch,
        Close::CoordinatorStopping,
        Close::MacScreenSharingOff,
        Close::MacUnreachableOrAsleep,
        Close::BmcFailed,
    ];

    /// The close code of the WebSocket's close frame.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Close::DeviceUnreachable => 4001,
            Close::BmcSolBusy => 4002,
            Close::BmcBusy => 4003,
            Close::BmcAuthFailed => 4004,
            Close::Idle => 4005,
            Close::TotalLimit => 4006,
            Close::ClosedByOperator => 4007,
            Close::TokenRevoked => 4008,
            Close::AddressMismatch => 4009,
            Close::CoordinatorStopping => 4010,
            Close::MacScreenSharingOff => 4011,
            Close::MacUnreachableOrAsleep => 4012,
            Close::BmcFailed => 4013,
        }
    }

    /// The reason token of the close frame and the cause of the audit line.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Close::DeviceUnreachable => "device_unreachable",
            Close::BmcSolBusy => "bmc_sol_busy",
            Close::BmcBusy => "bmc_busy",
            Close::BmcAuthFailed => "bmc_auth_failed",
            Close::Idle => "idle",
            Close::TotalLimit => "total_limit",
            Close::ClosedByOperator => "closed_by_operator",
            Close::TokenRevoked => "token_revoked",
            Close::AddressMismatch => "address_mismatch",
            Close::CoordinatorStopping => "coordinator_stopping",
            Close::MacScreenSharingOff => "mac_screen_sharing_off",
            Close::MacUnreachableOrAsleep => "mac_unreachable_or_asleep",
            Close::BmcFailed => "bmc_failed",
        }
    }
}

impl From<BmcError> for Close {
    fn from(why: BmcError) -> Self {
        match why {
            BmcError::Unreachable => Close::DeviceUnreachable,
            BmcError::SolBusy => Close::BmcSolBusy,
            BmcError::Busy => Close::BmcBusy,
            BmcError::AuthFailed => Close::BmcAuthFailed,
            BmcError::AddressMismatch => Close::AddressMismatch,
            BmcError::Failed(_) => Close::BmcFailed,
        }
    }
}

#[cfg(test)]
#[path = "close_tests.rs"]
mod tests;
