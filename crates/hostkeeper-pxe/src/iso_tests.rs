use std::io::Read;

use super::*;
use crate::testing::iso::{Image, PVD, continuation, entry, record};
use crate::testing::scratch;

/// `image` written to a scratch file and opened.
fn open(name: &str, image: &[u8]) -> Result<(Iso, hostkeeper_testkit::Scratch), IsoError> {
    let scratch = scratch(name);
    let path = scratch.path().join("image.iso");
    std::fs::write(&path, image).expect("write the image");
    let file = File::open(&path).expect("open the image");
    Iso::open(file).map(|iso| (iso, scratch))
}

/// The bytes of the file at `path` of `iso`.
fn read(iso: &Iso, path: &str) -> Result<Vec<u8>, IsoError> {
    let extent = iso.find(path)?;
    let mut bytes = Vec::new();
    iso.reader(extent)?
        .read_to_end(&mut bytes)
        .map_err(IsoError::Io)?;
    Ok(bytes)
}

fn malformed_text<T>(result: Result<T, IsoError>) -> String {
    match result {
        Err(IsoError::Malformed(why)) => why,
        Err(other) => panic!("not malformed: {other:?}"),
        Ok(_) => panic!("not refused"),
    }
}

/// The files of an image, at any depth and with Rock Ridge names longer than ISO 9660 allows
/// (`+`, `~`, lowercase, several dots), read back byte for byte by their paths; a file of zero
/// bytes and one of several sectors too.
///
/// Catches a reader that matches the ISO 9660 name instead of the Rock Ridge one, reads the
/// wrong extent, or reads past a file's size.
#[test]
fn files_are_found_by_their_rock_ridge_paths() {
    let big: Vec<u8> = (0..10_000_u32).map(|i| (i % 251) as u8).collect();
    let image = Image::of(&[
        ("casper/vmlinuz", b"the kernel"),
        ("casper/initrd", &big),
        (
            "pool/main/s/shim-signed/shim-signed_1.58+15.8-0ubuntu1_amd64.deb",
            b"a deb",
        ),
        ("pool/main/g/grub2-signed/grub~1.deb", b"another"),
        ("empty", b""),
    ]);
    let (iso, _scratch) = open("rock-ridge", &image.build()).expect("an image");
    assert_eq!(read(&iso, "casper/vmlinuz").expect("found"), b"the kernel");
    assert_eq!(read(&iso, "casper/initrd").expect("found"), big);
    assert_eq!(
        read(
            &iso,
            "pool/main/s/shim-signed/shim-signed_1.58+15.8-0ubuntu1_amd64.deb"
        )
        .expect("found"),
        b"a deb"
    );
    assert_eq!(
        read(&iso, "pool/main/g/grub2-signed/grub~1.deb").expect("found"),
        b"another"
    );
    assert_eq!(read(&iso, "empty").expect("found"), b"");
}

/// A name split over two `NM` entries, a name in a continuation area (after a `PX` entry, with an
/// `ST` entry after the `CE`), a boot record before the primary descriptor, and a directory of
/// more records than fit one sector are all read.
///
/// Catches a reader that takes only the first `NM` entry, ignores `CE` (the root of Ubuntu's ISO
/// carries one), stops at the first descriptor, or stops at the end of a directory's first
/// sector.
#[test]
fn split_continued_and_multi_sector_names_are_read() {
    let many: Vec<(String, Vec<u8>)> = (0..60)
        .map(|i| {
            (
                format!("pool/a-rather-long-file-name-number-{i:03}.deb"),
                vec![i as u8; 3],
            )
        })
        .collect();
    for (split, continued) in [(true, false), (false, true), (true, true)] {
        let mut image = Image::of(&[("casper/vmlinuz", b"kernel")]);
        image.split_names = split;
        image.continued_names = continued;
        image.boot_record_first = true;
        if !continued {
            image.files.extend(many.iter().cloned());
        }
        let (iso, _scratch) = open("split-names", &image.build()).expect("an image");
        assert_eq!(
            read(&iso, "casper/vmlinuz").expect("found"),
            b"kernel",
            "{split} {continued}"
        );
        if !continued {
            assert_eq!(
                read(&iso, "pool/a-rather-long-file-name-number-059.deb").expect("found"),
                vec![59; 3]
            );
        }
    }
}

/// A path that is not there, one that goes on below a file, and one that names a directory are
/// told apart; a name is matched whole, not as a prefix.
///
/// Catches a lookup that takes `casper/vmlinuz.efi` for `casper/vmlinuz`, or a directory's
/// records for a file's bytes.
#[test]
fn a_missing_path_and_a_directory_are_told_apart() {
    let image = Image::of(&[("casper/vmlinuz.efi", b"k"), ("casper/initrd", b"i")]);
    let (iso, _scratch) = open("missing", &image.build()).expect("an image");
    assert!(matches!(
        iso.find("casper/vmlinuz"),
        Err(IsoError::NotFound)
    ));
    assert!(matches!(iso.find("nothing/here"), Err(IsoError::NotFound)));
    assert!(matches!(
        iso.find("casper/initrd/below"),
        Err(IsoError::NotFound)
    ));
    assert!(matches!(iso.find("casper"), Err(IsoError::NotAFile)));
    assert_eq!(IsoError::NotFound.to_string(), "not found");
    assert_eq!(IsoError::NotAFile.to_string(), "a directory, not a file");
}

/// An image without Rock Ridge names, a file recorded in more than one extent, and a sector 16
/// that is not a volume descriptor, a terminator before any primary descriptor, a block size
/// other than 2048 and a root record of the wrong length are refused, each naming what is wrong.
///
/// Catches a reader that guesses names from ISO 9660's 8.3 ones, or serves the first extent of a
/// split file as the whole.
#[test]
fn an_image_of_another_form_is_refused() {
    let mut image = Image::of(&[("casper/vmlinuz", b"k")]);
    image.no_rock_ridge = true;
    assert_eq!(
        malformed_text(open("no-rr", &image.build())),
        "the image has no Rock Ridge names"
    );

    let mut image = Image::of(&[("casper/vmlinuz", b"k")]);
    image.multi_extent = vec!["casper/vmlinuz".to_owned()];
    let (iso, _scratch) = open("multi", &image.build()).expect("an image");
    assert_eq!(
        malformed_text(iso.find("casper/vmlinuz")),
        "the file is recorded in more than one extent"
    );

    let good = Image::of(&[("casper/vmlinuz", b"k")]).build();
    let mut not_cd = good.clone();
    not_cd[PVD + 1] = b'X';
    assert_eq!(
        malformed_text(open("not-cd", &not_cd)),
        "sector 16 is not a volume descriptor"
    );
    let mut terminated = good.clone();
    terminated[PVD] = 255;
    assert_eq!(
        malformed_text(open("terminated", &terminated)),
        "no primary volume descriptor"
    );
    let mut block = good.clone();
    block[PVD + 129] = 0;
    assert_eq!(
        malformed_text(open("block-size", &block)),
        "the logical block size is not 2048"
    );
    let mut root = good.clone();
    root[PVD + 156] = 40;
    assert_eq!(
        malformed_text(open("root-length", &root)),
        "a directory record is shorter than its name"
    );
    assert_eq!(
        malformed_text(open("empty", b"")),
        "a read runs past the end of the image"
    );
}

/// Lengths and offsets that point outside what holds them are refused, never followed: a file
/// extent past the end of the image, a directory larger than 16 MiB, a record longer than the
/// rest of its directory, a system use entry longer than its area or shorter than its head,
/// a continuation area past the end of the image, and a chain of more than 16 continuations.
///
/// Catches a wrong pin that makes the reader panic, read past the image, or loop.
#[test]
fn lengths_and_offsets_outside_the_image_are_refused() {
    let base = Image::of(&[("f", b"x")]).build();
    let (iso, _scratch) = open("base", &base).expect("an image");
    let root = iso.root.clone();
    let records = iso.records(&root).expect("the root's records");
    let file = records.last().expect("the file f").clone();

    let past = Record {
        extent: Extent {
            offset: file.extent.offset,
            bytes: 1 << 30,
        },
        ..file.clone()
    };
    assert_eq!(
        malformed_text(iso.within(past.extent)),
        "an extent runs past the end of the image"
    );
    let huge = Record {
        extent: Extent {
            offset: 0,
            bytes: MAX_DIRECTORY_BYTES + 1,
        },
        ..root.clone()
    };
    assert_eq!(
        malformed_text(iso.records(&huge)),
        "a directory is larger than 16 MiB"
    );

    // A directory whose one record claims more bytes than the directory holds: the record is
    // in sector 0, which the reader never reads otherwise.
    let mut cut = record(0, 0, 0, b"A", &[]);
    cut[0] = 200;
    let mut image = base.clone();
    image[..cut.len()].copy_from_slice(&cut);
    let dir = Record {
        extent: Extent {
            offset: 0,
            bytes: 100,
        },
        ..root.clone()
    };
    let (cut_iso, _cut) = open("cut", &image).expect("an image");
    assert_eq!(
        malformed_text(cut_iso.records(&dir)),
        "a directory record runs past its directory"
    );

    let entry_of = |system_use: Vec<u8>| Record {
        system_use,
        ..file.clone()
    };
    let mut long = entry(b"NM", b"\0name");
    long[2] = 60;
    assert_eq!(
        malformed_text(iso.name(&entry_of(long))),
        "a system use entry runs past its area"
    );
    let mut short = entry(b"NM", b"\0name");
    short[2] = 2;
    assert_eq!(
        malformed_text(iso.name(&entry_of(short))),
        "a system use entry runs past its area"
    );
    assert_eq!(
        malformed_text(iso.name(&entry_of(continuation(1 << 20, 0, 10)))),
        "an extent runs past the end of the image"
    );
    // A continuation area that points to itself: sector 0 of the image holds a CE entry to
    // sector 0.
    let mut looping = base.clone();
    let ce = continuation(0, 0, 28);
    looping[..ce.len()].copy_from_slice(&ce);
    let (loop_iso, _loop) = open("loop", &looping).expect("an image");
    assert_eq!(
        malformed_text(loop_iso.name(&entry_of(ce.clone()))),
        "more than 16 continuation areas"
    );
    let truncated_ce = ce[..20].to_vec();
    let mut short_ce = truncated_ce.clone();
    short_ce[2] = 20;
    assert_eq!(
        malformed_text(iso.name(&entry_of(short_ce))),
        "a field runs past its record"
    );
}

/// A failed read of the image is its I/O error.
///
/// Catches an I/O failure reported as a malformed image.
#[test]
fn a_failed_read_is_an_io_error() {
    let scratch = scratch("io");
    let path = scratch.path().join("dir");
    std::fs::create_dir(&path).expect("a directory");
    let directory = File::open(&path).expect("open the directory");
    let error = read_at(&directory, 4096, 0, 16).expect_err("a directory cannot be read");
    assert!(matches!(error, IsoError::Io(_)), "{error:?}");
    assert!(!error.to_string().is_empty());
}
