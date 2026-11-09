//! The readers of the packages that are tars: a deb (an `ar` of `control.tar.gz` and
//! `data.tar.gz`) and a tarball (a gzip, xz or zstd tar).
//!
//! An entry's owner is its tar header's: the name when the uid is 0 (an empty name is root's),
//! and `id N` for any other id.

use std::collections::BTreeMap;
use std::io::Read;

use super::{
    Contents, Error, Installed, Kind, REGULAR, SYMLINK, bad, gunzip, installed_path, take,
};

/// The gzip, xz and zstd magic numbers.
const GZIP: &[u8] = &[0x1f, 0x8b];
const XZ: &[u8] = &[0xfd, b'7', b'z', b'X', b'Z', 0];
const ZSTD: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd];

/// Whether `bytes` start like a compressed tar this check reads.
pub(super) fn compressed(bytes: &[u8]) -> bool {
    [GZIP, XZ, ZSTD]
        .iter()
        .any(|magic| bytes.starts_with(magic))
}

/// The tar inside a gzip, xz or zstd stream.
fn decompress(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let what = "the archive";
    if bytes.starts_with(GZIP) {
        return gunzip(bytes, what);
    }
    let mut out = Vec::new();
    if bytes.starts_with(XZ) {
        lzma_rs::xz_decompress(&mut &bytes[..], &mut out)
            .map_err(|e| bad(format!("{what}: {e}")))?;
    } else {
        ruzstd::decoding::StreamingDecoder::new(bytes)
            .map_err(|e| bad(format!("{what}: {e}")))?
            .read_to_end(&mut out)
            .map_err(|e| bad(format!("{what}: {e}")))?;
    }
    Ok(out)
}

/// Every entry of the tar `data`, by installed path; `what` names the tar in a message.
fn tar_files(data: &[u8], what: &str) -> Result<BTreeMap<String, Installed>, Error> {
    let mut found = BTreeMap::new();
    let mut archive = tar::Archive::new(data);
    for entry in archive.entries().map_err(|e| bad(format!("{what}: {e}")))? {
        let mut entry = entry.map_err(|e| bad(format!("{what}: {e}")))?;
        let path = installed_path(&String::from_utf8_lossy(&entry.path_bytes()));
        let header = entry.header();
        let name = |name: Option<&[u8]>, id: u64| match name {
            Some(n) if id == 0 && !n.is_empty() => String::from_utf8_lossy(n).into_owned(),
            _ if id == 0 => "root".to_owned(),
            _ => format!("id {id}"),
        };
        let kind = if header.entry_type().is_file() {
            REGULAR
        } else if header.entry_type().is_symlink() {
            SYMLINK
        } else {
            0
        };
        let target = header
            .link_name_bytes()
            .filter(|_| kind == SYMLINK)
            .map(|name| String::from_utf8_lossy(&name).into_owned());
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
                target,
            },
        );
    }
    Ok(found)
}

/// The files of a deb, and the dependencies its `control` declares.
pub(super) fn deb(bytes: &[u8]) -> Result<Contents, Error> {
    let (mut data, mut control) = (None, None);
    let mut at = 8;
    while at < bytes.len() {
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
            "data.tar.gz" => data = Some(gunzip(body, "data.tar.gz")?),
            "control.tar.gz" => control = Some(gunzip(body, "control.tar.gz")?),
            other if other.starts_with("data.tar") => {
                return Err(bad(format!("{other}: only data.tar.gz is read")));
            }
            other if other.starts_with("control.tar") => {
                return Err(bad(format!("{other}: only control.tar.gz is read")));
            }
            _ => {}
        }
        at += 60 + size + size % 2;
    }
    let data = data.ok_or_else(|| bad("the deb has no data.tar.gz"))?;
    let files = tar_files(&data, "data.tar")?;
    let control = control.ok_or_else(|| bad("the deb has no control.tar.gz"))?;
    let control = tar_files(&control, "control.tar")?
        .remove("/control")
        .ok_or_else(|| bad("control.tar.gz has no ./control"))?;
    Ok(Contents {
        kind: Kind::Deb,
        files,
        depends: control_depends(&String::from_utf8_lossy(&control.data)),
    })
}

/// The relations of the `Depends` and `Pre-Depends` fields of a deb's `control`. Field names are
/// case-insensitive, a line that starts with a space or a tab continues the field above it, and
/// relations are comma separated (`a | b` is one).
fn control_depends(control: &str) -> Vec<String> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in control.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            fields.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    fields
        .iter()
        .filter(|(name, _)| name == "depends" || name == "pre-depends")
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|relation| !relation.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The files of a tarball, which declares no dependency.
pub(super) fn tarred(bytes: &[u8]) -> Result<Contents, Error> {
    let files = tar_files(&decompress(bytes)?, "the archive")?;
    Ok(Contents {
        kind: Kind::Tarball,
        depends: Vec::new(),
        files,
    })
}
