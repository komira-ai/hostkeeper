//! Reading one member of a pinned archive.

mod common;

use common::{Tweak, tar_gz_of, zip_of, zip_raw};
use hostkeeper_mdm::archive::{ArchiveError, member, member_with_limit};
use hostkeeper_mdm_core::pins::Packing;

fn zip_err(bytes: &[u8]) -> String {
    match member("a.zip", Packing::Zip, bytes, "bin/tool") {
        Err(ArchiveError::Unreadable { why, .. }) => why,
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

/// Catches a reader that returns the wrong member's bytes, or none, from a zip it should
/// read: a deflated member and a stored one, among others.
#[test]
fn a_zip_member_is_found_by_exact_name_stored_or_deflated() {
    let members: [(&str, &[u8]); 3] = [
        ("other", b"zzz"),
        ("bin/tool", b"tool bytes"),
        ("bin/tool2", b"x"),
    ];
    for deflate in [true, false] {
        let zip = zip_raw(&members, deflate, &Tweak::default());
        let got = member("a.zip", Packing::Zip, &zip, "bin/tool").unwrap();
        assert_eq!(got, b"tool bytes");
    }
    let big = vec![7u8; 100_000];
    let zip = zip_of(&[("big", &big)]);
    assert_eq!(member("a.zip", Packing::Zip, &zip, "big").unwrap(), big);
}

/// Catches a missing or empty member reported as success: both are `NoMember`, whose text
/// names the archive and the member.
#[test]
fn a_missing_or_empty_member_is_refused() {
    let zip = zip_of(&[("empty", b""), ("x", b"1")]);
    for name in ["empty", "absent"] {
        let err = member("a.zip", Packing::Zip, &zip, name).unwrap_err();
        assert!(matches!(err, ArchiveError::NoMember { .. }));
        assert_eq!(
            err.to_string(),
            format!("a.zip has no {name}, or it is empty")
        );
    }
}

/// Catches a reader that trusts a damaged zip: each way the structure can be wrong is
/// refused with a reason and never panics or reads outside the bytes.
#[test]
fn a_damaged_zip_is_unreadable() {
    let m: [(&str, &[u8]); 1] = [("bin/tool", b"tool bytes")];
    assert_eq!(zip_err(&[1, 2, 3]), "malformed zip");
    let t = |t: Tweak| zip_err(&zip_raw(&m, true, &t));
    assert_eq!(
        t(Tweak {
            central_sig: Some(1),
            ..Tweak::default()
        }),
        "malformed zip"
    );
    assert_eq!(
        t(Tweak {
            local_sig: Some(1),
            ..Tweak::default()
        }),
        "malformed zip"
    );
    assert_eq!(
        t(Tweak {
            name_len: Some(60_000),
            ..Tweak::default()
        }),
        "malformed zip"
    );
    assert_eq!(
        t(Tweak {
            packed: Some(1_000_000),
            ..Tweak::default()
        }),
        "malformed zip"
    );
    assert_eq!(
        t(Tweak {
            method: Some(99),
            ..Tweak::default()
        }),
        "compression method 99 is not supported"
    );
    let why = t(Tweak {
        plain: Some(3),
        ..Tweak::default()
    });
    assert!(why.starts_with("member is "), "{why}");
    // A deflate stream that is not one.
    let junk = zip_raw(
        &m,
        false,
        &Tweak {
            method: Some(8),
            ..Tweak::default()
        },
    );
    assert!(!zip_err(&junk).is_empty());
    let stored_lie = zip_raw(
        &m,
        false,
        &Tweak {
            plain: Some(99),
            ..Tweak::default()
        },
    );
    assert!(zip_err(&stored_lie).starts_with("member is 10 bytes"));
}

/// Catches a tar reader that matches on a suffix or returns an entry it should skip, and
/// one that treats a non-gzip file as success.
#[test]
fn a_tar_gz_member_is_found_by_exact_path() {
    let tgz = tar_gz_of(&[("lego", b"lego bytes"), ("sub/lego", b"nested")]);
    assert_eq!(
        member("l.tar.gz", Packing::TarGz, &tgz, "lego").unwrap(),
        b"lego bytes"
    );
    assert_eq!(
        member("l.tar.gz", Packing::TarGz, &tgz, "sub/lego").unwrap(),
        b"nested"
    );
    let err = member("l.tar.gz", Packing::TarGz, &tgz, "go").unwrap_err();
    assert!(matches!(err, ArchiveError::NoMember { .. }));
    let err = member("l.tar.gz", Packing::TarGz, b"not gzip at all", "lego").unwrap_err();
    assert!(matches!(err, ArchiveError::Unreadable { .. }), "{err}");
    assert!(
        err.to_string()
            .starts_with("l.tar.gz is not a readable TarGz archive: ")
    );
}

/// Catches a member cut off at the size limit and returned as if whole: a member longer than
/// the limit is an error in a zip (stored or deflated) and in a tar.gz, and one exactly the
/// limit is read in full.
#[test]
fn a_member_above_the_limit_is_an_error_not_a_short_read() {
    let body = [9u8; 10];
    let zips = [
        zip_raw(&[("m", &body[..])], true, &Tweak::default()),
        zip_raw(&[("m", &body[..])], false, &Tweak::default()),
    ];
    for zip in &zips {
        let err = member_with_limit("a.zip", Packing::Zip, zip, "m", 5).unwrap_err();
        assert!(matches!(err, ArchiveError::Unreadable { .. }), "{err}");
        assert_eq!(
            member_with_limit("a.zip", Packing::Zip, zip, "m", 10).unwrap(),
            body
        );
    }
    let tgz = tar_gz_of(&[("m", &body[..])]);
    let err = member_with_limit("a.tar.gz", Packing::TarGz, &tgz, "m", 5).unwrap_err();
    assert!(
        err.to_string().contains("member is larger than 5 bytes"),
        "{err}"
    );
    assert_eq!(
        member_with_limit("a.tar.gz", Packing::TarGz, &tgz, "m", 10).unwrap(),
        body
    );
}

fn zip_why(bytes: &[u8], limit: u64) -> String {
    match member_with_limit("a.zip", Packing::Zip, bytes, "m", limit) {
        Err(ArchiveError::Unreadable { why, .. }) => why,
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

/// Catches a zip member whose listed size is above the limit but whose real output is
/// exactly the limit being accepted: the listed size decides, and the error names both the
/// listed size and the limit. Stored and deflated.
#[test]
fn a_zip_member_listed_larger_than_the_limit_is_refused_even_if_its_output_fits() {
    let body = [4u8; 5];
    for deflate in [true, false] {
        let zip = zip_raw(
            &[("m", &body[..])],
            deflate,
            &Tweak {
                plain: Some(10),
                ..Tweak::default()
            },
        );
        assert_eq!(
            zip_why(&zip, 5),
            "member is listed as 10 bytes, larger than the 5 byte limit"
        );
    }
}

/// Catches a size check that is off by one: a member of limit-1 and of exactly the limit is
/// read in full and one of limit+1 is refused, on a stored zip, a deflated zip and a tar.gz.
#[test]
fn a_member_of_exactly_the_limit_is_read_and_one_byte_more_is_refused() {
    let body = [3u8; 8];
    let tgz = tar_gz_of(&[("m", &body[..])]);
    let zips = [true, false].map(|d| zip_raw(&[("m", &body[..])], d, &Tweak::default()));
    for limit in [7u64, 8, 9] {
        let want_ok = limit >= 8;
        for zip in &zips {
            let got = member_with_limit("a.zip", Packing::Zip, zip, "m", limit);
            assert_eq!(got.is_ok(), want_ok, "zip, limit {limit}");
        }
        let got = member_with_limit("a.tar.gz", Packing::TarGz, &tgz, "m", limit);
        assert_eq!(got.is_ok(), want_ok, "tar, limit {limit}");
    }
    assert_eq!(
        zip_why(&zips[0], 7),
        "member is listed as 8 bytes, larger than the 7 byte limit"
    );
}

/// Catches `limit + 1` overflowing when the limit is `u64::MAX`: both packings read a small
/// member in full, and a missing one is still absent.
#[test]
fn a_limit_of_u64_max_reads_a_member_on_both_packings() {
    let body = b"some bytes";
    let tgz = tar_gz_of(&[("m", &body[..])]);
    assert_eq!(
        member_with_limit("a.tar.gz", Packing::TarGz, &tgz, "m", u64::MAX).unwrap(),
        body
    );
    for deflate in [true, false] {
        let zip = zip_raw(&[("m", &body[..])], deflate, &Tweak::default());
        assert_eq!(
            member_with_limit("a.zip", Packing::Zip, &zip, "m", u64::MAX).unwrap(),
            body
        );
    }
    let err = member_with_limit("a.tar.gz", Packing::TarGz, &tgz, "x", u64::MAX).unwrap_err();
    assert!(matches!(err, ArchiveError::NoMember { .. }));
}

/// Catches a zip member whose inflated output differs from its listed size: shorter or
/// longer is refused with both numbers, and a stored member whose packed size differs from
/// its listed size is refused too.
#[test]
fn a_zip_member_must_come_out_exactly_the_listed_size() {
    let body = [6u8; 10];
    let m = [("m", &body[..])];
    let tweak = |plain| Tweak {
        plain: Some(plain),
        ..Tweak::default()
    };
    let deflated = |plain| zip_raw(&m, true, &tweak(plain));
    let stored = |plain| zip_raw(&m, false, &tweak(plain));
    assert_eq!(
        zip_why(&deflated(11), 100),
        "member is 10 bytes, not the 11 the zip lists"
    );
    assert_eq!(
        zip_why(&deflated(9), 100),
        "member is more than the 9 bytes the zip lists"
    );
    assert_eq!(
        zip_why(&stored(11), 100),
        "member is 10 bytes, not the 11 the zip lists"
    );
    assert_eq!(
        zip_why(&stored(9), 100),
        "member is 10 bytes, not the 9 the zip lists"
    );
    assert_eq!(
        member_with_limit("a.zip", Packing::Zip, &stored(10), "m", 100).unwrap(),
        body
    );
}

fn put(bytes: &mut [u8], at: usize, v: u32, n: usize) {
    bytes[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
}

/// Catches arithmetic on offsets and lengths read from the archive that overflows or
/// indexes outside it: each field of the central directory and end record set to values
/// near `u32::MAX` (or `u16::MAX`) is refused, never a panic.
#[test]
fn hostile_zip_offsets_near_the_integer_limits_are_refused_not_panicked_on() {
    let zip = zip_raw(
        &[("m", b"tool bytes"), ("n", b"more")],
        true,
        &Tweak::default(),
    );
    let end = zip.len() - 22;
    let cd = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
    // The second central header follows the first, which has a one byte name.
    let second = cd + 46 + 1;
    // (header, offset of the field, its width): "n" is asked for, so the fields of its own
    // header decide where its data is, and the fields of the first header decide where the
    // second header is.
    let fields = [
        (second, 42usize, 4usize),
        (second, 20, 4),
        (second, 28, 2),
        (cd, 28, 2),
        (cd, 30, 2),
        (cd, 32, 2),
    ];
    for (header, field, width) in fields {
        for value in [u32::MAX, u32::MAX - 1, u32::MAX - 45, i32::MAX as u32] {
            let mut z = zip.clone();
            put(&mut z, header + field, value, width);
            let r = member_with_limit("a.zip", Packing::Zip, &z, "n", u64::MAX);
            assert!(r.is_err(), "field {field} = {value:#x}: {r:?}");
        }
    }
    // The local header's name and extra lengths.
    for field in [26usize, 28] {
        let mut z = zip.clone();
        put(&mut z, field, 0xffff, 2);
        assert!(member_with_limit("a.zip", Packing::Zip, &z, "m", u64::MAX).is_err());
    }
    // The end record: the directory's offset and entry count.
    for value in [u32::MAX, u32::MAX - 21] {
        let mut z = zip.clone();
        put(&mut z, end + 16, value, 4);
        assert_eq!(
            zip_why(&z, u64::MAX),
            "malformed zip",
            "directory at {value:#x}"
        );
    }
    let mut z = zip.clone();
    put(&mut z, end + 10, 0xffff, 2);
    let r = member_with_limit("a.zip", Packing::Zip, &z, "absent", u64::MAX);
    assert!(matches!(r, Err(ArchiveError::Unreadable { .. })), "{r:?}");
}
