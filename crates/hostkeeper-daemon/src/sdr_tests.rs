//! The tests of `sdr`: the names the fixture `tests/fixtures/sdr/names.sdr` gives (a full, a
//! compact and an event-only sensor record, a locator that names none, a name in Unicode), and
//! records cut short or naming nothing, each passed over.

use super::*;

const NAMES_SDR: &str = include_str!("../tests/fixtures/sdr/names.sdr");

fn records() -> Vec<Vec<u8>> {
    NAMES_SDR
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte"))
                .collect()
        })
        .collect()
}

/// Catches a sensor record of one of the three types that name a sensor not read, a name read
/// from the wrong offset, or a record of another type or a name in another encoding read as a
/// name: the fixture names five sensors by owner, unit and number, and passes over the locator
/// and the Unicode name.
#[test]
fn full_compact_and_event_only_records_name_their_sensors() {
    let mut names = SensorNames::default();
    let named: Vec<bool> = records().iter().map(|r| names.add(r)).collect();
    assert_eq!(named, [true, true, true, true, true, false, false]);
    assert_eq!(names.name(0x20, 0, 0x30), Some("CPU1 Temp"));
    assert_eq!(names.name(0x20, 0, 0x53), Some("DIMM_D1"));
    assert_eq!(names.name(0x20, 0, 0x31), Some("CPU1 Status"));
    assert_eq!(names.name(0x20, 0, 0x41), Some("FAN3"));
    assert_eq!(names.name(0x20, 0, 0x62), Some("PSU2 Status"));
    assert_eq!(
        names.name(0x20, 0, 0x54),
        None,
        "a name in Unicode is not read"
    );
    assert_eq!(names.name(0x20, 0, 0x99), None);
}

/// Catches a sensor of one owner named by another's record, or a SEL entry whose logical unit
/// differs from its record's left unnamed: the owner and number must match; the unit is matched
/// first, and any unit serves when none matches.
#[test]
fn a_name_is_found_by_owner_and_number_the_unit_first() {
    let mut names = SensorNames::default();
    let compact = records()[1].clone();
    assert!(names.add(&compact));
    let mut other_unit = compact.clone();
    other_unit[6] = 0x02;
    other_unit[32..39].copy_from_slice(b"DIMM_X9");
    assert!(names.add(&other_unit));
    assert_eq!(names.name(0x20, 0, 0x53), Some("DIMM_D1"));
    assert_eq!(names.name(0x20, 2, 0x53), Some("DIMM_X9"));
    assert_eq!(
        names.name(0x20, 1, 0x53),
        Some("DIMM_D1"),
        "no name on unit 1: the first"
    );
    assert_eq!(names.name(0x2c, 0, 0x53), None, "another owner's sensor");
}

/// Catches a record cut short read past its end, or a name of spaces or NULs kept: a record
/// shorter than its type, than its name's type/length byte or than the length that byte says,
/// and an empty name, are each passed over, and a name ends at its first NUL.
#[test]
fn records_cut_short_or_with_no_name_are_passed_over() {
    let compact = records()[1].clone();
    let mut names = SensorNames::default();
    assert!(!names.add(&compact[..3]), "no record type");
    assert!(!names.add(&compact[..31]), "no type/length byte");
    assert!(!names.add(&compact[..35]), "a name shorter than its length");
    let mut blank = compact.clone();
    blank[32..39].copy_from_slice(b"   \0abc");
    assert!(!names.add(&blank), "a name of spaces, then a NUL");
    let mut nul = compact.clone();
    nul[35] = 0;
    assert!(names.add(&nul));
    assert_eq!(names.name(0x20, 0, 0x53), Some("DIM"));
    assert_eq!(names, {
        let mut one = SensorNames::default();
        one.add(&nul);
        one
    });
}

/// Catches the SDR's last addition or erase read from the wrong bytes, or a short answer read.
#[test]
fn sdr_info_is_read_from_its_fourteen_bytes() {
    let mut data = vec![0x51, 0x07, 0x00, 0xff, 0xff];
    data.extend(1_794_000_000_u32.to_le_bytes());
    data.extend(1_793_000_000_u32.to_le_bytes());
    data.push(0x00);
    assert_eq!(
        parse_info(&data),
        Some(SdrInfo {
            added: 1_794_000_000,
            erased: 1_793_000_000,
        })
    );
    assert_eq!(parse_info(&data[..13]), None);
}
