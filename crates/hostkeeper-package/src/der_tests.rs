use super::*;
use crate::testing::{
    BUILD_SIGNER_URI, BundleSpec, RUNNER_ENVIRONMENT, SOURCE_REPOSITORY_DIGEST,
    SOURCE_REPOSITORY_OWNER_URI, certificate, extension as build_extension, tlv as build,
};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn spec() -> BundleSpec<'static> {
    BundleSpec {
        organisation: "example-org",
        repository: "tool",
        workflow: ".github/workflows/artifacts.yml",
        source_ref: "refs/heads/main",
        certificate_commit: COMMIT,
        provenance_commit: COMMIT,
        runner: "github-hosted",
        subject_name: "tool.tar",
        subject_sha256: COMMIT,
    }
}

/// A certificate whose extensions list holds exactly `list` (already encoded entries).
fn certificate_of(list: &[u8]) -> Vec<u8> {
    let tbs = build(0xa3, &build(0x30, list));
    build(0x30, &build(0x30, &tbs))
}

/// One valid extension entry for the source ref arc.
fn good_entry(value: &str) -> Vec<u8> {
    build_extension(SOURCE_REPOSITORY_REF, value)
}

fn ref_oid() -> Vec<u8> {
    fulcio(SOURCE_REPOSITORY_REF)
}

/// An extension entry for the source ref arc made of `parts` after its identifier.
fn entry_with(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut body = build(0x06, &ref_oid());
    for part in parts {
        body.extend_from_slice(part);
    }
    build(0x30, &body)
}

/// A short length is one byte, and the rest of the input is handed back.
/// Catches a reader that drops the trailing bytes or misreads the length.
#[test]
fn a_short_length_is_read_and_the_rest_is_returned() {
    let input = [0x04, 0x02, 1, 2, 3, 4];
    assert_eq!(tlv(&input), Some((0x04, &[1, 2][..], &[3, 4][..])));
    assert_eq!(tlv(&[0x05, 0x00]), Some((0x05, &[][..], &[][..])));
}

/// The long form with one, two and four length bytes gives the same element as the short form.
/// Catches a reader that mis-assembles a multi-byte length.
#[test]
fn long_lengths_of_one_two_and_four_bytes_are_read() {
    let mut one = vec![0x04, 0x81, 0x80];
    one.extend(vec![7; 0x80]);
    one.push(9);
    let (tag, content, rest) = tlv(&one).expect("one length byte");
    assert_eq!((tag, content.len(), rest), (0x04, 0x80, &[9][..]));

    let mut two = vec![0x30, 0x82, 0x01, 0x02];
    two.extend(vec![8; 0x102]);
    let (tag, content, rest) = tlv(&two).expect("two length bytes");
    assert_eq!((tag, content.len(), rest.len()), (0x30, 0x102, 0));

    let four = [0x04, 0x84, 0, 0, 0, 2, 9, 8, 7];
    assert_eq!(tlv(&four), Some((0x04, &[9, 8][..], &[7][..])));
}

/// A length of zero digits (indefinite) or more than four bytes is refused.
/// Catches a reader that follows an indefinite length or overflows on a huge one.
#[test]
fn a_length_of_more_than_four_bytes_or_indefinite_is_refused() {
    assert_eq!(tlv(&[0x30, 0x80, 0x04, 0x00, 0x00, 0x00]), None);
    assert_eq!(tlv(&[0x04, 0x85, 0, 0, 0, 0, 1, 1]), None);
    assert_eq!(tlv(&[0x04, 0xff, 1, 2]), None);
}

/// The high tag number form is refused.
/// Catches a reader that takes the next byte of a multi-byte tag for the length.
#[test]
fn a_high_tag_number_is_refused() {
    assert_eq!(tlv(&[0x1f, 0x01, 0x00]), None);
    assert_eq!(tlv(&[0xbf, 0x01, 0x00]), None);
}

/// Input that ends early (nothing, a tag alone, cut length digits, cut content) is refused.
/// Catches a reader that panics or invents bytes past the end of its input.
#[test]
fn truncated_input_is_refused() {
    assert_eq!(tlv(&[]), None);
    assert_eq!(tlv(&[0x04]), None);
    assert_eq!(tlv(&[0x04, 0x82, 0x01]), None);
    assert_eq!(tlv(&[0x04, 0x81]), None);
    assert_eq!(tlv(&[0x04, 0x05, 1, 2]), None);
    assert_eq!(tlv(&[0x04, 0x81, 0x05, 1]), None);
}

/// The Fulcio identifier of arc 14 is the bytes of 1.3.6.1.4.1.57264.1.14.
/// Catches a wrong base identifier or arc encoding.
#[test]
fn the_fulcio_identifier_of_arc_14_is_its_der_content() {
    assert_eq!(
        fulcio(14),
        vec![0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01, 0x0e]
    );
    assert_eq!(fulcio(0xff).last(), Some(&0x7f));
}

/// The source ref and the other arcs are each read from their own extension.
/// Catches a reader that returns the first extension, or confuses arcs.
#[test]
fn the_source_ref_is_read_from_its_fulcio_extension() {
    let cert = certificate(&spec());
    assert_eq!(
        utf8_extension(&cert, &fulcio(SOURCE_REPOSITORY_REF)),
        Some("refs/heads/main")
    );
    assert_eq!(
        utf8_extension(&cert, &fulcio(SOURCE_REPOSITORY_DIGEST)),
        Some(COMMIT)
    );
    assert_eq!(
        utf8_extension(&cert, &fulcio(BUILD_SIGNER_URI)),
        Some("https://github.com/example-org/tool/.github/workflows/artifacts.yml@refs/heads/main")
    );
    assert_eq!(
        utf8_extension(&cert, &fulcio(RUNNER_ENVIRONMENT)),
        Some("github-hosted")
    );
    assert_eq!(
        utf8_extension(&cert, &fulcio(SOURCE_REPOSITORY_OWNER_URI)),
        Some("https://github.com/example-org")
    );
}

/// An arc the certificate does not carry reads as absent.
/// Catches a reader that answers with some other extension's value.
#[test]
fn an_absent_arc_reads_as_none() {
    let cert = certificate(&spec());
    assert_eq!(utf8_extension(&cert, &fulcio(99)), None);
}

/// A critical flag before the value does not hide the value.
/// Catches a reader that expects the value right after the identifier.
#[test]
fn a_critical_flag_before_the_value_is_skipped() {
    let entry = entry_with(&[
        build(0x01, &[0xff]),
        build(0x04, &build(0x0c, b"refs/tags/v2")),
    ]);
    let cert = certificate_of(&entry);
    assert_eq!(utf8_extension(&cert, &ref_oid()), Some("refs/tags/v2"));
}

/// A value that is not a UTF8String, or not valid UTF-8, reads as absent.
/// Catches a reader that returns raw bytes of another type or panics on bad UTF-8.
#[test]
fn a_value_that_is_not_utf8_text_reads_as_none() {
    let octets = entry_with(&[build(0x04, &build(0x04, b"refs/tags/v2"))]);
    assert_eq!(utf8_extension(&certificate_of(&octets), &ref_oid()), None);

    let invalid = entry_with(&[build(0x04, &build(0x0c, &[0xff, 0xfe]))]);
    assert_eq!(utf8_extension(&certificate_of(&invalid), &ref_oid()), None);
}

/// Anything that is not a certificate with a tbsCertificate and a `[3]` list of SEQUENCEs
/// reads as absent.
/// Catches a reader that accepts the wrong tag at any level of the nesting.
#[test]
fn a_malformed_certificate_reads_as_none() {
    let good = good_entry("refs/tags/v2");
    let tbs = build(0xa3, &build(0x30, &good));

    let not_sequence = build(0x31, &build(0x30, &tbs));
    assert_eq!(utf8_extension(&not_sequence, &ref_oid()), None);

    let tbs_not_sequence = build(0x30, &build(0x31, &tbs));
    assert_eq!(utf8_extension(&tbs_not_sequence, &ref_oid()), None);

    let no_extensions = build(0x30, &build(0x30, &build(0x02, &[1])));
    assert_eq!(utf8_extension(&no_extensions, &ref_oid()), None);

    let list_not_sequence = build(0x30, &build(0x30, &build(0xa3, &build(0x31, &good))));
    assert_eq!(utf8_extension(&list_not_sequence, &ref_oid()), None);

    assert_eq!(utf8_extension(&[], &ref_oid()), None);
}

/// Entries that are not SEQUENCEs, or have no identifier first, or no value, are passed over
/// and a later valid entry is still found.
/// Catches a reader that stops at the first odd entry.
#[test]
fn odd_entries_are_skipped_and_a_later_match_is_found() {
    let mut list = build(0x31, &good_entry("refs/tags/v1"));
    list.extend(build(0x30, &build(0x04, b"not an identifier")));
    list.extend(entry_with(&[]));
    list.extend(entry_with(&[build(0x01, &[0xff])]));
    list.extend(build_extension(9, "other"));
    list.extend(good_entry("refs/tags/v3"));
    assert_eq!(
        utf8_extension(&certificate_of(&list), &ref_oid()),
        Some("refs/tags/v3")
    );
}

/// A malformed entry alone gives nothing, and garbage after the list does not hide an earlier
/// match.
/// Catches a reader that needs the whole list to be well formed.
#[test]
fn trailing_garbage_after_a_match_does_not_hide_it() {
    let odd = entry_with(&[]);
    assert_eq!(utf8_extension(&certificate_of(&odd), &ref_oid()), None);

    let mut list = good_entry("refs/tags/v4");
    list.extend([0x30, 0x09, 0x01]);
    assert_eq!(
        utf8_extension(&certificate_of(&list), &ref_oid()),
        Some("refs/tags/v4")
    );
}
