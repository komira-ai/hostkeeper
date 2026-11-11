use super::*;
use crate::testkit::{der, indefinite, oid_der};

#[test]
fn a_value_is_read_with_its_class_tag_and_content_and_the_rest_is_returned() {
    let bytes = [der(0x04, b"abc"), vec![9, 9]].concat();
    let (tlv, rest) = read(&bytes).unwrap();
    assert!(tlv.is_universal(OCTET_STRING));
    assert!(!tlv.constructed);
    assert_eq!(tlv.content, b"abc");
    assert_eq!(rest, &[9, 9]);
    let encoded = der(0xA2, &[]);
    let (context, _) = read(&encoded).unwrap();
    assert!(context.is_context(2));
    assert!(context.constructed);
    assert!(!context.is_universal(2));
}

#[test]
fn long_form_lengths_of_one_and_two_bytes_are_read() {
    let long = vec![7_u8; 300];
    let encoded = der(0x04, &long);
    let (tlv, rest) = read(&encoded).unwrap();
    assert_eq!(tlv.content.len(), 300);
    assert!(rest.is_empty());
    let encoded = der(0x04, &[1_u8; 200]);
    let (tlv, _) = read(&encoded).unwrap();
    assert_eq!(tlv.content.len(), 200);
}

#[test]
fn an_indefinite_length_ends_at_its_end_marker_and_holds_its_children() {
    let inner = [der(0x04, b"a"), indefinite(0x24, &der(0x04, b"b"))].concat();
    let bytes = [indefinite(0x30, &inner), vec![5]].concat();
    let (tlv, rest) = read(&bytes).unwrap();
    assert_eq!(tlv.content, inner.as_slice());
    assert_eq!(rest, &[5]);
    let kids = children(tlv.content).unwrap();
    assert_eq!(kids.len(), 2);
    assert_eq!(octets(&kids[0]).unwrap(), b"a");
    assert_eq!(octets(&kids[1]).unwrap(), b"b");
}

#[test]
fn an_octet_string_in_chunks_is_joined() {
    let chunks = indefinite(0x24, &[der(0x04, b"ab"), der(0x04, b"cd")].concat());
    let (tlv, _) = read(&chunks).unwrap();
    assert_eq!(octets(&tlv).unwrap(), b"abcd");
    let not_chunks = der(0x24, &der(0x02, &[1]));
    let (tlv, _) = read(&not_chunks).unwrap();
    assert_eq!(octets(&tlv), Err(BerError::Value));
}

#[test]
fn object_identifiers_with_one_and_many_byte_arcs_are_written_in_dots() {
    let text = |dotted: &str| oid(&read(&oid_der(dotted)).unwrap().0).unwrap();
    assert_eq!(text("1.2.840.113549.1.7.3"), "1.2.840.113549.1.7.3");
    assert_eq!(text("2.16.840.1.101.3.4.1.42"), "2.16.840.1.101.3.4.1.42");
    assert_eq!(text("1.3.14.3.2.7"), "1.3.14.3.2.7");
    assert_eq!(
        text("0.9.2342.19200300.100.1.1"),
        "0.9.2342.19200300.100.1.1"
    );
}

#[test]
fn a_value_that_is_not_an_identifier_or_is_cut_off_is_refused() {
    let refused = |encoded: Vec<u8>| oid(&read(&encoded).unwrap().0);
    assert_eq!(refused(der(0x04, &[1])), Err(BerError::Value));
    assert_eq!(refused(der(0x06, &[])), Err(BerError::Value));
    assert_eq!(refused(der(0x06, &[0x2a, 0x86])), Err(BerError::Value));
    assert_eq!(
        refused(der(
            0x06,
            &[
                0x2a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f
            ]
        )),
        Err(BerError::Value)
    );
    assert_eq!(refused(der(0x26, &[0x2a])), Err(BerError::Value));
}

#[test]
fn encodings_that_cannot_be_read_are_refused_with_the_reason() {
    assert_eq!(read(&[]), Err(BerError::Truncated));
    assert_eq!(read(&[0x04]), Err(BerError::Truncated));
    assert_eq!(read(&[0x04, 5, 1]), Err(BerError::Truncated));
    assert_eq!(read(&[0x04, 0x82, 1]), Err(BerError::Truncated));
    assert_eq!(read(&[0x1f, 1, 0]), Err(BerError::Tag));
    assert_eq!(read(&[0x04, 0x85, 1, 1, 1, 1, 1]), Err(BerError::Length));
    // An indefinite length on a primitive value, and one never closed.
    assert_eq!(read(&[0x04, 0x80, 0, 0]), Err(BerError::Length));
    assert_eq!(read(&[0x30, 0x80, 0x04, 1, 7]), Err(BerError::Truncated));
    assert_eq!(children(&[0x04, 5]), Err(BerError::Truncated));
    assert_eq!(BerError::Depth.to_string(), "the encoding nests too deep");
}

#[test]
fn nesting_deeper_than_the_limit_is_refused() {
    let mut deep = der(0x04, &[]);
    for _ in 0..=MAX_DEPTH {
        deep = indefinite(0x30, &deep);
    }
    assert_eq!(read(&deep), Err(BerError::Depth));
    let mut shallow = der(0x04, &[]);
    for _ in 0..4 {
        shallow = indefinite(0x30, &shallow);
    }
    assert!(read(&shallow).is_ok());
    // Chunks of chunks, deeper than the limit.
    let mut chunks = der(0x04, &[1]);
    for _ in 0..=MAX_DEPTH + 1 {
        chunks = der(0x24, &chunks);
    }
    let (tlv, _) = read(&chunks).unwrap();
    assert_eq!(octets(&tlv), Err(BerError::Depth));
}
