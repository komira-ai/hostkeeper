//! A deb read for one file of its `data.tar`: the shim of `shim-signed` and the network GRUB of
//! `grub-efi-<arch>-signed`, from the package pool of the ISO.
//!
//! A deb is an `ar` archive (`!<arch>\n`, then members of a 60-byte header and a body padded to
//! an even length). The first member whose name starts with `data.tar` is read; its compression
//! is told by its name, as `dpkg-deb` tells it: `data.tar.zst` (zstd), `data.tar.xz` (xz),
//! `data.tar.gz` (gzip) or `data.tar` (none). Ubuntu 24.04's `shim-signed` carries
//! `data.tar.xz` and its `grub2-signed` debs `data.tar.zst`, so the reader takes any of them, and
//! refuses another by its name. An xz body is decoded in memory, at most [`MAX_XZ_TAR_BYTES`].
//!
//! The file is the tar entry whose path, with a leading `./` removed, is the pin's `deb_path`. It
//! must be a regular file; it is copied out through [`copy`], so it is measured as it is written
//! and stopped at its cap.
//!
//! The deb comes out of an ISO whose SHA-256 matched its pin, so its bytes are the release's;
//! the reader still refuses what is not of this form rather than guess.

use std::io::{self, BufReader, Cursor, Read, Write};

use crate::copy::{CopyError, Measured, copy};

/// The magic bytes of an `ar` archive.
const AR_MAGIC: &[u8; 8] = b"!<arch>\n";
/// The size of an `ar` member header.
const AR_HEADER: usize = 60;
/// The most bytes an xz `data.tar` may decode to.
pub(crate) const MAX_XZ_TAR_BYTES: u64 = 256 << 20;

/// Why the file cannot be taken out of the deb.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DebError {
    /// The deb is not of the form above; the text says where.
    #[error("{0}")]
    Malformed(String),
    /// No entry of `data.tar` has the path.
    #[error("not found")]
    NotFound,
    /// The entry is not a regular file (a link, a directory).
    #[error("not a regular file")]
    NotAFile,
    /// Copying the file out: too large, a read that failed, a write that failed.
    #[error("{0}")]
    Copy(CopyError),
}

fn malformed(why: impl Into<String>) -> DebError {
    DebError::Malformed(why.into())
}

/// Takes the file `path` out of the deb `deb` into `to`, at most `cap` bytes.
pub(crate) fn extract(
    deb: &mut dyn Read,
    path: &str,
    to: &mut dyn Write,
    cap: u64,
) -> Result<Measured, DebError> {
    let mut magic = [0_u8; 8];
    deb.read_exact(&mut magic)
        .map_err(|_| malformed("not an ar archive"))?;
    if &magic != AR_MAGIC {
        return Err(malformed("not an ar archive"));
    }
    loop {
        let mut header = [0_u8; AR_HEADER];
        deb.read_exact(&mut header)
            .map_err(|_| malformed("no data.tar member"))?;
        let (name, size) = member(&header)?;
        let mut body = deb.take(size);
        if name.starts_with("data.tar") {
            let tar = decompress(&name, &mut body)?;
            return find(tar, path, to, cap);
        }
        // The body, and the byte that pads a body of odd length.
        let padded = size.saturating_add(size % 2);
        body.set_limit(padded);
        let skipped =
            io::copy(&mut body, &mut io::sink()).map_err(|e| DebError::Copy(CopyError::Read(e)))?;
        if skipped != padded {
            return Err(malformed(format!("the member {name} is cut short")));
        }
    }
}

/// The name and the body size of the member header `header`.
fn member(header: &[u8; AR_HEADER]) -> Result<(String, u64), DebError> {
    let text = |range: std::ops::Range<usize>| String::from_utf8_lossy(&header[range]).into_owned();
    let name = text(0..16).trim_end().trim_end_matches('/').to_owned();
    let size = text(48..58)
        .trim_end()
        .parse::<u64>()
        .ok()
        .filter(|_| &header[58..60] == b"`\n")
        .ok_or_else(|| {
            malformed(format!(
                "the header of the member {name:?} is not an ar header"
            ))
        })?;
    Ok((name, size))
}

/// A reader of the decompressed `data.tar` named `name`, whose body is `body`.
fn decompress<'a>(name: &str, body: &'a mut dyn Read) -> Result<Box<dyn Read + 'a>, DebError> {
    match name {
        "data.tar" => Ok(Box::new(body)),
        "data.tar.gz" => Ok(Box::new(flate2::read::GzDecoder::new(body))),
        "data.tar.zst" => ruzstd::decoding::StreamingDecoder::new(body)
            .map(|decoder| Box::new(decoder) as Box<dyn Read>)
            .map_err(|e| malformed(format!("{name}: {e}"))),
        "data.tar.xz" => {
            let mut tar = Bounded {
                bytes: Vec::new(),
                cap: MAX_XZ_TAR_BYTES,
            };
            lzma_rs::xz_decompress(&mut BufReader::new(body), &mut tar)
                .map_err(|e| malformed(format!("{name}: {e}")))?;
            Ok(Box::new(Cursor::new(tar.bytes)))
        }
        other => Err(malformed(format!(
            "{other} is not a data.tar this reader decodes (zst, xz, gz or none)"
        ))),
    }
}

/// Copies the tar entry at `path` of `tar` into `to`.
fn find(
    tar: Box<dyn Read + '_>,
    path: &str,
    to: &mut dyn Write,
    cap: u64,
) -> Result<Measured, DebError> {
    let mut archive = tar::Archive::new(tar);
    let read = |e: io::Error| DebError::Copy(CopyError::Read(e));
    for entry in archive.entries().map_err(read)? {
        let mut entry = entry.map_err(read)?;
        let name = entry.path_bytes().into_owned();
        if name.strip_prefix(b"./").unwrap_or(&name) != path.as_bytes() {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(DebError::NotAFile);
        }
        return copy(&mut entry, to, cap).map_err(DebError::Copy);
    }
    Err(DebError::NotFound)
}

/// A writer into memory that refuses to hold more than `cap` bytes.
struct Bounded {
    bytes: Vec<u8>,
    cap: u64,
}

impl Write for Bounded {
    fn write(&mut self, chunk: &[u8]) -> io::Result<usize> {
        let total = self.bytes.len().saturating_add(chunk.len());
        if u64::try_from(total).unwrap_or(u64::MAX) > self.cap {
            return Err(io::Error::other(format!(
                "decodes to more than {} bytes",
                self.cap
            )));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "deb_tests.rs"]
mod tests;
