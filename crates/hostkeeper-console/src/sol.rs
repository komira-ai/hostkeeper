//! A Linux server's serial console through its BMC's Serial over LAN
//! (`docs/design/console-broker.md`, sections 5.2, 6.1, 11.1, 12 and 17.3), over the
//! [`crate::ipmi`] port.
//!
//! [`Bmc::activate_sol`] first runs the checks in one shell session of the tool: the MAC read of
//! every BMC call (nothing more is sent to a BMC with another MAC); Get Payload Activation Status
//! ([`PAYLOAD_STATUS`]), so that a payload someone else holds, or that a holder left active, is
//! [`BmcError::SolBusy`] and is never taken or deactivated (on this firmware `sol activate` takes
//! the payload from its holder); `sol info`, and `sol set` of each bit rate that is not
//! 115.2 kbit/s. That session ends, and the activation is a run of the tool of its own, `sol
//! activate` with the escape byte [`crate::ipmi::SOL_ESCAPE`] ([`Invocation::sol`]), which exits when the
//! payload's session ends for any cause: no client byte can reach a prompt that takes commands
//! (`chassis power off` typed into a shell whose SOL had ended). The cost is a gap of one session's
//! setup, a few milliseconds, between the status read and the activation.
//!
//! When the stream ends on the coordinator's side (the browser left, a limit, a close), the tool
//! is stopped and [`Bmc::deactivate_sol`] runs `sol deactivate` in a session of its own, after
//! the same MAC read: a stopped tool leaves its payload active at the BMC. When the tool ends on
//! its own, [`ended`] names why from its standard error: another holder took the payload
//! ([`BmcError::SolBusy`]), which is left to that holder; the session could not be opened (no free
//! session, a refused credential); or the BMC stopped answering.

use crate::bmc::{Bmc, BmcError, Step, ask, is_hex_byte, named, of_shell};
use crate::ipmi::{Activated, Invocation, ToolText};

/// Get Payload Activation Status (net function 0x06, command 0x4a) of payload type 1, Serial over
/// LAN: the instance capacity, then a bit for each active instance, in two bytes.
pub const PAYLOAD_STATUS: &str = "raw 0x06 0x4a 0x01";

/// The command that reads the Serial over LAN settings, the two bit rates among them.
pub const SOL_INFO: &str = "sol info";

/// The command that ends the payload's active instance.
pub const SOL_DEACTIVATE: &str = "sol deactivate";

/// The bit rate of the fleet's serial standard, 115.2 kbit/s, as `sol info` and `sol set` write
/// it.
pub const BIT_RATE: &str = "115.2";

/// What the tool writes to its standard error when the BMC ended its Serial over LAN session:
/// another session took the payload (measured, section 17.3).
const CLOSED_BY_BMC: &str = "sol session closed by bmc";

impl Bmc<'_> {
    /// Opens the host's serial console, as the module says, and hands it over as the tool's
    /// streams.
    ///
    /// # Errors
    /// [`BmcError`]: as [`Bmc::power`], and [`BmcError::SolBusy`] when the payload is active.
    pub fn activate_sol(&self) -> Result<Activated, BmcError> {
        let mut shell = self.checked()?;
        let active = payload_active(&ask(shell.as_mut(), PAYLOAD_STATUS)?)
            .ok_or(BmcError::Failed(Step::PayloadStatus))?;
        if active {
            return Err(BmcError::SolBusy);
        }
        let info = ask(shell.as_mut(), SOL_INFO)?;
        for repair in bit_rate_repairs(&info) {
            // The tool says nothing of a set that worked; the next open reads the rate again.
            ask(shell.as_mut(), repair)?;
        }
        // The checks' session ends (`exit`) before the activation's opens.
        drop(shell);
        let invocation = Invocation::sol(self.address, self.credential, self.settings);
        self.tool.activate(&invocation).map_err(of_shell)
    }

    /// Ends the payload's active instance, in a session of its own after the MAC read.
    ///
    /// # Errors
    /// [`BmcError`]: as [`Bmc::power`].
    pub fn deactivate_sol(&self) -> Result<(), BmcError> {
        let mut shell = self.checked()?;
        ask(shell.as_mut(), SOL_DEACTIVATE).map(drop)
    }
}

/// Why an activated run ended on the tool's side, from what it wrote to its standard error:
/// [`BmcError::SolBusy`] when the BMC gave the payload to another session, [`BmcError::Busy`] or
/// [`BmcError::AuthFailed`] when its session could not be opened, else
/// [`BmcError::Unreachable`].
#[must_use]
pub fn ended(text: &ToolText) -> BmcError {
    if taken(text) {
        BmcError::SolBusy
    } else {
        named(text).unwrap_or(BmcError::Unreachable)
    }
}

/// Whether the tool's standard error `text` says the BMC gave its payload to another session.
#[must_use]
pub fn taken(text: &ToolText) -> bool {
    text.as_str().to_ascii_lowercase().contains(CLOSED_BY_BMC)
}

/// Whether a [`PAYLOAD_STATUS`] answer says an instance is active: any bit of the two bytes after
/// the capacity. `None` for an answer that is not those three bytes.
fn payload_active(answer: &str) -> Option<bool> {
    let bytes: Vec<u8> = answer
        .split_whitespace()
        .filter(|word| is_hex_byte(word))
        .filter_map(|word| u8::from_str_radix(word, 16).ok())
        .collect();
    match bytes.as_slice() {
        [_capacity, low, high] => Some(low | high != 0),
        _ => None,
    }
}

/// The `sol set` commands for each bit rate a [`SOL_INFO`] answer gives as other than
/// [`BIT_RATE`]; a rate the answer does not give is left alone.
fn bit_rate_repairs(info: &str) -> Vec<&'static str> {
    let rate = |name: &str| {
        info.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
    };
    [
        (
            "Volatile Bit Rate (kbps)",
            "sol set volatile-bit-rate 115.2",
        ),
        (
            "Non-Volatile Bit Rate (kbps)",
            "sol set non-volatile-bit-rate 115.2",
        ),
    ]
    .into_iter()
    .filter(|(name, _)| rate(name).is_some_and(|value| value != BIT_RATE))
    .map(|(_, repair)| repair)
    .collect()
}

#[cfg(test)]
#[path = "sol_tests.rs"]
mod tests;
