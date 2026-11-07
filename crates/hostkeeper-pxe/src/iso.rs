//! An ISO 9660 image (ECMA-119) read for the extent of one file, found by its Rock Ridge path
//! (`casper/vmlinuz`, `pool/main/s/shim-signed/<deb>`). Ubuntu's live-server ISOs carry Rock
//! Ridge names (an `SP` entry in the root's first record, an `NM` entry in each record), and a
//! path of the pins is matched against those names, byte for byte; an image without them is
//! refused.
//!
//! The image is read only after its SHA-256 matched its pin, so its bytes are the release's. The
//! reader still checks every length and offset it uses against the image, so a wrong pin is a
//! refusal, never a panic or a read past the image:
//!
//! - the volume descriptors from sector 16 up to the terminator; the first primary one is used,
//!   and its logical block size must be 2048;
//! - each directory record lies within its directory, and a directory is at most
//!   [`MAX_DIRECTORY_BYTES`];
//! - each extent lies within the image; a file recorded in more than one extent (only a file of
//!   4 GiB or more needs that) is refused;
//! - the system use entries of a record, and the continuation areas (`CE`) they point to, at most
//!   [`MAX_CONTINUATIONS`] of them, each within the image.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;

/// The size of a sector, and the one logical block size read.
pub(crate) const SECTOR: u64 = 2048;
/// The first sector of the volume descriptors.
const FIRST_DESCRIPTOR: u64 = 16;
/// The most bytes of one directory read.
pub(crate) const MAX_DIRECTORY_BYTES: u64 = 16 << 20;
/// The most continuation areas of one record's system use entries.
pub(crate) const MAX_CONTINUATIONS: usize = 16;
/// The bytes of a directory record before its name.
const RECORD_HEAD: usize = 33;
/// The directory flag of a record.
const DIRECTORY: u8 = 0x02;
/// The multi-extent flag of a record: the file goes on in the next record.
const MULTI_EXTENT: u8 = 0x80;

/// Why a file cannot be taken out of the image.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IsoError {
    /// The image is not of the form above; the text says where.
    #[error("{0}")]
    Malformed(String),
    /// No record of a directory on the path has the name.
    #[error("not found")]
    NotFound,
    /// The path names a directory.
    #[error("a directory, not a file")]
    NotAFile,
    /// Reading the image failed.
    #[error("{0}")]
    Io(io::Error),
}

fn malformed(why: impl Into<String>) -> IsoError {
    IsoError::Malformed(why.into())
}

/// Where a file's bytes are in the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    pub offset: u64,
    pub bytes: u64,
}

/// One directory record, as far as this reader needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    extent: Extent,
    flags: u8,
    /// The ISO 9660 name: `[0]` for a directory itself, `[1]` for its parent.
    iso_name: Vec<u8>,
    /// The system use area.
    system_use: Vec<u8>,
}

/// An opened image.
pub(crate) struct Iso {
    file: File,
    len: u64,
    root: Record,
    /// The bytes skipped at the start of every system use area (`SP`'s `LEN_SKP`).
    skip: usize,
}

/// A little-endian `u32` at `at` of `bytes`, which must hold it.
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, IsoError> {
    let field: [u8; 4] = bytes
        .get(at..at.saturating_add(4))
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| malformed("a field runs past its record"))?;
    Ok(u32::from_le_bytes(field))
}

impl Iso {
    /// Opens `file` as an image: the primary volume descriptor, the root directory, and the
    /// Rock Ridge `SP` entry of the root's first record.
    pub(crate) fn open(file: File) -> Result<Iso, IsoError> {
        let len = file.metadata().map_err(IsoError::Io)?.len();
        let mut sector = FIRST_DESCRIPTOR;
        let descriptor = loop {
            let block = read_at(&file, len, sector.saturating_mul(SECTOR), SECTOR)?;
            if block.get(1..6) != Some(b"CD001".as_slice()) {
                return Err(malformed(format!(
                    "sector {sector} is not a volume descriptor"
                )));
            }
            match block.first() {
                Some(1) => break block,
                Some(255) => return Err(malformed("no primary volume descriptor")),
                _ => sector = sector.saturating_add(1),
            }
        };
        let block_size = descriptor
            .get(128..130)
            .map(|b| u16::from_le_bytes([b[0], b[1]]));
        if block_size != Some(2048) {
            return Err(malformed("the logical block size is not 2048"));
        }
        let root = record(descriptor.get(156..190).unwrap_or_default())?;
        let mut iso = Iso {
            file,
            len,
            root,
            skip: 0,
        };
        let first = iso.records(&iso.root.clone())?.into_iter().next();
        let sp = first.as_ref().and_then(|r| r.system_use.get(..7));
        iso.skip = match sp {
            Some([b'S', b'P', 7, 1, 0xBE, 0xEF, skip]) => usize::from(*skip),
            _ => return Err(malformed("the image has no Rock Ridge names")),
        };
        Ok(iso)
    }

    /// The extent of the file at `path`, its parts separated by `/`.
    pub(crate) fn find(&self, path: &str) -> Result<Extent, IsoError> {
        let mut at = self.root.clone();
        for part in path.split('/') {
            if at.flags & DIRECTORY == 0 {
                return Err(IsoError::NotFound);
            }
            let mut found = None;
            for record in self.records(&at)? {
                let named = !matches!(record.iso_name.as_slice(), [0] | [1]);
                if named && self.name(&record)? == part.as_bytes() {
                    found = Some(record);
                    break;
                }
            }
            at = found.ok_or(IsoError::NotFound)?;
        }
        if at.flags & DIRECTORY != 0 {
            return Err(IsoError::NotAFile);
        }
        if at.flags & MULTI_EXTENT != 0 {
            return Err(malformed("the file is recorded in more than one extent"));
        }
        self.within(at.extent)?;
        Ok(at.extent)
    }

    /// A reader of the bytes of `extent`.
    pub(crate) fn reader(&self, extent: Extent) -> Result<impl Read + '_, IsoError> {
        let mut file = &self.file;
        file.seek(SeekFrom::Start(extent.offset))
            .map_err(IsoError::Io)?;
        Ok(file.take(extent.bytes))
    }

    /// `extent`, unless it runs past the image.
    fn within(&self, extent: Extent) -> Result<Extent, IsoError> {
        match extent.offset.checked_add(extent.bytes) {
            Some(end) if end <= self.len => Ok(extent),
            _ => Err(malformed("an extent runs past the end of the image")),
        }
    }

    /// The records of the directory `dir`, in order: a record never crosses a sector, and a
    /// zero length byte ends the records of its sector.
    fn records(&self, dir: &Record) -> Result<Vec<Record>, IsoError> {
        if dir.extent.bytes > MAX_DIRECTORY_BYTES {
            return Err(malformed("a directory is larger than 16 MiB"));
        }
        let extent = self.within(dir.extent)?;
        let bytes = read_at(&self.file, self.len, extent.offset, extent.bytes)?;
        let mut records = Vec::new();
        let mut at = 0_usize;
        while let Some(&length) = bytes.get(at) {
            if length == 0 {
                let sector = usize::try_from(SECTOR).unwrap_or(usize::MAX);
                at = (at / sector + 1).saturating_mul(sector);
                continue;
            }
            let end = at.saturating_add(usize::from(length));
            let text = bytes
                .get(at..end)
                .ok_or_else(|| malformed("a directory record runs past its directory"))?;
            records.push(record(text)?);
            at = end;
        }
        Ok(records)
    }

    /// The Rock Ridge name of `record`: the content of its `NM` entries, in order, following
    /// its continuation areas.
    fn name(&self, record: &Record) -> Result<Vec<u8>, IsoError> {
        let mut name = Vec::new();
        let mut area = record
            .system_use
            .get(self.skip..)
            .unwrap_or_default()
            .to_vec();
        let mut continuations = 0;
        loop {
            let mut next = None;
            let mut at = 0_usize;
            while let Some(head) = area.get(at..at.saturating_add(4)) {
                let length = usize::from(head[2]);
                let entry = area
                    .get(at..at.saturating_add(length))
                    .filter(|_| length >= 4)
                    .ok_or_else(|| malformed("a system use entry runs past its area"))?;
                match &entry[..2] {
                    b"NM" => name.extend_from_slice(entry.get(5..).unwrap_or_default()),
                    b"CE" => next = Some(continuation(entry)?),
                    b"ST" => break,
                    _ => {}
                }
                at = at.saturating_add(length);
            }
            let Some(extent) = next else {
                return Ok(name);
            };
            continuations += 1;
            if continuations > MAX_CONTINUATIONS {
                return Err(malformed("more than 16 continuation areas"));
            }
            let extent = self.within(extent)?;
            area = read_at(&self.file, self.len, extent.offset, extent.bytes)?;
        }
    }
}

/// The continuation area a `CE` entry points to: its block, its offset in that block and its
/// length, each the little-endian half of a both-endian field.
fn continuation(entry: &[u8]) -> Result<Extent, IsoError> {
    let block = u64::from(u32_at(entry, 4)?);
    let offset = u64::from(u32_at(entry, 12)?);
    let bytes = u64::from(u32_at(entry, 20)?);
    Ok(Extent {
        offset: block.saturating_mul(SECTOR).saturating_add(offset),
        bytes,
    })
}

/// The record `bytes`, its length byte included.
fn record(bytes: &[u8]) -> Result<Record, IsoError> {
    let name_len = usize::from(*bytes.get(32).unwrap_or(&0));
    let name_end = RECORD_HEAD.saturating_add(name_len);
    let iso_name = bytes
        .get(RECORD_HEAD..name_end)
        .filter(|_| usize::from(bytes[0]) == bytes.len())
        .ok_or_else(|| malformed("a directory record is shorter than its name"))?;
    // A padding byte follows a name of even length.
    let system_use = bytes
        .get(name_end + (1 - name_len % 2)..)
        .unwrap_or_default();
    let block = u64::from(u32_at(bytes, 2)?);
    let extended = u64::from(bytes[1]);
    Ok(Record {
        extent: Extent {
            offset: block.saturating_add(extended).saturating_mul(SECTOR),
            bytes: u64::from(u32_at(bytes, 10)?),
        },
        flags: bytes[25],
        iso_name: iso_name.to_vec(),
        system_use: system_use.to_vec(),
    })
}

/// `bytes` bytes at `offset` of `file`, whose length is `len`.
fn read_at(file: &File, len: u64, offset: u64, bytes: u64) -> Result<Vec<u8>, IsoError> {
    let end = offset.checked_add(bytes).filter(|end| *end <= len);
    let size = end
        .and_then(|_| usize::try_from(bytes).ok())
        .ok_or_else(|| malformed("a read runs past the end of the image"))?;
    let mut buffer = vec![0_u8; size];
    file.read_exact_at(&mut buffer, offset)
        .map_err(IsoError::Io)?;
    Ok(buffer)
}

#[cfg(test)]
#[path = "iso_tests.rs"]
mod tests;
