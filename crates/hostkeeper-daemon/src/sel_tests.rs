//! The tests of `sel`'s pure parts at their boundaries: the answers of `Get SEL Info` and
//! `Get SEL Entry` cut short, the three kinds of SEL time and the boundary between them, the
//! calendar of the raw line across a leap day, a year's end and a century that is not leap, and an
//! entry's bytes read field by field. Each entry kind is normalized whole in
//! `sel_normalize_tests.rs`, over the fixtures.

use super::*;

/// Catches the SEL's count, last addition or last erase read from the wrong bytes, or a short
/// answer read as a SEL: 14 bytes read, 13 refused.
#[test]
fn sel_info_is_read_from_its_fourteen_bytes() {
    let mut data = vec![0x51, 0x34, 0x12, 0xff, 0xff];
    data.extend(1_795_662_012_u32.to_le_bytes());
    data.extend(1_790_000_000_u32.to_le_bytes());
    data.push(0x02);
    assert_eq!(
        parse_info(&data),
        Some(SelInfo {
            entries: 0x1234,
            added: 1_795_662_012,
            erased: 1_790_000_000,
        })
    );
    assert_eq!(parse_info(&data[..13]), None);
}

/// Catches the next record id read big-endian, or an answer shorter than it taken as one.
#[test]
fn the_next_id_is_the_first_two_bytes() {
    assert_eq!(
        split_next(&[0xa5, 0x01, 7, 8]),
        Some((0x01a5, &[7_u8, 8][..]))
    );
    assert_eq!(split_next(&[0xff, 0xff]), Some((LAST_RECORD, &[][..])));
    assert_eq!(split_next(&[0xa5]), None);
}

/// Catches a time at the pre-init bound read as a date, one past it as pre-init, or the
/// unspecified time read as a date: at most 0x20000000 is pre-init, 0xFFFFFFFF unspecified,
/// and neither has a Unix second.
#[test]
fn a_sel_time_is_a_date_pre_init_or_unspecified() {
    assert_eq!(SelTime::of(PRE_INIT_MAX), SelTime::PreInit(PRE_INIT_MAX));
    assert_eq!(SelTime::of(PRE_INIT_MAX + 1), SelTime::At(PRE_INIT_MAX + 1));
    assert_eq!(SelTime::of(u32::MAX), SelTime::Unspecified);
    assert_eq!(SelTime::of(0), SelTime::PreInit(0));
    assert_eq!(SelTime::At(1_795_662_012).unix(), 1_795_662_012);
    assert_eq!(SelTime::PreInit(4660).unix(), 0);
    assert_eq!(SelTime::Unspecified.unix(), 0);
}

/// Catches the raw line's calendar off by a day at a leap day, at a year's end, or in a century
/// year that is not leap.
#[test]
fn the_raw_line_dates_are_utc_calendar_dates() {
    assert_eq!(
        SelTime::At(1_835_395_200).columns(),
        "02/29/2028 | 00:00:00"
    );
    assert_eq!(
        SelTime::At(1_798_761_599).columns(),
        "12/31/2026 | 23:59:59"
    );
    assert_eq!(
        SelTime::At(1_798_761_600).columns(),
        "01/01/2027 | 00:00:00"
    );
    assert_eq!(
        SelTime::At(4_107_587_696).columns(),
        "03/01/2100 | 12:34:56"
    );
    assert_eq!(SelTime::PreInit(4660).columns(), "Pre-Init | 0000004660");
    assert_eq!(SelTime::Unspecified.columns(), "Unspecified | Unspecified");
}

/// Catches an entry of another length than 16 read, or a field read from the wrong byte or bits:
/// the record id, type and time, the generator and its logical unit (not its channel), the
/// sensor, the direction bit apart from the event type, the three event data bytes and the
/// offset in the low nibble of the first.
#[test]
fn an_entry_is_read_field_by_field() {
    let bytes = [
        0xa4, 0x01, 0x02, 0xbc, 0xa0, 0x07, 0x6b, 0x41, 0x13, 0x04, 0x0c, 0x53, 0xef, 0xa1, 0x00,
        0x03,
    ];
    let record = SelRecord::parse(&bytes).expect("an entry");
    assert_eq!(
        record,
        SelRecord {
            id: 0x01a4,
            record_type: 0x02,
            time: SelTime::At(1_795_662_012),
            generator: 0x41,
            lun: 0x03,
            sensor_type: 0x0c,
            sensor_number: 0x53,
            deassertion: true,
            event_type: 0x6f,
            data: [0xa1, 0x00, 0x03],
            bytes,
        }
    );
    assert_eq!(record.offset(), 1);
    assert!(!record.is_oem());
    assert_eq!(SelRecord::parse(&bytes[..15]), None);
    let mut long = bytes.to_vec();
    long.push(0);
    assert_eq!(SelRecord::parse(&long), None);
    let mut oem = bytes;
    oem[2] = 0xc0;
    assert!(SelRecord::parse(&oem).expect("an OEM entry").is_oem());
    oem[2] = 0xe0;
    assert_eq!(
        SelRecord::parse(&oem).expect("an OEM entry").time,
        SelTime::Unspecified,
        "an OEM entry with no timestamp has no time, whatever its bytes"
    );
}

/// Catches a condition's key that does not tell two logical units, two sensors, two event types
/// or two offsets apart, so a deassertion would end another condition.
#[test]
fn a_condition_is_keyed_by_generator_unit_sensor_type_and_offset() {
    let entry = |generator: u8, lun: u8, number: u8, event_type: u8, data1: u8| {
        let mut bytes = [0_u8; SEL_RECORD_LEN];
        bytes[2] = 0x02;
        bytes[3..7].copy_from_slice(&1_795_662_012_u32.to_le_bytes());
        bytes[7] = generator;
        bytes[8] = lun;
        bytes[10] = 0x08;
        bytes[11] = number;
        bytes[12] = event_type;
        bytes[13] = data1;
        decode(
            &SelRecord::parse(&bytes).expect("an entry"),
            &SensorNames::default(),
        )
        .condition
    };
    assert_eq!(entry(0x20, 0, 0x62, 0x6f, 0x01), "20.0:62:6f:1");
    assert_eq!(entry(0x20, 0x11, 0x62, 0x6f, 0x01), "20.1:62:6f:1");
    assert_eq!(entry(0x2c, 0, 0x62, 0x6f, 0x01), "2c.0:62:6f:1");
    assert_eq!(entry(0x20, 0, 0x63, 0x6f, 0x01), "20.0:63:6f:1");
    assert_eq!(entry(0x20, 0, 0x62, 0x0b, 0x01), "20.0:62:0b:1");
    assert_eq!(entry(0x20, 0, 0x62, 0x6f, 0xa2), "20.0:62:6f:2");
}
