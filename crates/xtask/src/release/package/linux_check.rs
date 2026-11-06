//! `release check-linux-pkg`: a built deb or rpm, read back, carries the files of
//! [`nfpm_stage`](super::nfpm_stage) at the daemon's paths: `/usr/lib/hostkeeper/gh`, a regular
//! file of mode 0755, and `/usr/lib/hostkeeper/trusted_root.json`, 0644, each owned by root and
//! group root, each with the bytes of the file staged for it. Every way the package differs is
//! reported, one line each.
//!
//! The package is told apart by its first bytes and read with no tool:
//!
//! - a deb is an `ar` archive whose `data.tar.gz` member is the tree it installs; an entry's mode
//!   and owner are its tar header's (owner and group by name, and uid and gid 0);
//! - an rpm is a lead, a signature header, a header and a gzip `cpio` (newc) payload. The mode,
//!   the owner and the group rpm installs a file with are the header's (`FILEMODES`,
//!   `FILEUSERNAME`, `FILEGROUPNAME`, one per file named by `DIRNAMES`, `BASENAMES` and
//!   `DIRINDEXES`); the bytes are the payload's.
//!
//! nfpm 2.48.0 compresses both with gzip, its default, which `nfpm.yaml` does not change; any
//! other compression is refused by name rather than read.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use super::nfpm_stage::{GH_FILE, TRUSTED_ROOT_FILE};
use crate::release::Error;

/// Where the deb and the rpm install `gh`: `hostkeeper_daemon::cli::GH_BINARY`.
pub(crate) const INSTALLED_GH: &str = "/usr/lib/hostkeeper/gh";

/// Where they install the trusted root: `hostkeeper_daemon::cli::SIGSTORE_TRUSTED_ROOT`.
pub(crate) const INSTALLED_TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";

/// Each checked file: where it is installed, the staged file it must equal, its mode.
const CHECKED: [(&str, &str, u32); 2] = [
    (INSTALLED_GH, GH_FILE, 0o755),
    (INSTALLED_TRUSTED_ROOT, TRUSTED_ROOT_FILE, 0o644),
];

/// The file-type bits of a mode, and those of a regular file.
const TYPE_BITS: u32 = 0o170_000;
const REGULAR: u32 = 0o100_000;

/// A file of the package as it is installed.
#[derive(Debug, PartialEq, Eq)]
struct Installed {
    /// With its type bits.
    mode: u32,
    owner: String,
    group: String,
    data: Vec<u8>,
}

/// Why the package cannot be read.
fn bad(why: impl std::fmt::Display) -> Error {
    Error::Failed(format!("not a package this check reads: {why}"))
}

/// `len` bytes of `bytes` at `at`.
fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| bad(format!("cut short: {len} bytes at offset {at}")))
}

fn be_u32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let b = take(bytes, at, 4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A whole gzip stream, decoded.
fn gunzip(bytes: &[u8], what: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| bad(format!("{what}: {e}")))?;
    Ok(out)
}

/// The installed path of an archive path: `./usr/x` and `usr/x` are `/usr/x`.
fn installed_path(name: &str) -> String {
    format!("/{}", name.trim_start_matches("./").trim_start_matches('/'))
}

/// The files of [`CHECKED`] a deb holds.
fn deb(bytes: &[u8]) -> Result<BTreeMap<String, Installed>, Error> {
    let mut at = 8;
    let data = loop {
        if at >= bytes.len() {
            return Err(bad("the deb has no data.tar.gz"));
        }
        let header = take(bytes, at, 60)?;
        let text = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range]).trim().to_owned()
        };
        let name = text(0..16);
        let size: usize = text(48..58)
            .parse()
            .map_err(|_| bad(format!("the ar member {name:?} has no size")))?;
        let body = take(bytes, at + 60, size)?;
        match name.trim_end_matches('/') {
            "data.tar.gz" => break gunzip(body, "data.tar.gz")?,
            other if other.starts_with("data.tar") => {
                return Err(bad(format!("{other}: only data.tar.gz is read")));
            }
            _ => at += 60 + size + size % 2,
        }
    };
    let mut found = BTreeMap::new();
    let mut archive = tar::Archive::new(data.as_slice());
    for entry in archive
        .entries()
        .map_err(|e| bad(format!("data.tar: {e}")))?
    {
        let mut entry = entry.map_err(|e| bad(format!("data.tar: {e}")))?;
        let path = installed_path(&String::from_utf8_lossy(&entry.path_bytes()));
        if !CHECKED.iter().any(|(dst, _, _)| *dst == path) {
            continue;
        }
        let header = entry.header();
        let name = |name: Option<&[u8]>, id: u64| match name {
            Some(n) if id == 0 => String::from_utf8_lossy(n).into_owned(),
            _ => format!("id {id}"),
        };
        let kind = if header.entry_type().is_file() {
            REGULAR
        } else {
            0
        };
        let mode = kind | (header.mode().map_err(|e| bad(format!("{path}: {e}")))? & 0o7777);
        let owner = name(header.username_bytes(), header.uid().unwrap_or(1));
        let group = name(header.groupname_bytes(), header.gid().unwrap_or(1));
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| bad(format!("{path}: {e}")))?;
        found.insert(
            path,
            Installed {
                mode,
                owner,
                group,
                data,
            },
        );
    }
    Ok(found)
}

/// An rpm header at `at`: its tags (`tag` to type, count and data) and where it ends.
struct Header<'a> {
    tags: BTreeMap<u32, (u32, usize, &'a [u8])>,
    end: usize,
}

const HEADER_MAGIC: [u8; 4] = [0x8e, 0xad, 0xe8, 0x01];

fn header(bytes: &[u8], at: usize) -> Result<Header<'_>, Error> {
    if take(bytes, at, 4)? != HEADER_MAGIC {
        return Err(bad(format!("no rpm header at offset {at}")));
    }
    let entries = be_u32(bytes, at + 8)? as usize;
    let size = be_u32(bytes, at + 12)? as usize;
    let store_at = at + 16 + entries * 16;
    let store = take(bytes, store_at, size)?;
    let mut tags = BTreeMap::new();
    for n in 0..entries {
        let e = at + 16 + n * 16;
        let (tag, kind) = (be_u32(bytes, e)?, be_u32(bytes, e + 4)?);
        let (offset, count) = (
            be_u32(bytes, e + 8)? as usize,
            be_u32(bytes, e + 12)? as usize,
        );
        let data = store
            .get(offset..)
            .ok_or_else(|| bad(format!("rpm tag {tag} points past its header")))?;
        tags.insert(tag, (kind, count, data));
    }
    Ok(Header {
        tags,
        end: store_at + size,
    })
}

const STRING: u32 = 6;
const STRING_ARRAY: u32 = 8;
const INT16: u32 = 3;
const INT32: u32 = 4;

impl Header<'_> {
    fn tag(&self, tag: u32, kind: u32) -> Result<(usize, &[u8]), Error> {
        match self.tags.get(&tag) {
            Some((k, count, data)) if *k == kind => Ok((*count, data)),
            _ => Err(bad(format!(
                "the rpm header has no tag {tag} of type {kind}"
            ))),
        }
    }

    fn strings(&self, tag: u32) -> Result<Vec<String>, Error> {
        let (count, data) = self.tag(tag, STRING_ARRAY)?;
        let found: Vec<String> = data
            .split(|b| *b == 0)
            .take(count)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        if found.len() < count {
            return Err(bad(format!(
                "rpm tag {tag} holds fewer than {count} strings"
            )));
        }
        Ok(found)
    }

    fn string(&self, tag: u32) -> Result<String, Error> {
        let (_, data) = self.tag(tag, STRING)?;
        let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
        Ok(String::from_utf8_lossy(&data[..end]).into_owned())
    }

    fn numbers(&self, tag: u32, kind: u32) -> Result<Vec<u32>, Error> {
        let (count, data) = self.tag(tag, kind)?;
        let width = if kind == INT16 { 2 } else { 4 };
        (0..count)
            .map(|n| {
                let b = take(data, n * width, width)?;
                Ok(if kind == INT16 {
                    u32::from(u16::from_be_bytes([b[0], b[1]]))
                } else {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
                })
            })
            .collect()
    }
}

const FILEMODES: u32 = 1030;
const FILEUSERNAME: u32 = 1039;
const FILEGROUPNAME: u32 = 1040;
const DIRINDEXES: u32 = 1116;
const BASENAMES: u32 = 1117;
const DIRNAMES: u32 = 1118;
const PAYLOADCOMPRESSOR: u32 = 1125;

/// The regular files of a newc `cpio` archive, by installed path.
fn cpio(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut files = BTreeMap::new();
    let mut at = 0;
    loop {
        let head = take(bytes, at, 110)?;
        if &head[..6] != b"070701" {
            return Err(bad(format!("no cpio newc header at payload offset {at}")));
        }
        let field = |n: usize| {
            let hex = String::from_utf8_lossy(&head[6 + n * 8..14 + n * 8]).into_owned();
            usize::from_str_radix(&hex, 16).map_err(|_| bad(format!("cpio field {hex:?}")))
        };
        let (size, name_size) = (field(6)?, field(11)?);
        let name = take(bytes, at + 110, name_size.saturating_sub(1))?;
        let name = String::from_utf8_lossy(name).into_owned();
        if name == "TRAILER!!!" {
            return Ok(files);
        }
        let data_at = (at + 110 + name_size).next_multiple_of(4);
        files.insert(installed_path(&name), take(bytes, data_at, size)?.to_vec());
        at = (data_at + size).next_multiple_of(4);
    }
}

/// The files of [`CHECKED`] an rpm holds.
fn rpm(bytes: &[u8]) -> Result<BTreeMap<String, Installed>, Error> {
    let signature = header(bytes, 96)?;
    let main = header(bytes, signature.end.next_multiple_of(8))?;
    let compressor = main.string(PAYLOADCOMPRESSOR)?;
    if compressor != "gzip" {
        return Err(bad(format!(
            "the rpm payload is {compressor}: only gzip is read"
        )));
    }
    let payload = cpio(&gunzip(&bytes[main.end..], "the rpm payload")?)?;
    let dirs = main.strings(DIRNAMES)?;
    let bases = main.strings(BASENAMES)?;
    let indexes = main.numbers(DIRINDEXES, INT32)?;
    let modes = main.numbers(FILEMODES, INT16)?;
    let owners = main.strings(FILEUSERNAME)?;
    let groups = main.strings(FILEGROUPNAME)?;
    let mut found = BTreeMap::new();
    for (n, base) in bases.iter().enumerate() {
        let dir = indexes
            .get(n)
            .and_then(|i| dirs.get(*i as usize))
            .ok_or_else(|| bad(format!("rpm file {base:?} has no directory")))?;
        let path = format!("{dir}{base}");
        if !CHECKED.iter().any(|(dst, _, _)| *dst == path) {
            continue;
        }
        let (Some(mode), Some(owner), Some(group)) = (modes.get(n), owners.get(n), groups.get(n))
        else {
            return Err(bad(format!("rpm file {path} has no mode, owner or group")));
        };
        found.insert(
            path.clone(),
            Installed {
                mode: *mode,
                owner: owner.clone(),
                group: group.clone(),
                data: payload.get(&path).cloned().unwrap_or_default(),
            },
        );
    }
    Ok(found)
}

/// The problems of `found` against the files staged in `staged`, one line each.
fn problems(found: &BTreeMap<String, Installed>, staged: &Path) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for (dst, file, mode) in CHECKED {
        let path = staged.join(file);
        let want = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        let Some(got) = found.get(dst) else {
            out.push(format!("{dst}: not in the package"));
            continue;
        };
        if got.mode & TYPE_BITS != REGULAR {
            out.push(format!("{dst}: not a regular file"));
        }
        if got.mode & 0o7777 != mode {
            out.push(format!("{dst}: mode {:o}, not {mode:o}", got.mode & 0o7777));
        }
        if (got.owner.as_str(), got.group.as_str()) != ("root", "root") {
            out.push(format!(
                "{dst}: owned by {}:{}, not root:root",
                got.owner, got.group
            ));
        }
        if got.data != want {
            out.push(format!(
                "{dst}: its bytes are not those of {}",
                path.display()
            ));
        }
    }
    Ok(out)
}

/// Checks the package at `package` against the files staged in `staged`: the lines to print.
pub(crate) fn check(package: &Path, staged: &Path) -> Result<Vec<String>, Error> {
    let bytes = std::fs::read(package).map_err(Error::io(format!("read {}", package.display())))?;
    let found = if bytes.starts_with(b"!<arch>\n") {
        deb(&bytes)?
    } else if bytes.starts_with(&[0xed, 0xab, 0xee, 0xdb]) {
        rpm(&bytes)?
    } else {
        return Err(bad(format!(
            "{} is neither a deb nor an rpm",
            package.display()
        )));
    };
    let problems = problems(&found, staged)?;
    if !problems.is_empty() {
        let name = package.display();
        return Err(Error::Refused(
            problems
                .into_iter()
                .map(|p| format!("{name}: {p}"))
                .collect(),
        ));
    }
    Ok(CHECKED
        .iter()
        .map(|(dst, _, mode)| {
            format!(
                "{}: {dst} is {mode:o} root:root and the staged bytes",
                package.display()
            )
        })
        .collect())
}

#[cfg(test)]
mod tests;
