//! A Linux server's power and boot device through its BMC (`docs/design/console-broker.md`,
//! decisions D6 and D8, sections 6.1, 11.3, 11.4 and 17.3), over the [`crate::ipmi`] port.
//!
//! Every call is one session of the tool, and its first command reads the BMC's MAC address in one
//! exchange ([`MAC_READ`], Get LAN Configuration Parameters, parameter 5 of channel 1). Only when
//! it equals the MAC of the host's inventory record does anything else go to the BMC; otherwise
//! the answer is [`BmcError::AddressMismatch`] and nothing was sent.
//!
//! - [`Bmc::power`] reads the power state, then sends the action: `on` instead of `cycle` to a
//!   host that is off, nothing for `status`. The answer is the BMC's acknowledgement; for `soft`,
//!   a press of the power button, the host's system shuts down in its own time.
//! - [`Bmc::set_boot_device`] sets the device (`chassis bootdev`, with `options=persistent` and
//!   `options=efiboot` as asked) and reads the boot flags back (`chassis bootparam get 5`), so the
//!   answer is what the BMC holds, not what was asked.
//!
//! A session that cannot be opened is named from the tool's standard error: no free session is
//! [`BmcError::Busy`]. At its default verbosity the tool says the same one line for a BMC that
//! did not answer and for a credential the BMC refused, so a failure the first run does not name
//! is followed by one run with `-v`, which opens a session and sends no command, and whose text
//! names a refused credential ([`BmcError::AuthFailed`]); anything else is
//! [`BmcError::Unreachable`]. The tool's text is in no error.
//!
//! A host's serial console over Serial over LAN, in the same way, is [`crate::sol`].

use hostkeeper_enrollment::names::MacAddress;

use crate::credential::BmcCredential;
use crate::ipmi::{Invocation, Settings, Shell, ShellError, Tool, ToolText};
use crate::targets::Address;

/// The command that reads the BMC's MAC address in one exchange: Get LAN Configuration
/// Parameters (net function 0x0c, command 0x02), channel 1, parameter 5, set 0, block 0. Its
/// answer is the parameter's revision and the six octets.
pub const MAC_READ: &str = "raw 0x0c 0x02 0x01 0x05 0x00 0x00";

/// The command that reads the power state.
pub const POWER_STATUS: &str = "chassis power status";

/// The command that reads the boot flags back (System Boot Options, parameter 5).
pub const BOOT_FLAGS_READ: &str = "chassis bootparam get 5";

/// A power command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    /// Read the state; send nothing.
    Status,
    /// Power on.
    On,
    /// Power off at once.
    Off,
    /// Press the power button: the system shuts down in its own time.
    Soft,
    /// Power off and on again.
    Cycle,
    /// Reset without powering off.
    Reset,
}

impl PowerAction {
    /// The action's name, as the audit line and `chassis power` take it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            PowerAction::Status => "status",
            PowerAction::On => "on",
            PowerAction::Off => "off",
            PowerAction::Soft => "soft",
            PowerAction::Cycle => "cycle",
            PowerAction::Reset => "reset",
        }
    }

    /// What the tool prints when the BMC took the command: `Chassis Power Control: <this>`.
    const fn acknowledgement(self) -> &'static str {
        match self {
            PowerAction::Status => "",
            PowerAction::On => "Up/On",
            PowerAction::Off => "Down/Off",
            PowerAction::Soft => "Soft",
            PowerAction::Cycle => "Cycle",
            PowerAction::Reset => "Reset",
        }
    }
}

/// A host's power as its BMC reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerState {
    /// On.
    On,
    /// Off.
    Off,
}

/// What a power call found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerAnswer {
    /// The state before anything was sent.
    pub state: PowerState,
    /// The command sent: [`PowerAction::Status`] when none was.
    pub sent: PowerAction,
}

/// A device a BMC boots its host from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootDevice {
    /// The network (PXE).
    Pxe,
    /// The first disk.
    Disk,
    /// The firmware's setup screen.
    Bios,
}

impl BootDevice {
    /// The device as `chassis bootdev` and the audit line name it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BootDevice::Pxe => "pxe",
            BootDevice::Disk => "disk",
            BootDevice::Bios => "bios",
        }
    }

    /// The device of boot device selector `selector` (bits 5 to 2 of the flags' second byte), if
    /// it is one of the three.
    const fn of_selector(selector: u8) -> Option<BootDevice> {
        match selector {
            0x1 => Some(BootDevice::Pxe),
            0x2 => Some(BootDevice::Disk),
            0x6 => Some(BootDevice::Bios),
            _ => None,
        }
    }
}

/// The boot flags a call sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootRequest {
    /// The device.
    pub device: BootDevice,
    /// For every boot from now on, not only the next one.
    pub persistent: bool,
    /// Boot in UEFI mode.
    pub efi: bool,
}

/// The boot flags a BMC holds, read back. With no valid override, all three are empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootFlags {
    /// The device, when the flags are valid and name one of the three.
    pub device: Option<BootDevice>,
    /// The override is for every boot.
    pub persistent: bool,
    /// The override boots in UEFI mode.
    pub efi: bool,
}

/// The step at which the BMC's answer could not be read or did not acknowledge the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The tool could not be started.
    Start,
    /// [`MAC_READ`].
    MacRead,
    /// [`POWER_STATUS`].
    PowerStatus,
    /// The power command.
    PowerCommand,
    /// `chassis bootdev`.
    BootDevice,
    /// [`BOOT_FLAGS_READ`].
    BootFlags,
    /// [`crate::sol::PAYLOAD_STATUS`].
    PayloadStatus,
}

/// Why a call to a BMC did not do what it was asked. The text is the refusal's token and holds
/// nothing of the tool's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BmcError {
    /// Nothing answered at the address, or the tool gave no answer in time.
    #[error("device_unreachable")]
    Unreachable,
    /// The BMC has no free session.
    #[error("bmc_busy")]
    Busy,
    /// The BMC refused the credential.
    #[error("bmc_auth_failed")]
    AuthFailed,
    /// The BMC at the address has another MAC than the host's record; nothing was sent to it.
    #[error("address_mismatch")]
    AddressMismatch,
    /// Another holder has the BMC's Serial over LAN payload, or took it from this session.
    #[error("bmc_sol_busy")]
    SolBusy,
    /// The tool could not be started, or an answer at this step could not be read or did not
    /// acknowledge the command.
    #[error("bmc_failed at {0:?}")]
    Failed(Step),
}

/// The BMC of one host: where it is, the MAC it must have, and how the tool reaches it.
pub struct Bmc<'a> {
    /// The tool.
    pub tool: &'a dyn Tool,
    /// The BMC's address.
    pub address: &'a Address,
    /// The one BMC credential.
    pub credential: &'a BmcCredential,
    /// How the tool is run.
    pub settings: &'a Settings,
    /// The BMC MAC of the host's inventory record.
    pub mac: MacAddress,
}

impl Bmc<'_> {
    /// Reads the host's power and sends `action`, as the module says.
    ///
    /// # Errors
    /// [`BmcError`].
    pub fn power(&self, action: PowerAction) -> Result<PowerAnswer, BmcError> {
        let mut shell = self.checked()?;
        let state = parse_power_state(&ask(shell.as_mut(), POWER_STATUS)?)
            .ok_or(BmcError::Failed(Step::PowerStatus))?;
        let sent = match (action, state) {
            (PowerAction::Cycle, PowerState::Off) => PowerAction::On,
            (asked, _) => asked,
        };
        if sent != PowerAction::Status {
            let answer = ask(shell.as_mut(), &format!("chassis power {}", sent.as_str()))?;
            let acknowledged = format!("Chassis Power Control: {}", sent.acknowledgement());
            if !answer.lines().any(|line| line.trim() == acknowledged) {
                return Err(BmcError::Failed(Step::PowerCommand));
            }
        }
        Ok(PowerAnswer { state, sent })
    }

    /// Sets the boot device as `request` says and reads the flags back, as the module says.
    ///
    /// # Errors
    /// [`BmcError`].
    pub fn set_boot_device(&self, request: BootRequest) -> Result<BootFlags, BmcError> {
        let mut shell = self.checked()?;
        let answer = ask(shell.as_mut(), &boot_device_command(request))?;
        let acknowledged = format!("Set Boot Device to {}", request.device.as_str());
        if !answer.lines().any(|line| line.trim() == acknowledged) {
            return Err(BmcError::Failed(Step::BootDevice));
        }
        parse_boot_flags(&ask(shell.as_mut(), BOOT_FLAGS_READ)?)
            .ok_or(BmcError::Failed(Step::BootFlags))
    }

    /// Whether a call would reach the host's BMC at the address: a session in which the BMC's MAC
    /// is read and compared with the record's, and nothing else is sent.
    ///
    /// # Errors
    /// [`BmcError`]: [`BmcError::AddressMismatch`] for another MAC, or the session's own error.
    pub fn check(&self) -> Result<(), BmcError> {
        self.checked().map(drop)
    }

    /// An open session in which the BMC's MAC was read and equals the record's.
    pub(crate) fn checked(&self) -> Result<Box<dyn Shell>, BmcError> {
        let mut shell = self.open()?;
        let octets =
            parse_mac(&ask(shell.as_mut(), MAC_READ)?).ok_or(BmcError::Failed(Step::MacRead))?;
        if octets != self.mac.octets() {
            return Err(BmcError::AddressMismatch);
        }
        Ok(shell)
    }

    /// A session, or the error that names why there is none.
    fn open(&self) -> Result<Box<dyn Shell>, BmcError> {
        match self.tool.open(&self.invocation(false)) {
            Ok(shell) => Ok(shell),
            Err(ShellError::Ended(text)) => match named(&text) {
                Some(named) => Err(named),
                None => Err(self.diagnose()),
            },
            Err(other) => Err(of_shell(other)),
        }
    }

    /// Why a session could not be opened, from one run with `-v` that sends no command. A run
    /// that opens one now says the first run was not answered.
    fn diagnose(&self) -> BmcError {
        match self.tool.open(&self.invocation(true)) {
            Ok(_dropped) => BmcError::Unreachable,
            Err(ShellError::Ended(text)) => named(&text).unwrap_or(BmcError::Unreachable),
            Err(other) => of_shell(other),
        }
    }

    fn invocation(&self, verbose: bool) -> Invocation<'_> {
        Invocation::new(self.address, self.credential, self.settings, verbose)
    }
}

/// `command`'s answer in `shell`.
pub(crate) fn ask(shell: &mut dyn Shell, command: &str) -> Result<String, BmcError> {
    shell.ask(command).map_err(of_shell)
}

/// The error of a run of the tool that gave no answer.
pub(crate) fn of_shell(why: ShellError) -> BmcError {
    match why {
        ShellError::Start => BmcError::Failed(Step::Start),
        ShellError::Deadline => BmcError::Unreachable,
        ShellError::Ended(text) => named(&text).unwrap_or(BmcError::Unreachable),
    }
}

/// The error the tool's standard error names, if it names one: no free session (measured, section
/// 17.3), or a credential the BMC refused (the texts of the RMCP+ handshake's refusals, which the
/// tool prints with `-v`).
pub(crate) fn named(text: &ToolText) -> Option<BmcError> {
    let text = text.as_str().to_ascii_lowercase();
    let refused = [
        "unauthorized name",
        "hmac is invalid",
        "invalid integrity check value",
    ];
    if text.contains("insufficient resources for session") {
        Some(BmcError::Busy)
    } else if refused.iter().any(|needle| text.contains(needle)) {
        Some(BmcError::AuthFailed)
    } else {
        None
    }
}

/// The `chassis bootdev` command of `request`.
fn boot_device_command(request: BootRequest) -> String {
    let options: Vec<&str> = [
        request.persistent.then_some("persistent"),
        request.efi.then_some("efiboot"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let device = request.device.as_str();
    if options.is_empty() {
        format!("chassis bootdev {device}")
    } else {
        format!("chassis bootdev {device} options={}", options.join(","))
    }
}

/// The six octets of a [`MAC_READ`] answer: the lines of two-digit hex bytes the tool prints,
/// which are exactly the parameter's revision and six octets.
fn parse_mac(answer: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = answer
        .lines()
        .filter(|line| {
            let mut words = line.split_whitespace().peekable();
            words.peek().is_some() && words.all(is_hex_byte)
        })
        .flat_map(str::split_whitespace)
        .filter_map(|word| u8::from_str_radix(word, 16).ok())
        .collect();
    match bytes.as_slice() {
        [_revision, octets @ ..] => octets.try_into().ok(),
        [] => None,
    }
}

/// Whether `word` is two hex digits.
pub(crate) fn is_hex_byte(word: &str) -> bool {
    word.len() == 2 && word.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The state of a [`POWER_STATUS`] answer: `Chassis Power is on` or `off`.
fn parse_power_state(answer: &str) -> Option<PowerState> {
    answer.lines().find_map(|line| match line.trim() {
        "Chassis Power is on" => Some(PowerState::On),
        "Chassis Power is off" => Some(PowerState::Off),
        _ => None,
    })
}

/// The flags of a [`BOOT_FLAGS_READ`] answer, from its `Boot parameter data:` line: the first
/// byte's bit 7 says the flags are valid, bit 6 for every boot, bit 5 UEFI; bits 5 to 2 of the
/// second are the device.
fn parse_boot_flags(answer: &str) -> Option<BootFlags> {
    let data = answer
        .lines()
        .find_map(|line| line.trim().strip_prefix("Boot parameter data:"))?
        .trim();
    if data.len() < 4 || data.len() % 2 != 0 {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(data.get(at..at + 2)?, 16).ok();
    let (first, second) = (byte(0)?, byte(2)?);
    if first & 0x80 == 0 {
        return Some(BootFlags::default());
    }
    Some(BootFlags {
        device: BootDevice::of_selector((second >> 2) & 0x0f),
        persistent: first & 0x40 != 0,
        efi: first & 0x20 != 0,
    })
}

#[cfg(test)]
#[path = "bmc_tests.rs"]
pub(crate) mod tests;
