//! The tables the SEL decoder ([`crate::sel`]) reads an entry by: the sensor types of IPMI 2.0
//! table 42-3, each with the part of the machine it is (`HardwareComponent`) and the
//! sensor-specific offsets docs/design/host-health-alerts.md section 3.1 names, each offset with
//! the words `ipmitool` prints for it (the record's `raw`), the words a person reads (its
//! `message`), its `code`, its severity and what it is ([`Kind`]); the threshold offsets of table
//! 42-2 and the generic discrete states of table 42-2's event types 0x02 to 0x0C. An entry of a
//! type or offset none of these knows is kept as `info` by the decoder, never dropped.

use hostkeeper_proto::hostkeeper::agent::v1::{HardwareComponent as Part, HardwareSeverity as Sev};

/// What an entry of an offset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// It happened: an uncorrectable error, a thermal trip, a boot.
    Event,
    /// A condition began, which a later deassertion of the same sensor and offset ends: a
    /// failed power supply, a disabled memory device, a sensor past its threshold.
    Condition,
    /// A corrected error, counted per subject and hour into one `info` record
    /// (docs/design/host-health-alerts.md section 2.1).
    Corrected,
}

/// One sensor-specific offset of a sensor type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offset {
    /// The offset (the low nibble of event data 1).
    pub offset: u8,
    /// What `ipmitool sel list` prints for it.
    pub text: &'static str,
    /// What a person reads, before ` on <subject>`; for a [`Kind::Corrected`] offset the plural
    /// the hour's count is told in (`corrected ECC errors`).
    pub phrase: &'static str,
    /// The record's `code`.
    pub code: &'static str,
    pub severity: Sev,
    pub kind: Kind,
}

/// One sensor type of IPMI 2.0 table 42-3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorType {
    pub code: u8,
    /// Its name as `ipmitool` prints it.
    pub name: &'static str,
    pub component: Part,
    /// Its sensor-specific offsets this decoder knows, by offset.
    pub offsets: &'static [Offset],
}

impl SensorType {
    /// The sensor-specific offset `offset`, when this table knows it.
    #[must_use]
    pub fn offset(&self, offset: u8) -> Option<&'static Offset> {
        self.offsets.iter().find(|o| o.offset == offset)
    }
}

const fn o(
    offset: u8,
    text: &'static str,
    phrase: &'static str,
    code: &'static str,
    severity: Sev,
    kind: Kind,
) -> Offset {
    Offset {
        offset,
        text,
        phrase,
        code,
        severity,
        kind,
    }
}

use Kind::{Condition, Corrected, Event};
use Sev::{Critical, Info, Warning};

// The tables below are one offset to a line, as IPMI 2.0 table 42-3 lists them, so that each is
// read across; rustfmt would spread each offset over eight lines.

#[rustfmt::skip]
const MEMORY: &[Offset] = &[
    o(0, "Correctable ECC", "corrected ECC errors", "memory_corrected", Info, Corrected),
    o(1, "Uncorrectable ECC", "Uncorrectable ECC error", "memory_uncorrectable", Critical, Event),
    o(2, "Parity", "Memory parity error", "memory_parity", Critical, Event),
    o(3, "Memory Scrub Failed", "Memory scrub failed", "memory_scrub_failed", Critical, Event),
    o(4, "Memory Device Disabled", "Memory device disabled", "memory_disabled", Critical, Condition),
    o(5, "Correctable ECC logging limit reached", "Corrected ECC error logging limit reached", "memory_corrected_limit", Warning, Condition),
    o(6, "Presence Detected", "Memory module present", "memory_present", Info, Condition),
    o(7, "Configuration Error", "Memory configuration error", "memory_config_error", Warning, Condition),
    o(8, "Spare", "Memory spare in use", "memory_spare", Warning, Condition),
    o(9, "Throttled", "Memory throttled", "memory_throttled", Warning, Condition),
    o(10, "Critical Overtemperature", "Memory critically over temperature", "memory_overtemperature", Critical, Condition),
];

#[rustfmt::skip]
const PROCESSOR: &[Offset] = &[
    o(0, "IERR", "Processor internal error (IERR)", "cpu_ierr", Critical, Event),
    o(1, "Thermal Trip", "Processor thermal trip", "cpu_thermal_trip", Critical, Event),
    o(2, "FRB1/BIST failure", "Processor self-test failure (FRB1)", "cpu_bist_failed", Critical, Event),
    o(3, "FRB2/Hang in POST failure", "Processor hang in POST (FRB2)", "cpu_post_hang", Critical, Event),
    o(4, "FRB3/Processor Startup/Init failure", "Processor start-up failure (FRB3)", "cpu_startup_failed", Critical, Event),
    o(5, "Configuration Error", "Processor configuration error", "cpu_config_error", Warning, Condition),
    o(6, "SM BIOS Uncorrectable CPU-complex Error", "Uncorrectable CPU complex error", "cpu_complex_error", Critical, Event),
    o(7, "Presence detected", "Processor present", "cpu_present", Info, Condition),
    o(8, "Disabled", "Processor disabled", "cpu_disabled", Critical, Condition),
    o(9, "Terminator presence detected", "Processor terminator present", "cpu_terminator", Info, Condition),
    o(10, "Throttled", "Processor throttled", "cpu_throttled", Warning, Condition),
    o(11, "Uncorrectable machine check exception", "Uncorrectable machine check", "cpu_machine_check", Critical, Event),
    o(12, "Correctable machine check error", "corrected machine checks", "cpu_corrected_mce", Info, Corrected),
];

#[rustfmt::skip]
const POWER_SUPPLY: &[Offset] = &[
    o(0, "Presence detected", "Power supply present", "power_supply_present", Info, Condition),
    o(1, "Failure detected", "Power supply failure", "power_supply_failed", Critical, Condition),
    o(2, "Predictive failure", "Power supply predicts failure", "power_supply_predictive", Warning, Condition),
    o(3, "Power Supply AC lost", "Power supply input lost", "power_supply_ac_lost", Critical, Condition),
    o(4, "AC lost or out-of-range", "Power supply input lost or out of range", "power_supply_ac_lost", Critical, Condition),
    o(5, "AC out-of-range, but present", "Power supply input out of range", "power_supply_ac_range", Warning, Condition),
    o(6, "Config Error", "Power supply configuration error", "power_supply_config", Warning, Condition),
];

#[rustfmt::skip]
const POWER_UNIT: &[Offset] = &[
    o(0, "Power off/down", "Power off", "power_off", Info, Condition),
    o(1, "Power cycle", "Power cycle", "power_cycle", Info, Event),
    o(2, "240VA power down", "240VA power down", "power_240va_down", Warning, Event),
    o(3, "Interlock power down", "Interlock power down", "power_interlock_down", Warning, Event),
    o(4, "AC lost", "Power input lost", "power_ac_lost", Critical, Condition),
    o(5, "Soft-power control failure", "Soft power control failure", "power_soft_failed", Warning, Event),
    o(6, "Failure detected", "Power unit failure", "power_unit_failed", Critical, Condition),
    o(7, "Predictive failure", "Power unit predicts failure", "power_unit_predictive", Warning, Condition),
];

#[rustfmt::skip]
const DRIVE_SLOT: &[Offset] = &[
    o(0, "Drive Present", "Drive present", "drive_present", Info, Condition),
    o(1, "Drive Fault", "Drive fault", "drive_fault", Warning, Condition),
    o(2, "Predictive Failure", "Drive predicts failure", "drive_predictive", Warning, Condition),
];

#[rustfmt::skip]
const FIRMWARE: &[Offset] = &[
    o(0, "System Firmware Error", "Firmware error during POST", "post_error", Warning, Event),
    o(1, "System Firmware Hang", "Firmware hang during POST", "post_hang", Warning, Event),
    o(2, "System Firmware Progress", "Firmware progress", "post_progress", Info, Event),
];

#[rustfmt::skip]
const EVENT_LOGGING: &[Offset] = &[
    o(0, "Correctable memory error logging disabled", "Corrected memory error logging disabled", "memory_logging_disabled", Warning, Condition),
    o(1, "Event Type Logging Disabled", "Event logging disabled for a type", "sel_type_disabled", Info, Condition),
    o(2, "Log area reset/cleared", "Event log cleared", "sel_cleared", Info, Event),
    o(3, "All Event Logging Disabled", "All event logging disabled", "sel_disabled", Warning, Condition),
    o(4, "Log full", "Event log full", "sel_full", Warning, Condition),
    o(5, "Log almost full", "Event log almost full", "sel_almost_full", Info, Condition),
];

#[rustfmt::skip]
const WATCHDOG_1: &[Offset] = &[
    o(0, "BIOS Reset", "BIOS watchdog reset", "watchdog", Warning, Event),
    o(1, "OS Reset", "OS watchdog reset", "watchdog", Warning, Event),
    o(2, "OS Shut Down", "OS watchdog shut down", "watchdog", Warning, Event),
    o(3, "OS Power Down", "OS watchdog power down", "watchdog", Warning, Event),
    o(4, "OS Power Cycle", "OS watchdog power cycle", "watchdog", Warning, Event),
    o(5, "OS NMI/Diag Interrupt", "OS watchdog NMI", "watchdog", Warning, Event),
    o(6, "OS Expired", "OS watchdog expired", "watchdog", Warning, Event),
    o(7, "OS pre-timeout Interrupt", "OS watchdog pre-timeout interrupt", "watchdog", Warning, Event),
];

#[rustfmt::skip]
const SYSTEM_EVENT: &[Offset] = &[
    o(0, "System Reconfigured", "System reconfigured", "system_reconfigured", Info, Event),
    o(1, "OEM System boot event", "System boot", "system_boot", Info, Event),
    o(2, "Undetermined system hardware failure", "Undetermined hardware failure", "hardware_failure", Warning, Event),
    o(3, "Entry added to auxiliary log", "Entry added to the auxiliary log", "aux_log_entry", Info, Event),
    o(4, "PEF Action", "Event filter action", "pef_action", Info, Event),
    o(5, "Timestamp Clock Sync", "Clock synchronised", "clock_sync", Info, Event),
];

#[rustfmt::skip]
const CRITICAL_INTERRUPT: &[Offset] = &[
    o(0, "NMI/Diag Interrupt", "NMI or diagnostic interrupt", "nmi", Warning, Event),
    o(1, "Bus Timeout", "Bus timeout", "bus_timeout", Warning, Event),
    o(2, "I/O Channel check NMI", "I/O channel check NMI", "io_channel_check", Critical, Event),
    o(3, "Software NMI", "Software NMI", "software_nmi", Warning, Event),
    o(4, "PCI PERR", "PCI parity error", "pci_perr", Critical, Event),
    o(5, "PCI SERR", "PCI system error", "pci_serr", Critical, Event),
    o(6, "EISA failsafe timeout", "EISA fail-safe timeout", "eisa_timeout", Warning, Event),
    o(7, "Bus Correctable error", "corrected bus errors", "bus_corrected", Info, Corrected),
    o(8, "Bus Uncorrectable error", "Uncorrectable bus error", "bus_uncorrectable", Critical, Event),
    o(9, "Fatal NMI", "Fatal NMI", "fatal_nmi", Critical, Event),
    o(10, "Bus Fatal Error", "Fatal bus error", "bus_fatal", Critical, Event),
    o(11, "Bus Degraded", "Bus degraded", "bus_degraded", Warning, Condition),
];

#[rustfmt::skip]
const SYSTEM_BOOT: &[Offset] = &[
    o(0, "Initiated by power up", "Boot by power up", "system_boot", Info, Event),
    o(1, "Initiated by hard reset", "Boot by hard reset", "system_boot", Info, Event),
    o(2, "Initiated by warm reset", "Boot by warm reset", "system_boot", Info, Event),
    o(3, "User requested PXE boot", "Network boot requested", "system_boot", Info, Event),
    o(4, "Automatic boot to diagnostic", "Boot to diagnostics", "system_boot", Info, Event),
    o(5, "OS initiated hard reset", "Hard reset by the OS", "system_boot", Info, Event),
    o(6, "OS initiated warm reset", "Warm reset by the OS", "system_boot", Info, Event),
    o(7, "System Restart", "System restart", "system_boot", Info, Event),
];

#[rustfmt::skip]
const OS_CRITICAL_STOP: &[Offset] = &[
    o(0, "Stop during OS load/init", "OS critical stop while loading", "os_critical_stop", Critical, Event),
    o(1, "Run-time stop", "OS critical stop at run time", "os_critical_stop", Critical, Event),
    o(2, "OS graceful stop", "OS graceful stop", "os_graceful_stop", Info, Event),
    o(3, "OS graceful shutdown", "OS graceful shutdown", "os_graceful_shutdown", Info, Event),
    o(4, "PEF initiated soft shutdown", "Soft shutdown by the event filter", "pef_soft_shutdown", Info, Event),
    o(5, "Agent not responding", "Platform agent not responding", "os_agent_silent", Warning, Event),
];

#[rustfmt::skip]
const WATCHDOG_2: &[Offset] = &[
    o(0, "Timer expired", "Watchdog timer expired", "watchdog", Warning, Event),
    o(1, "Hard reset", "Watchdog hard reset", "watchdog", Warning, Event),
    o(2, "Power down", "Watchdog power down", "watchdog", Warning, Event),
    o(3, "Power cycle", "Watchdog power cycle", "watchdog", Warning, Event),
    o(8, "Timer interrupt", "Watchdog pre-timeout interrupt", "watchdog", Warning, Event),
];

const fn t(
    code: u8,
    name: &'static str,
    component: Part,
    offsets: &'static [Offset],
) -> SensorType {
    SensorType {
        code,
        name,
        component,
        offsets,
    }
}

/// The sensor types this decoder names, IPMI 2.0 table 42-3.
#[rustfmt::skip]
pub const SENSOR_TYPES: &[SensorType] = &[
    t(0x01, "Temperature", Part::Thermal, &[]),
    t(0x02, "Voltage", Part::Power, &[]),
    t(0x03, "Current", Part::Power, &[]),
    t(0x04, "Fan", Part::Thermal, &[]),
    t(0x05, "Physical Security", Part::Board, &[]),
    t(0x06, "Platform Security", Part::Board, &[]),
    t(0x07, "Processor", Part::Cpu, PROCESSOR),
    t(0x08, "Power Supply", Part::Power, POWER_SUPPLY),
    t(0x09, "Power Unit", Part::Power, POWER_UNIT),
    t(0x0A, "Cooling Device", Part::Thermal, &[]),
    t(0x0B, "Other", Part::Board, &[]),
    t(0x0C, "Memory", Part::Memory, MEMORY),
    t(0x0D, "Drive Slot / Bay", Part::Disk, DRIVE_SLOT),
    t(0x0E, "POST Memory Resize", Part::Memory, &[]),
    t(0x0F, "System Firmware Progress", Part::Board, FIRMWARE),
    t(0x10, "Event Logging Disabled", Part::Board, EVENT_LOGGING),
    t(0x11, "Watchdog1", Part::Board, WATCHDOG_1),
    t(0x12, "System Event", Part::Board, SYSTEM_EVENT),
    t(0x13, "Critical Interrupt", Part::Board, CRITICAL_INTERRUPT),
    t(0x14, "Button", Part::Board, &[]),
    t(0x15, "Module / Board", Part::Board, &[]),
    t(0x16, "Microcontroller", Part::Board, &[]),
    t(0x17, "Add-in Card", Part::Board, &[]),
    t(0x18, "Chassis", Part::Board, &[]),
    t(0x19, "Chip Set", Part::Board, &[]),
    t(0x1A, "Other FRU", Part::Board, &[]),
    t(0x1B, "Cable / Interconnect", Part::Board, &[]),
    t(0x1C, "Terminator", Part::Board, &[]),
    t(0x1D, "System Boot Initiated", Part::Board, SYSTEM_BOOT),
    t(0x1E, "Boot Error", Part::Board, &[]),
    t(0x1F, "OS Boot", Part::Board, &[]),
    t(0x20, "OS Critical Stop", Part::Board, OS_CRITICAL_STOP),
    t(0x21, "Slot / Connector", Part::Board, &[]),
    t(0x22, "System ACPI Power State", Part::Board, &[]),
    t(0x23, "Watchdog2", Part::Board, WATCHDOG_2),
    t(0x24, "Platform Alert", Part::Board, &[]),
    t(0x25, "Entity Presence", Part::Board, &[]),
    t(0x26, "Monitor ASIC", Part::Board, &[]),
    t(0x27, "LAN", Part::Board, &[]),
    t(0x28, "Management Subsys Health", Part::Board, &[]),
    t(0x29, "Battery", Part::Board, &[]),
    t(0x2A, "Session Audit", Part::Board, &[]),
    t(0x2B, "Version Change", Part::Board, &[]),
    t(0x2C, "FRU State", Part::Board, &[]),
];

/// The sensor type `code`, when this table names it.
#[must_use]
pub fn sensor_type(code: u8) -> Option<&'static SensorType> {
    SENSOR_TYPES.iter().find(|t| t.code == code)
}

/// How far past normal a threshold is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    NonCritical,
    Critical,
    NonRecoverable,
}

/// One threshold offset (IPMI 2.0 table 42-2, event type 0x01).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threshold {
    /// What `ipmitool sel list` prints for it.
    pub text: &'static str,
    /// A lower threshold (false: an upper one).
    pub lower: bool,
    pub level: Level,
    /// Whether it is the crossing away from normal (lower going low, upper going high).
    pub outward: bool,
}

const fn th(text: &'static str, lower: bool, level: Level, outward: bool) -> Threshold {
    Threshold {
        text,
        lower,
        level,
        outward,
    }
}

/// The threshold offsets 0x00 to 0x0B.
#[rustfmt::skip]
pub const THRESHOLDS: [Threshold; 12] = [
    th("Lower Non-critical going low", true, Level::NonCritical, true),
    th("Lower Non-critical going high", true, Level::NonCritical, false),
    th("Lower Critical going low", true, Level::Critical, true),
    th("Lower Critical going high", true, Level::Critical, false),
    th("Lower Non-recoverable going low", true, Level::NonRecoverable, true),
    th("Lower Non-recoverable going high", true, Level::NonRecoverable, false),
    th("Upper Non-critical going low", false, Level::NonCritical, false),
    th("Upper Non-critical going high", false, Level::NonCritical, true),
    th("Upper Critical going low", false, Level::Critical, false),
    th("Upper Critical going high", false, Level::Critical, true),
    th("Upper Non-recoverable going low", false, Level::NonRecoverable, false),
    th("Upper Non-recoverable going high", false, Level::NonRecoverable, true),
];

/// The states of the generic discrete event types 0x02 to 0x0C, by event type then offset, as
/// `ipmitool sel list` prints them.
#[rustfmt::skip]
pub const GENERIC: &[(u8, &[&str])] = &[
    (0x02, &["Transition to Idle", "Transition to Active", "Transition to Busy"]),
    (0x03, &["State Deasserted", "State Asserted"]),
    (0x04, &["Predictive Failure Deasserted", "Predictive Failure Asserted"]),
    (0x05, &["Limit Not Exceeded", "Limit Exceeded"]),
    (0x06, &["Performance Met", "Performance Lags"]),
    (0x07, &[
        "Transition to OK", "Transition to Non-critical from OK",
        "Transition to Critical from less severe", "Transition to Non-recoverable from less severe",
        "Transition to Non-critical from more severe", "Transition to Critical from Non-recoverable",
        "Transition to Non-recoverable", "Monitor", "Informational",
    ]),
    (0x08, &["Device Absent", "Device Present"]),
    (0x09, &["Device Disabled", "Device Enabled"]),
    (0x0A, &[
        "Transition to Running", "Transition to In Test", "Transition to Power Off",
        "Transition to On Line", "Transition to Off Line", "Transition to Off Duty",
        "Transition to Degraded", "Transition to Power Save", "Install Error",
    ]),
    (0x0B, &[
        "Fully Redundant", "Redundancy Lost", "Redundancy Degraded",
        "Non-Redundant: Sufficient from Redundant", "Non-Redundant: Sufficient from Insufficient",
        "Non-Redundant: Insufficient Resources", "Redundancy Degraded from Fully Redundant",
        "Redundancy Degraded from Non-redundant",
    ]),
    (0x0C, &["D0 Power State", "D1 Power State", "D2 Power State", "D3 Power State"]),
];

/// The generic discrete state of event type `event_type` at `offset`, when the table names it.
#[must_use]
pub fn generic(event_type: u8, offset: u8) -> Option<&'static str> {
    GENERIC
        .iter()
        .find(|(et, _)| *et == event_type)
        .and_then(|(_, states)| states.get(usize::from(offset)).copied())
}
