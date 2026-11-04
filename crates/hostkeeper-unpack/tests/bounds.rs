//! The bounds and shape rules of the archive (design section 5.5, rows 3 and 4 of the bounds
//! table): each violation is refused as `bad_archive` and leaves nothing; each boundary passes.

mod support;

use std::path::{Path, PathBuf};

use hostkeeper_hook::{Class, Outcome};
use hostkeeper_unpack::{BUFFER, Limits, MAX_WINDOW_BYTES, Rule, UnpackError, unpack};
use support::{
    Call, Faulty, Head, MANIFEST, Rig, Tar, VERSION, WINDOW_1M, block_header, frame_header, limits,
    rle_blocks, valid_tar, zstd, zstd_with,
};

/// The manifest and `bin/`: what most rows add their entry to.
fn base() -> Tar {
    let mut tar = Tar::new();
    tar.file("manifest.toml", 0o644, MANIFEST.as_bytes())
        .dir("bin")
        .dir("files")
        .file("files/c.conf", 0o644, b"c\n");
    tar
}

fn with(add: impl Fn(&mut Tar)) -> Vec<u8> {
    let mut tar = base();
    add(&mut tar);
    zstd(&tar.end().bytes())
}

fn head(name: &[u8], flag: u8, mode: u64, size: u64) -> Head {
    Head::new(name, flag, mode, size)
}

/// The manifest padded with a comment to exactly `len` bytes.
fn manifest_of(len: usize) -> Vec<u8> {
    let mut text = MANIFEST.as_bytes().to_vec();
    text.extend_from_slice(b"#");
    text.resize(len - 1, b'x');
    text.push(b'\n');
    text
}

/// `bin/` and the directories under it, down to `depth` components for a file.
fn deep(tar: &mut Tar, depth: usize) {
    let mut name = "bin".to_owned();
    for _ in 2..depth {
        name.push_str("/d");
        tar.dir(&name);
    }
    tar.file(&format!("{name}/x"), 0o644, b"x");
}

fn long(len: usize) -> String {
    format!("bin/{}", "a".repeat(len - 4))
}

/// A row: its name, the archive (compressed), the bounds, and the rule it breaks.
struct Row {
    name: &'static str,
    archive: Vec<u8>,
    limits: Limits,
    rule: Rule,
}

fn row(name: &'static str, archive: Vec<u8>, rule: Rule) -> Row {
    Row {
        name,
        archive,
        limits: limits(),
        rule,
    }
}

fn rows() -> Vec<Row> {
    let valid = valid_tar().end().bytes();
    let mut over = row(
        "one byte over max_unpacked_bytes",
        zstd(&valid),
        Rule::TooLarge,
    );
    over.limits.max_unpacked_bytes = valid.len() as u64 - 1;
    let mut entries = row("max_entries plus one", zstd(&valid), Rule::TooManyEntries);
    entries.limits.max_entries = 6;
    let mut bomb = row(
        "a zip-bomb frame: zero blocks after the end",
        zstd_with(&valid, WINDOW_1M, &rle_blocks(0, 8192)),
        Rule::TooLarge,
    );
    bomb.limits.max_unpacked_bytes = 1 << 20;
    let mut huge = row(
        "a zip-bomb frame: a huge file of one byte repeated",
        zstd_with(
            &[
                base().bytes(),
                Tar::header_block(&head(b"bin/big", b'0', 0o644, 1 << 30)).to_vec(),
            ]
            .concat(),
            WINDOW_1M,
            &rle_blocks(b'a', 8192),
        ),
        Rule::TooLarge,
    );
    huge.limits.max_unpacked_bytes = 1 << 20;
    let mut wide = frame_header(14 << 3);
    wide.extend_from_slice(&block_header(true, 0, 0));
    let tar = valid_tar().end().bytes();
    let framed = zstd(&tar);
    let mut second = framed.clone();
    second.extend_from_slice(&zstd(b""));
    let mut ragged = valid_tar().end().bytes();
    ragged.extend_from_slice(&[0u8; 100]);
    let mut nonzero_end = valid_tar().bytes();
    nonzero_end.extend_from_slice(&[0u8; 512]);
    nonzero_end.extend_from_slice(&Tar::header_block(&head(b"bin/late", b'0', 0o644, 0)));
    let mut bad_sum = Tar::header_block(&head(b"bin/x", b'0', 0o644, 0));
    bad_sum[0] = b'c';
    let mut no_magic = Tar::header_block(&head(b"bin/x", b'0', 0o644, 0));
    no_magic[257..265].copy_from_slice(&[0u8; 8]);
    let mut base256 = head(b"bin/x", b'0', 0o644, 0);
    base256.mode = 0;
    let mut base256 = Tar::header_block(&base256);
    base256[124] = 0x80;
    let fix = |block: &mut [u8; 512]| {
        block[148..156].copy_from_slice(b"        ");
        let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
        let mut field = format!("{sum:06o}").into_bytes();
        field.extend_from_slice(b"\0 ");
        block[148..156].copy_from_slice(&field);
    };
    fix(&mut no_magic);
    fix(&mut base256);
    vec![
        over,
        bomb,
        huge,
        entries,
        row(
            "a 256-byte name",
            with(|t| {
                t.pax(&[("path", long(256).as_bytes())])
                    .file("x", 0o644, b"x");
            }),
            Rule::NameTooLong,
        ),
        row(
            "a 256-byte name in the ustar prefix",
            with(|t| {
                t.entry(
                    &head(
                        format!("bin/{}/{}", "p".repeat(151), "n".repeat(100)).as_bytes(),
                        b'0',
                        0o644,
                        0,
                    ),
                    b"",
                );
            }),
            Rule::NameTooLong,
        ),
        row(
            "a GNU long name of 256 bytes",
            with(|t| {
                t.long_name(long(256).as_bytes()).file("x", 0o644, b"x");
            }),
            Rule::NameTooLong,
        ),
        row("depth 17", with(|t| deep(t, 17)), Rule::TooDeep),
        row(
            "a manifest.toml of 64 KiB plus one",
            {
                let mut t = Tar::new();
                t.file("manifest.toml", 0o644, &manifest_of(65_537));
                zstd(&t.end().bytes())
            },
            Rule::ManifestTooLarge,
        ),
        row(
            "a top-level other/",
            with(|t| {
                t.dir("other");
            }),
            Rule::TopLevel,
        ),
        row(
            "a top-level file bin",
            {
                let mut t = Tar::new();
                t.file("manifest.toml", 0o644, MANIFEST.as_bytes())
                    .file("bin", 0o755, b"x");
                zstd(&t.end().bytes())
            },
            Rule::TopLevel,
        ),
        row(
            "a top-level directory manifest.toml",
            {
                let mut t = Tar::new();
                t.dir("manifest.toml");
                zstd(&t.end().bytes())
            },
            Rule::TopLevel,
        ),
        row(
            "a top-level Bin/ beside bin/",
            with(|t| {
                t.dir("Bin");
            }),
            Rule::TopLevel,
        ),
        row(
            "an entry under manifest.toml",
            with(|t| {
                t.file("manifest.toml/x", 0o644, b"x");
            }),
            Rule::TopLevel,
        ),
        row(
            "a symlink",
            with(|t| {
                t.entry(&head(b"bin/l", b'2', 0o777, 0), b"");
            }),
            Rule::Kind(b'2'),
        ),
        row(
            "a hard link",
            with(|t| {
                t.entry(&head(b"bin/h", b'1', 0o644, 0), b"");
            }),
            Rule::Kind(b'1'),
        ),
        row(
            "a character device",
            with(|t| {
                t.entry(&head(b"bin/c", b'3', 0o644, 0), b"");
            }),
            Rule::Kind(b'3'),
        ),
        row(
            "a fifo",
            with(|t| {
                t.entry(&head(b"bin/f", b'6', 0o644, 0), b"");
            }),
            Rule::Kind(b'6'),
        ),
        row(
            "a pax global header",
            with(|t| {
                t.entry(&head(b"pax_global_header", b'g', 0o644, 0), b"");
            }),
            Rule::Kind(b'g'),
        ),
        row(
            "an absolute path",
            with(|t| {
                t.file("/bin/x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a .. path",
            with(|t| {
                t.file("../x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a/../b",
            with(|t| {
                t.file("bin/../bin/x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a . component",
            with(|t| {
                t.file("bin/./x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "an empty component",
            with(|t| {
                t.file("bin//x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a file name ending in /",
            with(|t| {
                t.file("bin/x/", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a NUL in a pax path",
            with(|t| {
                t.pax(&[("path", b"bin/a\0b")]).file("x", 0o644, b"x");
            }),
            Rule::BadPath,
        ),
        row(
            "a name that is not UTF-8",
            with(|t| {
                t.entry(&head(b"bin/\xff", b'0', 0o644, 0), b"");
            }),
            Rule::NotUtf8,
        ),
        row(
            "an entry before its directory",
            with(|t| {
                t.file("units/x.service", 0o644, b"x");
            }),
            Rule::ParentMissing,
        ),
        row(
            "a duplicate name",
            with(|t| {
                t.file("bin/x", 0o644, b"x").file("bin/x", 0o644, b"y");
            }),
            Rule::Duplicate,
        ),
        row(
            "bin/X beside bin/x",
            with(|t| {
                t.file("bin/x", 0o644, b"x").file("bin/X", 0o644, b"y");
            }),
            Rule::CaseFold,
        ),
        row(
            "bin/\u{130} beside bin/i",
            with(|t| {
                t.file("bin/i", 0o644, b"x")
                    .file("bin/\u{130}", 0o644, b"y");
            }),
            Rule::CaseFold,
        ),
        row(
            "an xattr pax record",
            with(|t| {
                t.pax(&[("SCHILY.xattr.user.a", b"1")])
                    .file("bin/x", 0o644, b"x");
            }),
            Rule::ExtendedAttribute,
        ),
        row(
            "an ACL pax record",
            with(|t| {
                t.pax(&[("SCHILY.acl.access", b"user::rwx")])
                    .file("bin/x", 0o644, b"x");
            }),
            Rule::ExtendedAttribute,
        ),
        row(
            "a pax record not on the list",
            with(|t| {
                t.pax(&[("comment", b"hi")]).file("bin/x", 0o644, b"x");
            }),
            Rule::PaxRecord,
        ),
        row(
            "setuid",
            with(|t| {
                t.file("bin/x", 0o4755, b"x");
            }),
            Rule::SpecialBits,
        ),
        row(
            "setgid",
            with(|t| {
                t.file("bin/x", 0o2755, b"x");
            }),
            Rule::SpecialBits,
        ),
        row(
            "sticky",
            with(|t| {
                t.entry(&head(b"bin/d/", b'5', 0o1755, 0), b"");
            }),
            Rule::SpecialBits,
        ),
        row(
            "trailing bytes after the end marker",
            {
                let mut t = valid_tar();
                t.end().raw(b"x").raw(&[0u8; 511]);
                zstd(&t.bytes())
            },
            Rule::Trailing,
        ),
        row(
            "zeros that are not whole blocks after the end",
            zstd(&ragged),
            Rule::Trailing,
        ),
        row(
            "a header after a lone zero block",
            zstd(&nonzero_end),
            Rule::Trailing,
        ),
        row("a second zstd frame", second, Rule::SecondFrame),
        row(
            "a truncated archive",
            framed[..framed.len() - 10].to_vec(),
            Rule::Truncated,
        ),
        row(
            "a pax header cut short",
            zstd(
                &[
                    base().bytes(),
                    Tar::header_block(&head(b"P", b'x', 0o644, 100)).to_vec(),
                    vec![b'1'; 50],
                ]
                .concat(),
            ),
            Rule::Truncated,
        ),
        row(
            "a GNU long name cut short",
            zstd(
                &[
                    base().bytes(),
                    Tar::header_block(&head(b"L", b'L', 0o644, 200)).to_vec(),
                    vec![b'a'; 50],
                ]
                .concat(),
            ),
            Rule::Truncated,
        ),
        row(
            "file data cut short",
            zstd(
                &[
                    base().bytes(),
                    Tar::header_block(&head(b"bin/x", b'0', 0o644, 1000)).to_vec(),
                    vec![b'x'; 10],
                ]
                .concat(),
            ),
            Rule::Truncated,
        ),
        row(
            "file padding cut short",
            zstd(
                &[
                    base().bytes(),
                    Tar::header_block(&head(b"bin/x", b'0', 0o644, 1)).to_vec(),
                    vec![b'x'; 100],
                ]
                .concat(),
            ),
            Rule::Truncated,
        ),
        row(
            "a tar that ends before its end marker",
            zstd(&valid_tar().bytes()),
            Rule::Truncated,
        ),
        row(
            "a decoder window above the cap",
            wide,
            Rule::Window {
                requested: 16 << 20,
            },
        ),
        row(
            "a checksum that does not add up",
            with(|t| {
                t.raw(&bad_sum);
            }),
            Rule::Header,
        ),
        row(
            "a header that is not ustar",
            with(|t| {
                t.raw(&no_magic);
            }),
            Rule::Header,
        ),
        row(
            "a base-256 size",
            with(|t| {
                t.raw(&base256);
            }),
            Rule::Header,
        ),
        row(
            "a directory with a size",
            with(|t| {
                t.entry(&head(b"bin/d/", b'5', 0o755, 1), b"x");
            }),
            Rule::Header,
        ),
        row(
            "two pax headers for one entry",
            with(|t| {
                t.pax(&[("path", b"bin/a")])
                    .pax(&[("path", b"bin/b")])
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "two GNU long names for one entry",
            with(|t| {
                t.long_name(b"bin/a")
                    .long_name(b"bin/b")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax header before the end",
            with(|t| {
                t.pax(&[("path", b"bin/a")]);
            }),
            Rule::Header,
        ),
        row(
            "a pax header over 8 KiB",
            with(|t| {
                t.pax(&[("path", &[b'a'; 9000])]).file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record with no length",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 9), b"path=bin\n")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record longer than its header",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 12), b"99 path=bin\n")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record shorter than its length field",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 3), b"2 x")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record with no key",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 3), b"3 \n")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record without its newline",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 12), b"12 path=binx")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax record without =",
            with(|t| {
                t.entry(&head(b"P", b'x', 0o644, 12), b"12 pathxbin\n")
                    .file("x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row(
            "a pax size that is not a number",
            with(|t| {
                t.pax(&[("size", b"1x")]).file("bin/x", 0o644, b"x");
            }),
            Rule::Header,
        ),
        row("not a zstd frame", valid, Rule::Decoder(String::new())),
    ]
}

/// The boundaries that must pass, each the largest a bound allows.
fn passing() -> Vec<(&'static str, Vec<u8>, Limits)> {
    let valid = valid_tar().end().bytes();
    let exact = Limits {
        max_unpacked_bytes: valid.len() as u64,
        max_entries: 7,
    };
    let mut window = frame_header(13 << 3);
    let body = valid_tar().end().bytes();
    for chunk in body.chunks(128 * 1024) {
        window.extend_from_slice(&block_header(false, 0, chunk.len() as u32));
        window.extend_from_slice(chunk);
    }
    window.extend_from_slice(&block_header(true, 0, 0));
    vec![
        (
            "exactly max_unpacked_bytes and max_entries",
            zstd(&valid),
            exact,
        ),
        (
            "a 255-byte name in a pax path",
            with(|t| {
                t.pax(&[
                    ("path", long(255).as_bytes()),
                    ("mtime", b"1.5"),
                    ("uid", b"0"),
                ])
                .file("x", 0o644, b"x");
            }),
            limits(),
        ),
        (
            "a 255-byte name in the ustar prefix",
            with(|t| {
                t.pax(&[("path", format!("bin/{}", "p".repeat(150)).as_bytes())])
                    .entry(&head(b"d/", b'5', 0o755, 0), b"");
                t.entry(
                    &head(
                        format!("bin/{}/{}", "p".repeat(150), "n".repeat(100)).as_bytes(),
                        b'0',
                        0o644,
                        0,
                    ),
                    b"",
                );
            }),
            limits(),
        ),
        (
            "a 255-byte GNU long name",
            with(|t| {
                t.long_name(long(255).as_bytes()).file("x", 0o644, b"x");
            }),
            limits(),
        ),
        ("depth 16", with(|t| deep(t, 16)), limits()),
        (
            "a manifest.toml of 64 KiB",
            {
                let mut t = Tar::new();
                t.file("manifest.toml", 0o644, &manifest_of(65_536))
                    .dir("files")
                    .file("files/c.conf", 0o644, b"c\n");
                zstd(&t.end().bytes())
            },
            limits(),
        ),
        (
            "a pax size and a zero-length file",
            with(|t| {
                t.pax(&[
                    ("size", b"0"),
                    ("gname", b"g"),
                    ("uname", b"u"),
                    ("gid", b"1"),
                ])
                .file("bin/empty", 0o644, b"");
            }),
            limits(),
        ),
        ("a window of exactly the cap", window, limits()),
        (
            "a GNU header for a file",
            with(|t| {
                let mut gnu = head(b"bin/g", b'0', 0o644, 0);
                gnu.gnu = true;
                t.entry(&gnu, b"");
            }),
            limits(),
        ),
        (
            "zero padding to a record after the end",
            {
                let mut t = valid_tar();
                t.end().raw(&[0u8; 8192]);
                zstd(&t.bytes())
            },
            limits(),
        ),
    ]
}

fn assert_nothing_left(rig: &Rig, name: &str) {
    assert!(rig.listing().is_empty(), "{name}: {:?}", rig.listing());
}

/// Catches a bound or a shape rule that is not enforced: every row is `Failed`, release,
/// `bad_archive`, names the rule it breaks, and leaves no `.stage-*` and no version; every
/// boundary row (each bound at its largest allowed value) unpacks.
#[test]
fn archive_bounds_and_shape_rules_refuse_each_violation() {
    for row in rows() {
        let rig = Rig::new("b-row");
        let (path, order) = rig.archive("a.tar.zst", &row.archive);
        let fake = Faulty::new(rig.host());
        let err = unpack(&path, &order, &row.limits, &fake).unwrap_err();
        let UnpackError::BadArchive { rule, entry, .. } = &err else {
            panic!("{}: {err:?}", row.name)
        };
        // The offending entry was checked before anything of it was made: no create of its
        // name, except the earlier entry a duplicate repeats, and a file whose data the archive
        // cut short (a stream rule, found while the file is written).
        let name = entry.trim_end_matches('/');
        let made = fake
            .calls()
            .iter()
            .filter(|c| match c {
                Call::CreateFile(p) | Call::CreateDir(p) => {
                    let inside: PathBuf = p.components().skip(2).collect();
                    inside
                        .as_os_str()
                        .as_encoded_bytes()
                        .escape_ascii()
                        .to_string()
                        == name
                }
                _ => false,
            })
            .count();
        let allowed = match rule {
            Rule::Duplicate => 1..=1,
            Rule::Truncated => 0..=1,
            _ => 0..=0,
        };
        assert!(
            allowed.contains(&made),
            "{}: {made}: {:?}",
            row.name,
            fake.calls()
        );
        match (&row.rule, rule) {
            (Rule::Decoder(_), Rule::Decoder(_)) => {}
            (want, got) => assert_eq!(want, got, "{}", row.name),
        }
        assert_eq!(
            err.outcome(),
            (Outcome::Failure, Class::Release, "bad_archive"),
            "{}",
            row.name
        );
        assert!(err.to_string().starts_with("bad archive: "), "{err}");
        assert_nothing_left(&rig, row.name);
    }
    for (name, archive, limits) in passing() {
        let rig = Rig::new("b-pass");
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let unpacked = unpack(&path, &order, &limits, &rig.host());
        assert!(unpacked.is_ok(), "{name}: {unpacked:?}");
        assert!(rig.service().join(VERSION).is_dir(), "{name}");
    }
}

/// Catches the decompressed bound being judged after the fact: a frame of a few kilobytes that
/// would decompress to a gibibyte of zeros after the archive's end is stopped once
/// `max_unpacked_bytes` is passed, within one buffer of it, and nothing past the archive is
/// written.
#[test]
fn a_zip_bomb_is_stopped_within_one_buffer_of_the_bound() {
    let rig = Rig::new("b-bomb");
    let valid = valid_tar().end().bytes();
    let archive = zstd_with(&valid, WINDOW_1M, &rle_blocks(0, 8192));
    assert!(archive.len() < 64 * 1024);
    let (path, order) = rig.archive("a.tar.zst", &archive);
    let bound = 1 << 20;
    let limits = Limits {
        max_unpacked_bytes: bound,
        max_entries: 100,
    };
    let fake = Faulty::new(rig.host());
    let err = unpack(&path, &order, &limits, &fake).unwrap_err();
    let UnpackError::BadArchive {
        rule: Rule::TooLarge,
        at,
        ..
    } = err
    else {
        panic!("{err:?}")
    };
    assert!(at > bound && at <= bound + BUFFER as u64, "{at}");
    assert!(fake.written() <= valid.len(), "{}", fake.written());
}

/// Catches a check made after an entry was written: for an entry that breaks a rule, the port
/// sees no create and no write of it, while the entries before it were written.
#[test]
fn no_byte_is_written_for_an_entry_that_breaks_a_rule() {
    let offending: Vec<(&str, Vec<u8>, Limits)> = vec![
        (
            "bin/s",
            with(|t| {
                t.file("bin/s", 0o4755, b"s");
            }),
            limits(),
        ),
        (
            "bin/x",
            with(|t| {
                t.file("bin/x", 0o644, b"x").file("bin/x", 0o644, b"y");
            }),
            limits(),
        ),
        (
            "bin/big",
            with(|t| {
                t.file("bin/big", 0o644, &[7u8; 8192]);
            }),
            Limits {
                max_unpacked_bytes: 8192,
                max_entries: 100,
            },
        ),
        (
            "bin/l",
            with(|t| {
                t.entry(&head(b"bin/l", b'2', 0o777, 0), b"");
            }),
            limits(),
        ),
        (
            "manifest.toml",
            {
                let mut t = Tar::new();
                t.dir("bin").file("bin/x", 0o644, b"x").file(
                    "manifest.toml",
                    0o644,
                    &manifest_of(65_537),
                );
                zstd(&t.end().bytes())
            },
            limits(),
        ),
    ];
    for (name, archive, limits) in offending {
        let rig = Rig::new("b-nobyte");
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let fake = Faulty::new(rig.host());
        unpack(&path, &order, &limits, &fake).unwrap_err();
        let calls = fake.calls();
        let creates = |p: &Path| {
            calls
                .iter()
                .filter(|c| matches!(c, Call::CreateFile(q) if q.ends_with(p)))
                .count()
        };
        let writes = calls
            .iter()
            .filter(|c| matches!(c, Call::Write(q, _) if q.ends_with(name)))
            .count();
        let before = if name == "bin/x" { 1 } else { 0 };
        assert_eq!(creates(Path::new(name)), before, "{name}: {calls:?}");
        assert_eq!(writes, before, "{name}: {calls:?}");
        assert!(
            calls.iter().any(|c| matches!(c, Call::CreateDir(_))),
            "{name}"
        );
    }
}

/// Catches a path check that lets an entry reach outside the staging directory: with `..`, an
/// absolute path, and a link followed by a write through it, no call of the port names a path
/// outside `<service>/.stage-*`, and nothing appears outside the packages directory.
#[test]
fn nothing_is_written_outside_the_staging_directory() {
    let archives = [
        with(|t| {
            t.file("bin/../../escape", 0o644, b"x");
        }),
        with(|t| {
            t.file("/tmp/escape", 0o644, b"x");
        }),
        with(|t| {
            t.entry(&head(b"bin/l", b'2', 0o777, 0), b"")
                .file("bin/l/escape", 0o644, b"x");
        }),
        with(|t| {
            t.pax(&[("path", b"../escape")]).file("x", 0o644, b"x");
        }),
    ];
    for archive in archives {
        let rig = Rig::new("b-outside");
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let fake = Faulty::new(rig.host());
        unpack(&path, &order, &limits(), &fake).unwrap_err();
        for call in fake.calls().iter().filter(|c| c.writes()) {
            for p in call.paths() {
                // The staging directory, or the seal names the cleanup removes after it.
                let rest = p
                    .strip_prefix("tierstore")
                    .map(|r| r.to_string_lossy().into_owned());
                let inside = rest.is_ok_and(|r| {
                    r.is_empty()
                        || r.starts_with(".stage-")
                        || r == ".1.5.0.seal.tmp"
                        || r == "1.5.0.seal"
                });
                assert!(inside, "{call:?}");
            }
        }
        let mut top: Vec<String> = std::fs::read_dir(rig.scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        assert_eq!(top, ["a.tar.zst", "packages"]);
        assert_eq!(
            std::fs::read_dir(&rig.packages).unwrap().count(),
            1,
            "only the service directory"
        );
    }
}

/// Catches a window cap that is not applied before the decoder allocates for the frame: a
/// frame header built by hand that declares a window one step above the cap, or a terabyte
/// window, is refused with the window it asked for, whatever follows it.
#[test]
fn the_decoder_has_a_window_cap() {
    assert_eq!(MAX_WINDOW_BYTES, 8 << 20);
    for (descriptor, requested) in [
        (14u8 << 3, 16u64 << 20),
        ((30 << 3) | 7, (1 << 40) + 7 * (1 << 37)),
    ] {
        let rig = Rig::new("b-window");
        let mut archive = frame_header(descriptor);
        archive.extend_from_slice(&[0xffu8; 64]);
        let (path, order) = rig.archive("a.tar.zst", &archive);
        let err = unpack(&path, &order, &limits(), &rig.host()).unwrap_err();
        let UnpackError::BadArchive { rule, at, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(rule, &Rule::Window { requested });
        assert_eq!(*at, 0);
        assert!(
            err.to_string().contains("above the cap of 8388608"),
            "{err}"
        );
    }
}
