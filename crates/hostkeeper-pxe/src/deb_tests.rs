use super::*;
use crate::testing::deb::{Kind, deb, member};
use crate::testing::{BrokenReader, Failing, sha256_hex};

const SHIM: &str = "usr/lib/shim/shimx64.efi.signed.latest";

/// The file `path` of `deb`, at most `cap` bytes.
fn take(deb: &[u8], path: &str, cap: u64) -> Result<(Measured, Vec<u8>), DebError> {
    let mut out = Vec::new();
    extract(&mut &deb[..], path, &mut out, cap).map(|measured| (measured, out))
}

fn malformed(result: Result<(Measured, Vec<u8>), DebError>) -> String {
    match result {
        Err(DebError::Malformed(why)) => why,
        other => panic!("not malformed: {other:?}"),
    }
}

/// The file is taken out of a `data.tar` of each compression a deb may carry (zstd, as
/// `grub2-signed`'s; xz, as `shim-signed`'s; gzip; none), past other entries, byte for byte, its
/// size and SHA-256 measured.
///
/// Catches a reader that knows only one compression, so Ubuntu's shim or GRUB could not be
/// taken out, and one that copies the wrong entry.
#[test]
fn the_file_is_taken_out_of_each_compression() {
    let shim = b"the shim, signed";
    for compression in ["data.tar.zst", "data.tar.xz", "data.tar.gz", "data.tar"] {
        let deb = deb(
            compression,
            &[
                ("usr/lib/shim/shimx64.efi", b"unsigned", Kind::File),
                (SHIM, shim, Kind::File),
                ("usr/lib/shim/zzz", b"after", Kind::File),
            ],
        );
        let (measured, out) = take(&deb, SHIM, 1000).expect(compression);
        assert_eq!(out, shim, "{compression}");
        assert_eq!(measured.sha256, sha256_hex(shim), "{compression}");
        assert_eq!(measured.bytes, shim.len() as u64, "{compression}");
    }
}

/// A path matched only as a whole: an entry that is a prefix of it, or has it as a prefix, is
/// not it, and a path in no entry is not found; an entry that is a link is not a regular file.
///
/// Catches `shimx64.efi` served for `shimx64.efi.signed.latest`, and a link's target served as
/// the file.
#[test]
fn only_a_regular_entry_of_the_whole_path_is_taken() {
    let deb = deb(
        "data.tar.xz",
        &[
            ("usr/lib/shim/shimx64.efi", b"short", Kind::File),
            (
                "usr/lib/shim/shimx64.efi.signed.latest.bak",
                b"long",
                Kind::File,
            ),
            ("usr/lib/shim/link", b"", Kind::Symlink),
        ],
    );
    assert!(matches!(take(&deb, SHIM, 100), Err(DebError::NotFound)));
    assert!(matches!(
        take(&deb, "usr/lib/shim/link", 100),
        Err(DebError::NotAFile)
    ));
    assert_eq!(DebError::NotFound.to_string(), "not found");
    assert_eq!(DebError::NotAFile.to_string(), "not a regular file");
}

/// A file over its cap is refused once one byte more was read.
///
/// Catches an unbounded copy out of a deb.
#[test]
fn a_file_over_its_cap_is_refused() {
    let deb = deb("data.tar.zst", &[(SHIM, &[1; 100], Kind::File)]);
    let refused = take(&deb, SHIM, 99).expect_err("over the cap");
    assert!(
        matches!(refused, DebError::Copy(CopyError::TooLarge(99))),
        "{refused:?}"
    );
    assert_eq!(refused.to_string(), "more than 99 bytes");
}

/// What is not a deb of this form is refused naming what is wrong: no `ar` magic, a short file,
/// a member header that is not one, a member cut short, no `data.tar` member, a `data.tar` of a
/// compression the reader does not decode, and a `data.tar` whose bytes are not of its
/// compression.
///
/// Catches a reader that guesses, panics or reads garbage as a boot file.
#[test]
fn a_deb_of_another_form_is_refused() {
    assert_eq!(malformed(take(b"!<arch>", SHIM, 9)), "not an ar archive");
    assert_eq!(malformed(take(b"!<arch>X", SHIM, 9)), "not an ar archive");
    assert_eq!(malformed(take(b"!<arch>\n", SHIM, 9)), "no data.tar member");
    let mut bad_header = b"!<arch>\n".to_vec();
    let mut header = member("debian-binary", b"2.0\n");
    header[58] = b'X';
    bad_header.extend(header);
    assert_eq!(
        malformed(take(&bad_header, SHIM, 9)),
        "the header of the member \"debian-binary\" is not an ar header"
    );
    let mut cut = b"!<arch>\n".to_vec();
    cut.extend(&member("control.tar.xz", b"0123456789")[..65]);
    assert_eq!(
        malformed(take(&cut, SHIM, 9)),
        "the member control.tar.xz is cut short"
    );
    let mut cut_pad = b"!<arch>\n".to_vec();
    let odd = member("control.tar.xz", b"odd");
    cut_pad.extend(&odd[..odd.len() - 1]);
    assert_eq!(
        malformed(take(&cut_pad, SHIM, 9)),
        "the member control.tar.xz is cut short"
    );
    let mut no_data = b"!<arch>\n".to_vec();
    no_data.extend(member("debian-binary", b"2.0\n"));
    assert_eq!(malformed(take(&no_data, SHIM, 9)), "no data.tar member");
    let bz2 = deb("data.tar.bz2", &[(SHIM, b"x", Kind::File)]);
    assert_eq!(
        malformed(take(&bz2, SHIM, 9)),
        "data.tar.bz2 is not a data.tar this reader decodes (zst, xz, gz or none)"
    );
    for name in ["data.tar.xz", "data.tar.zst"] {
        let mut garbage = b"!<arch>\n".to_vec();
        garbage.extend(member(name, b"these bytes are of no compression"));
        let why = malformed(take(&garbage, SHIM, 9));
        assert!(why.starts_with(&format!("{name}: ")), "{why}");
    }
}

/// A tar that cannot be read past its start, a gzip body that is not gzip, and a read of the
/// deb that fails are read errors; a write that fails is a write error.
///
/// Catches a broken deb or a full disk reported as a missing file.
#[test]
fn read_and_write_failures_while_copying_are_told_apart() {
    let mut garbage = b"!<arch>\n".to_vec();
    garbage.extend(member("data.tar.gz", b"not gzip at all"));
    let refused = take(&garbage, SHIM, 9).expect_err("not gzip");
    assert!(
        matches!(refused, DebError::Copy(CopyError::Read(_))),
        "{refused:?}"
    );

    let mut bad_tar = b"!<arch>\n".to_vec();
    bad_tar.extend(member("data.tar", &[b'x'; 512]));
    let refused = take(&bad_tar, SHIM, 9).expect_err("not a tar");
    assert!(
        matches!(refused, DebError::Copy(CopyError::Read(_))),
        "{refused:?}"
    );

    let whole = deb("data.tar", &[(SHIM, b"x", Kind::File)]);
    let mut broken = BrokenReader::after(&whole[..70], "the image could not be read");
    let refused = extract(&mut broken, SHIM, &mut Vec::new(), 9).expect_err("a failed read");
    assert!(
        matches!(refused, DebError::Copy(CopyError::Read(_))),
        "{refused:?}"
    );

    let deb = deb("data.tar.zst", &[(SHIM, b"the shim", Kind::File)]);
    let refused = extract(
        &mut &deb[..],
        SHIM,
        &mut Failing::writes(io::ErrorKind::StorageFull),
        100,
    )
    .expect_err("a full disk");
    assert!(
        matches!(refused, DebError::Copy(CopyError::Write(_))),
        "{refused:?}"
    );
}

/// An xz `data.tar` that decodes to more than its bound is refused while it is decoded.
///
/// Catches a deb whose `data.tar.xz` fills memory.
#[test]
fn an_xz_data_tar_is_decoded_within_its_bound() {
    let mut bounded = Bounded {
        bytes: Vec::new(),
        cap: 4,
    };
    assert_eq!(bounded.write(b"abcd").expect("within"), 4);
    let refused = bounded.write(b"e").expect_err("past the bound");
    assert_eq!(refused.to_string(), "decodes to more than 4 bytes");
    bounded.flush().expect("a flush");
    assert_eq!(bounded.bytes, b"abcd");
}
