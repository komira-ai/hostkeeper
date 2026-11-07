//! Debs made in memory: an `ar` archive of `debian-binary`, a `control.tar.xz` and a `data.tar`
//! of the compression asked for, whose entries are given with a leading `./` as `dpkg-deb`
//! writes them.

use std::io::{Cursor, Write};

/// What a tar entry is.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    File,
    Symlink,
}

/// A tar of `entries` (path without `./`, bytes, kind), with a directory entry first.
pub(crate) fn tar(entries: &[(&str, &[u8], Kind)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    builder
        .append_data(&mut dir, "./usr/", &[][..])
        .expect("a directory entry");
    for (path, bytes, kind) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        match kind {
            Kind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(bytes.len() as u64);
                builder
                    .append_data(&mut header, format!("./{path}"), *bytes)
                    .expect("a file entry");
            }
            Kind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                builder
                    .append_link(&mut header, format!("./{path}"), "elsewhere")
                    .expect("a link entry");
            }
        }
    }
    builder.into_inner().expect("the tar")
}

/// `tar` compressed as the member `name` (`data.tar`, `data.tar.gz`, `.xz` or `.zst`, or any
/// other name, left as it is).
pub(crate) fn compress(name: &str, tar: &[u8]) -> Vec<u8> {
    match name {
        "data.tar.gz" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(tar).expect("gzip");
            encoder.finish().expect("gzip")
        }
        "data.tar.xz" => {
            let mut out = Vec::new();
            lzma_rs::xz_compress(&mut Cursor::new(tar), &mut out).expect("xz");
            out
        }
        "data.tar.zst" => {
            ruzstd::encoding::compress_to_vec(tar, ruzstd::encoding::CompressionLevel::Fastest)
        }
        _ => tar.to_vec(),
    }
}

/// One `ar` member: the header, the body, a padding byte after a body of odd length.
pub(crate) fn member(name: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        format!("{name}/"),
        0,
        0,
        0,
        100_644,
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(b'\n');
    }
    out
}

/// A deb whose `data.tar` member is `data_name`, holding `entries`.
pub(crate) fn deb(data_name: &str, entries: &[(&str, &[u8], Kind)]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    out.extend(member("debian-binary", b"2.0\n"));
    // A control member of odd length, so the padding byte is skipped.
    out.extend(member("control.tar.xz", b"not read"));
    out.extend(member(data_name, &compress(data_name, &tar(entries))));
    out
}
