//! The reader of an rpm: a lead, a signature header, a header and a gzip `cpio` (newc) payload.
//! The mode, the owner and the group rpm installs a file with are the header's (`FILEMODES`,
//! `FILEUSERNAME`, `FILEGROUPNAME`, one per file named by `DIRNAMES`, `BASENAMES` and
//! `DIRINDEXES`); the bytes of a regular file (by its `FILEMODES`) are the payload's, and a
//! link's target is the header's `FILELINKTOS`, never read as bytes. The dependencies are the
//! names of `REQUIRENAME` other than `rpmlib(...)`, which rpm adds for the format features the
//! package uses.

use std::collections::BTreeMap;

use super::{
    Contents, Error, Installed, Kind, REGULAR, SYMLINK, TYPE_BITS, bad, be_u32, gunzip,
    installed_path, take,
};

/// An rpm header at `at`: its tags (`tag` to type, count and data) and where it ends.
struct Header<'a> {
    tags: BTreeMap<u32, (u32, usize, &'a [u8])>,
    end: usize,
}

pub(super) const HEADER_MAGIC: [u8; 4] = [0x8e, 0xad, 0xe8, 0x01];

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

pub(super) const STRING: u32 = 6;
pub(super) const STRING_ARRAY: u32 = 8;
pub(super) const INT16: u32 = 3;
pub(super) const INT32: u32 = 4;

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

pub(super) const FILEMODES: u32 = 1030;
pub(super) const FILEUSERNAME: u32 = 1039;
pub(super) const FILEGROUPNAME: u32 = 1040;
pub(super) const DIRINDEXES: u32 = 1116;
pub(super) const BASENAMES: u32 = 1117;
pub(super) const DIRNAMES: u32 = 1118;
pub(super) const PAYLOADCOMPRESSOR: u32 = 1125;
pub(super) const FILELINKTOS: u32 = 1036;
pub(super) const REQUIRENAME: u32 = 1049;

/// The entries of a newc `cpio` archive, by installed path.
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

/// The files and the dependencies of an rpm. A header without `FILELINKTOS` holds no link, and
/// one without `REQUIRENAME` requires nothing.
pub(super) fn read(bytes: &[u8]) -> Result<Contents, Error> {
    let signature = header(bytes, 96)?;
    let main = header(bytes, signature.end.next_multiple_of(8))?;
    let compressor = main.string(PAYLOADCOMPRESSOR)?;
    if compressor != "gzip" {
        return Err(bad(format!(
            "the rpm payload is {compressor}: only gzip is read"
        )));
    }
    let mut payload = cpio(&gunzip(&bytes[main.end..], "the rpm payload")?)?;
    let dirs = main.strings(DIRNAMES)?;
    let bases = main.strings(BASENAMES)?;
    let indexes = main.numbers(DIRINDEXES, INT32)?;
    let modes = main.numbers(FILEMODES, INT16)?;
    let owners = main.strings(FILEUSERNAME)?;
    let groups = main.strings(FILEGROUPNAME)?;
    let targets = main.strings(FILELINKTOS).unwrap_or_default();
    let requires = if main.tags.contains_key(&REQUIRENAME) {
        main.strings(REQUIRENAME)?
    } else {
        Vec::new()
    };
    let mut files = BTreeMap::new();
    for (n, base) in bases.iter().enumerate() {
        let dir = indexes
            .get(n)
            .and_then(|i| dirs.get(*i as usize))
            .ok_or_else(|| bad(format!("rpm file {base:?} has no directory")))?;
        let path = format!("{dir}{base}");
        let (Some(mode), Some(owner), Some(group)) = (modes.get(n), owners.get(n), groups.get(n))
        else {
            return Err(bad(format!("rpm file {path} has no mode, owner or group")));
        };
        let regular = mode & TYPE_BITS == REGULAR;
        files.insert(
            path.clone(),
            Installed {
                mode: *mode,
                owner: owner.clone(),
                group: group.clone(),
                data: payload
                    .remove(&path)
                    .filter(|_| regular)
                    .unwrap_or_default(),
                target: targets
                    .get(n)
                    .filter(|_| mode & TYPE_BITS == SYMLINK)
                    .cloned(),
            },
        );
    }
    Ok(Contents {
        kind: Kind::Rpm,
        files,
        depends: requires
            .into_iter()
            .filter(|name| !name.starts_with("rpmlib("))
            .collect(),
    })
}
