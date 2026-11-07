//! One member of a pinned archive, read in memory: a zip (stored or deflated) or a
//! gzip-compressed tar. Nothing is unpacked to disk, so no member name can place a file.
//!
//! A zip member must be no larger than the limit as the central directory lists it, and its
//! output must be exactly that listed size; a tar member is read up to one byte past the
//! limit. Every offset and length read from a zip is added with saturating arithmetic, so a
//! hostile
//! value lands outside the archive and is refused.
//!
//! The archive has been checked against its pinned sha256 before it is read here; the limits
//! below only keep a hostile or damaged archive from filling memory.

use std::io::Read;

use flate2::read::{DeflateDecoder, GzDecoder};
use hostkeeper_mdm_core::pins::Packing;

/// The largest member read, in bytes.
pub const MAX_MEMBER: u64 = 512 * 1024 * 1024;

const LOCAL: usize = 0x0403_4b50;
const CENTRAL: usize = 0x0201_4b50;
const END: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];

/// Why a member could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The archive is damaged or of a kind this reader does not handle.
    #[error("{archive} is not a readable {packing:?} archive: {why}")]
    Unreadable {
        /// The archive's name.
        archive: String,
        /// How it is packed.
        packing: Packing,
        /// What is wrong.
        why: String,
    },
    /// The member is not in the archive, or is empty.
    #[error("{archive} has no {member}, or it is empty")]
    NoMember {
        /// The archive's name.
        archive: String,
        /// The member.
        member: String,
    },
}

/// The bytes of `member` in `bytes`, an archive called `archive` (for errors).
pub fn member(
    archive: &str,
    packing: Packing,
    bytes: &[u8],
    member: &str,
) -> Result<Vec<u8>, ArchiveError> {
    member_with_limit(archive, packing, bytes, member, MAX_MEMBER)
}

/// [`member`] with a member of at most `limit` bytes: a larger one is an error, never a
/// truncated read.
pub fn member_with_limit(
    archive: &str,
    packing: Packing,
    bytes: &[u8],
    member: &str,
    limit: u64,
) -> Result<Vec<u8>, ArchiveError> {
    let found = match packing {
        Packing::Zip => zip_member(bytes, member, limit),
        Packing::TarGz => tar_member(bytes, member, limit),
    };
    let found = found.map_err(|why| ArchiveError::Unreadable {
        archive: archive.to_owned(),
        packing,
        why,
    })?;
    found
        .filter(|b| !b.is_empty())
        .ok_or_else(|| ArchiveError::NoMember {
            archive: archive.to_owned(),
            member: member.to_owned(),
        })
}

fn why(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn zip_member(bytes: &[u8], member: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let bad = || "malformed zip".to_owned();
    // A little-endian number of `n` bytes at `at`.
    let num = |at: usize, n: usize| -> Result<usize, String> {
        let b = bytes.get(at..at.saturating_add(n)).ok_or_else(bad)?;
        Ok(b.iter()
            .rev()
            .fold(0, |acc, v| (acc << 8) | usize::from(*v)))
    };
    // A sum of offsets and lengths read from the archive. One that would overflow stays at
    // `usize::MAX`, which no read of the archive accepts.
    let sum =
        |parts: &[usize]| -> usize { parts.iter().fold(0usize, |acc, p| acc.saturating_add(*p)) };
    // The end record is the last 22 bytes, or earlier by the length of a comment.
    let last = bytes.len().saturating_sub(22);
    let end = (last.saturating_sub(65535)..=last)
        .rev()
        .find(|i| bytes.get(*i..i.saturating_add(4)) == Some(&END[..]))
        .ok_or_else(bad)?;
    let mut at = num(sum(&[end, 16]), 4)?;
    for _ in 0..num(sum(&[end, 10]), 2)? {
        if num(at, 4)? != CENTRAL {
            return Err(bad());
        }
        let (method, packed, plain) = (
            num(sum(&[at, 10]), 2)?,
            num(sum(&[at, 20]), 4)?,
            num(sum(&[at, 24]), 4)?,
        );
        let (name, extra, comment) = (
            num(sum(&[at, 28]), 2)?,
            num(sum(&[at, 30]), 2)?,
            num(sum(&[at, 32]), 2)?,
        );
        let local = num(sum(&[at, 42]), 4)?;
        let name_at = sum(&[at, 46]);
        let listed = bytes.get(name_at..sum(&[name_at, name])).ok_or_else(bad)?;
        if listed == member.as_bytes() {
            // The local header repeats the name and extra lengths; the data follows them.
            if num(local, 4)? != LOCAL {
                return Err(bad());
            }
            let data = sum(&[
                local,
                30,
                num(sum(&[local, 26]), 2)?,
                num(sum(&[local, 28]), 2)?,
            ]);
            let packed = bytes.get(data..sum(&[data, packed])).ok_or_else(bad)?;
            return inflate(method, packed, plain as u64, limit).map(Some);
        }
        at = sum(&[name_at, name, extra, comment]);
    }
    Ok(None)
}

/// The member's bytes, which must be exactly the `plain` bytes the central directory lists,
/// and `plain` must not exceed `limit`.
fn inflate(method: usize, packed: &[u8], plain: u64, limit: u64) -> Result<Vec<u8>, String> {
    if plain > limit {
        return Err(format!(
            "member is listed as {plain} bytes, larger than the {limit} byte limit"
        ));
    }
    let mut out = Vec::new();
    match method {
        0 => {
            if packed.len() as u64 != plain {
                return Err(format!(
                    "member is {} bytes, not the {plain} the zip lists",
                    packed.len()
                ));
            }
            out.extend_from_slice(packed);
        }
        8 => {
            // One byte more than listed is read, so a longer member is seen as longer.
            DeflateDecoder::new(packed)
                .take(plain.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(why)?;
        }
        other => return Err(format!("compression method {other} is not supported")),
    }
    let got = out.len() as u64;
    if got > plain {
        return Err(format!(
            "member is more than the {plain} bytes the zip lists"
        ));
    }
    if got != plain {
        return Err(format!(
            "member is {got} bytes, not the {plain} the zip lists"
        ));
    }
    Ok(out)
}

fn tar_member(bytes: &[u8], member: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let mut tar = tar::Archive::new(GzDecoder::new(bytes));
    for entry in tar.entries().map_err(why)? {
        let entry = entry.map_err(why)?;
        let named = entry.path().map_err(why)?.to_str() == Some(member);
        if named {
            let mut out = Vec::new();
            // One byte more than the limit is read, so a longer member is seen as longer.
            entry
                .take(limit.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(why)?;
            if out.len() as u64 > limit {
                return Err(format!("member is larger than {limit} bytes"));
            }
            return Ok(Some(out));
        }
    }
    Ok(None)
}
