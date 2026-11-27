//! The AER lines of newer kernels (`AER: <kind> error message received from <device>`, with
//! `Correctable` and `Uncorrectable` kinds), read as the older `error received: <device>` lines
//! are.

use super::*;

fn line(usec: u64, text: &str) -> KernelLine {
    KernelLine {
        boot_id: "6d3a1c0e-9b2f-4e57-a1c4-2f8e7d6b5a49".to_owned(),
        usec,
        time_unix: 1_795_600_000 + usec / 1_000_000,
        text: text.to_owned(),
    }
}

/// Each kind of the newer wording gives the record of the older one, by the device named after
/// `from`.
#[test]
fn aer_lines_of_newer_kernels_name_the_device_after_from() {
    let lines = [
        line(
            1_000_000,
            "pcieport 0000:00:1c.0: AER: Correctable error message received from 0000:3b:00.0",
        ),
        line(
            2_000_000,
            "pcieport 0000:00:01.0: AER: Uncorrectable (Fatal) error message received from \
             0000:af:00.0",
        ),
        line(
            3_000_000,
            "pcieport 0000:00:01.0: AER: Uncorrectable (Non-Fatal) error message received from \
             0000:af:00.1",
        ),
    ];
    let records: Vec<(String, i32, String, String)> = classify(&lines, false)
        .into_iter()
        .map(|e| (e.code, e.severity, e.subject, e.message))
        .collect();
    let critical = i32::from(pb::HardwareSeverity::Critical);
    assert_eq!(
        records,
        [
            (
                "pcie_corrected".to_owned(),
                i32::from(pb::HardwareSeverity::Info),
                "0000:3b:00.0".to_owned(),
                "Corrected PCIe error on 0000:3b:00.0 (AER)".to_owned(),
            ),
            (
                "pcie_fatal".to_owned(),
                critical,
                "0000:af:00.0".to_owned(),
                "Fatal PCIe error on 0000:af:00.0 (AER)".to_owned(),
            ),
            (
                "pcie_uncorrected".to_owned(),
                critical,
                "0000:af:00.1".to_owned(),
                "Uncorrected PCIe error on 0000:af:00.1 (AER)".to_owned(),
            ),
        ]
    );
}

/// A newer line that names no device after `from` is the reporting port's.
#[test]
fn aer_lines_of_newer_kernels_without_a_device_are_the_ports() {
    let lines = [line(
        1_000_000,
        "pcieport 0000:00:1c.0: AER: Correctable error message received from id=00e0",
    )];
    let records = classify(&lines, false);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].subject, "0000:00:1c.0");
}
