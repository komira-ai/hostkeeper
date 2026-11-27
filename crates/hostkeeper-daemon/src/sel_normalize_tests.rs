//! The records a [`SelReader`] makes of each kind of SEL entry, compared whole
//! (docs/design/host-health-alerts.md section 8, "Normalization, per source"): the incident's
//! fixture, whose uncorrectable error on DIMM D1 is the Linux record of section 2.2 byte for byte,
//! and the kinds fixture, one entry of each kind the decoder knows or keeps.

use super::*;

/// Catches the incident's entry decoded into anything but the record of
/// docs/design/host-health-alerts.md section 2.2, less what the coordinator adds: its id of
/// record id and erase time, its time, `of_previous_boot` (it is before the boot it is read in),
/// `memory`, `DIMM D1` named by the SDR, `critical`, `memory_uncorrectable`, its message and its
/// raw line byte for byte. Also catches the two corrected errors of the hour before not counted
/// into one `info` record, and the boot entry not kept.
#[test]
fn the_incident_entry_gives_the_record_of_section_2_2_byte_for_byte() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-incident", &bmc, INCIDENT_BOOT);
    assert_eq!(rig.reader.poll(INCIDENT_READ), POLL_SECS);
    let held = rig.held();
    #[rustfmt::skip]
    let expected = [
        sel_record(
            "sel:01a4@1790000000", 1_795_662_012, true, C::Memory, "DIMM D1", Sv::Critical,
            St::Event, "memory_uncorrectable", "Uncorrectable ECC error on DIMM D1",
            "01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted",
            "",
        ),
        sel_record(
            "sel:01a5@1790000000", 1_795_662_080, true, C::Board, "System Boot Initiated #0x73",
            Sv::Info, St::Event, "system_boot", "Boot by power up on System Boot Initiated #0x73",
            "01a5 | 11/26/2026 | 03:01:20 | System Boot Initiated #0x73 | Initiated by power up | Asserted",
            "",
        ),
        sel_record(
            "sel:01a2@1790000000", 1_795_660_867, true, C::Memory, "DIMM D1", Sv::Info,
            St::Event, "memory_corrected", "2 corrected ECC errors on DIMM D1 in the hour",
            "01a3 | 11/26/2026 | 02:58:30 | Memory #0x53 DIMM_D1 | Correctable ECC | Asserted",
            "",
        ),
    ];
    assert_eq!(held, expected);
    assert_eq!(rig.state(), (pb::SourceStatus::Ok, String::new()));
    assert_eq!(rig.log.lines(), ["the BMC's event log is read"]);
    assert_eq!(
        rig.keeper.health().events[0],
        held[0],
        "the critical record is the first sent"
    );
}

/// Catches an entry of section 3.1's sensor types, or of the kinds the decoder keeps as `info`,
/// normalized into another part, subject, severity, state, code, message or raw line than its
/// table's, or dropped: memory disabled, a processor's IERR, thermal trip and machine check, a
/// power supply's failure and its deassertion (`about` the failure), a fan and a temperature past
/// each threshold and back, OEM entries with and without a time, a cleared log, a pre-init time,
/// an unknown sensor type, an OS critical stop, a watchdog reset, a generic discrete state, a
/// sensor whose SDR name is not ASCII, offsets past the tables, and corrected errors counted per
/// subject and hour (with a known hour and with none). `of_previous_boot` is set for the entries
/// before the boot (15 seconds in) and for no entry whose time is not known. The conditions not
/// deasserted stay open in the mark, by sensor and offset.
#[test]
fn each_kind_of_entry_is_normalized_whole() {
    let bmc = FakeBmc::holding(KINDS_SEL, KINDS_ERASED);
    let mut rig = Rig::new("sel-kinds", &bmc, KINDS_BOOT);
    assert_eq!(rig.reader.poll(INCIDENT_READ), POLL_SECS);
    let at = |s: u64| KINDS_T + s;
    let id = |n: u16| format!("sel:{n:04x}@{KINDS_ERASED}");
    let l = |n: u16, s: u64, rest: &str| format!("{n:04x} | 11/25/2026 | 10:00:{s:02} | {rest}");
    // The entry's id, time, of the boot before, part, subject, severity, state, code, message,
    // raw line, and the id of the assertion it ends (0: none).
    #[rustfmt::skip]
    let rows = [
        (0x10, at(0), true, C::Memory, "DIMM D1", Sv::Critical, St::Asserted, "memory_disabled", "Memory device disabled on DIMM D1", l(0x10, 0, "Memory #0x53 DIMM_D1 | Memory Device Disabled | Asserted"), 0),
        (0x11, at(1), true, C::Cpu, "CPU1 Status", Sv::Critical, St::Event, "cpu_ierr", "Processor internal error (IERR) on CPU1 Status", l(0x11, 1, "Processor #0x31 CPU1 Status | IERR | Asserted"), 0),
        (0x12, at(2), true, C::Cpu, "CPU1 Status", Sv::Critical, St::Event, "cpu_thermal_trip", "Processor thermal trip on CPU1 Status", l(0x12, 2, "Processor #0x31 CPU1 Status | Thermal Trip | Asserted"), 0),
        (0x13, at(3), true, C::Cpu, "CPU1 Status", Sv::Critical, St::Event, "cpu_machine_check", "Uncorrectable machine check on CPU1 Status", l(0x13, 3, "Processor #0x31 CPU1 Status | Uncorrectable machine check exception | Asserted"), 0),
        (0x14, at(4), true, C::Power, "PSU2 Status", Sv::Critical, St::Asserted, "power_supply_failed", "Power supply failure on PSU2 Status", l(0x14, 4, "Power Supply #0x62 PSU2 Status | Failure detected | Asserted"), 0),
        (0x15, at(5), true, C::Power, "PSU2 Status", Sv::Info, St::Deasserted, "power_supply_failed", "Ended: Power supply failure on PSU2 Status", l(0x15, 5, "Power Supply #0x62 PSU2 Status | Failure detected | Deasserted"), 0x14),
        (0x16, at(6), true, C::Thermal, "FAN3", Sv::Critical, St::Asserted, "fan_failed", "Fan below its lower critical threshold on FAN3", l(0x16, 6, "Fan #0x41 FAN3 | Lower Critical going low | Asserted"), 0),
        (0x17, at(7), true, C::Thermal, "CPU1 Temp", Sv::Warning, St::Asserted, "thermal_critical", "Temperature above its upper critical threshold on CPU1 Temp", l(0x17, 7, "Temperature #0x30 CPU1 Temp | Upper Critical going high | Asserted"), 0),
        (0x18, at(8), true, C::Thermal, "CPU1 Temp", Sv::Info, St::Deasserted, "thermal_critical", "Ended: Temperature above its upper critical threshold on CPU1 Temp", l(0x18, 8, "Temperature #0x30 CPU1 Temp | Upper Critical going high | Deasserted"), 0x17),
        (0x19, at(9), true, C::Thermal, "CPU1 Temp", Sv::Warning, St::Asserted, "thermal_threshold", "Temperature above its upper non-critical threshold on CPU1 Temp", l(0x19, 9, "Temperature #0x30 CPU1 Temp | Upper Non-critical going high | Asserted"), 0),
        (0x1a, at(10), true, C::Board, "board", Sv::Info, St::Event, "bmc_oem", "OEM record of type 0xc1 in the BMC's log", l(0x1a, 10, "OEM record 0xc1 | 570100deadbeef0102"), 0),
        (0x1b, 0, false, C::Board, "board", Sv::Info, St::Event, "bmc_oem", "OEM record of type 0xe0 in the BMC's log", "001b | OEM record 0xe0 | 303132333435363738393a3b3c".to_owned(), 0),
        (0x1c, at(12), true, C::Board, "Event Logging Disabled #0x72", Sv::Info, St::Event, "sel_cleared", "Event log cleared on Event Logging Disabled #0x72", l(0x1c, 12, "Event Logging Disabled #0x72 | Log area reset/cleared | Asserted"), 0),
        (0x1d, 0, false, C::Board, "System Boot Initiated #0x73", Sv::Info, St::Event, "system_boot", "Boot by power up on System Boot Initiated #0x73", "001d | Pre-Init | 0000004660 | System Boot Initiated #0x73 | Initiated by power up | Asserted".to_owned(), 0),
        (0x1e, at(14), true, C::Board, "Sensor type 0xc5 #0x90", Sv::Info, St::Event, "bmc_event", "Sensor type 0xc5 event (offset 0x03) on Sensor type 0xc5 #0x90", l(0x1e, 14, "Sensor type 0xc5 #0x90 | Sensor-specific offset 0x03 | Asserted"), 0),
        (0x1f, at(15), false, C::Board, "OS Critical Stop #0x74", Sv::Critical, St::Event, "os_critical_stop", "OS critical stop at run time on OS Critical Stop #0x74", l(0x1f, 15, "OS Critical Stop #0x74 | Run-time stop | Asserted"), 0),
        (0x20, at(16), false, C::Board, "Watchdog2 #0x75", Sv::Warning, St::Event, "watchdog", "Watchdog hard reset on Watchdog2 #0x75", l(0x20, 16, "Watchdog2 #0x75 | Hard reset | Asserted"), 0),
        (0x21, at(17), false, C::Power, "Power Supply #0x63", Sv::Info, St::Event, "bmc_event", "Power Supply: Redundancy Lost on Power Supply #0x63", l(0x21, 17, "Power Supply #0x63 | Redundancy Lost | Asserted"), 0),
        (0x22, at(18), false, C::Memory, "Memory #0x54", Sv::Critical, St::Event, "memory_uncorrectable", "Uncorrectable ECC error on Memory #0x54", l(0x22, 18, "Memory #0x54 | Uncorrectable ECC | Asserted"), 0),
        (0x27, at(23), false, C::Memory, "DIMM D1", Sv::Info, St::Event, "bmc_event", "Memory event (type 0x75, offset 0x02) on DIMM D1", l(0x27, 23, "Memory #0x53 DIMM_D1 | Event type 0x75 offset 0x02 | Asserted"), 0),
        (0x28, at(24), false, C::Power, "Power Supply #0x63", Sv::Info, St::Event, "bmc_event", "Power Supply: Generic event 0x0b offset 0x0e on Power Supply #0x63", l(0x28, 24, "Power Supply #0x63 | Generic event 0x0b offset 0x0e | Asserted"), 0),
        (0x29, at(25), false, C::Thermal, "FAN3", Sv::Critical, St::Asserted, "fan_failed", "Fan below its lower non-recoverable threshold on FAN3", l(0x29, 25, "Fan #0x41 FAN3 | Lower Non-recoverable going low | Asserted"), 0),
        (0x2a, at(26), false, C::Thermal, "FAN3", Sv::Warning, St::Asserted, "fan_threshold", "Fan above its upper critical threshold on FAN3", l(0x2a, 26, "Fan #0x41 FAN3 | Upper Critical going high | Asserted"), 0),
        (0x2b, at(27), false, C::Thermal, "FAN3", Sv::Warning, St::Asserted, "fan_threshold", "Fan below its lower non-critical threshold on FAN3", l(0x2b, 27, "Fan #0x41 FAN3 | Lower Non-critical going low | Asserted"), 0),
        (0x2c, at(28), false, C::Power, "Voltage #0x80", Sv::Info, St::Asserted, "bmc_threshold", "Voltage crossed its lower critical threshold back on Voltage #0x80", l(0x2c, 28, "Voltage #0x80 | Lower Critical going high | Asserted"), 0),
        (0x2d, at(29), false, C::Power, "Voltage #0x80", Sv::Info, St::Asserted, "bmc_threshold", "Voltage threshold event (offset 0x0c) on Voltage #0x80", l(0x2d, 29, "Voltage #0x80 | Threshold offset 0x0c | Asserted"), 0),
        (0x2e, at(30), false, C::Memory, "DIMM D1", Sv::Info, St::Event, "bmc_event", "Memory event (offset 0x0f) on DIMM D1", l(0x2e, 30, "Memory #0x53 DIMM_D1 | Sensor-specific offset 0x0f | Asserted"), 0),
        (0x26, 0, false, C::Cpu, "CPU1 Status", Sv::Info, St::Event, "cpu_corrected_mce", "1 corrected machine check on CPU1 Status", "0026 | Unspecified | Unspecified | Processor #0x31 CPU1 Status | Correctable machine check error | Asserted".to_owned(), 0),
        (0x23, at(19), false, C::Memory, "DIMM D1", Sv::Info, St::Event, "memory_corrected", "3 corrected ECC errors on DIMM D1 in the hour", l(0x25, 21, "Memory #0x53 DIMM_D1 | Correctable ECC | Asserted"), 0),
    ];
    let expected: Vec<pb::HardwareEvent> = rows
        .iter()
        .map(
            |(n, time, before, part, subject, severity, state, code, message, raw, ends)| {
                let about = if *ends == 0 { String::new() } else { id(*ends) };
                sel_record(
                    &id(*n),
                    *time,
                    *before,
                    *part,
                    subject,
                    *severity,
                    *state,
                    code,
                    message,
                    raw,
                    &about,
                )
            },
        )
        .collect();
    let held = rig.held();
    for (got, want) in held.iter().zip(&expected) {
        assert_eq!(got, want);
    }
    assert_eq!(held.len(), expected.len());
    let mark = rig.mark();
    assert_eq!(mark.last_id, Some(0x2e));
    assert_eq!(
        (mark.entries, mark.added, mark.erased),
        (31, KINDS_ERASED + 1, KINDS_ERASED)
    );
    assert!(mark.corrected.is_empty(), "{:?}", mark.corrected);
    assert_eq!(
        mark.open.values().cloned().collect::<Vec<_>>(),
        [
            id(0x19),
            id(0x2b),
            id(0x16),
            id(0x29),
            id(0x2a),
            id(0x10),
            id(0x2c),
            id(0x2d)
        ],
        "the conditions not deasserted, by sensor and offset"
    );
}
