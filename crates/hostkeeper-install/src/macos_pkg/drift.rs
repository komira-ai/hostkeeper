//! Drift (packages-repositories.md section 4.7): at every heartbeat the observer reads the
//! receipt, `/var/db/receipts/<identifier>.plist`, with no tool run, and the files of the recorded
//! version's tree. It compares each installed file's size, mode and modification time with what
//! it saw last, and hashes against `tree/` only a file whose values changed (and every file once
//! after the daemon starts). A receipt at another version than the record's, a receipt where no
//! record names the package, a record with no receipt, or a payload file missing or with other
//! bytes is drift; the next `install` or `update` corrects it (`drift_repaired`).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::activate::same;
use super::receipt::{self, Receipt};
use super::record::PkgRecord;
use super::TREE;
use crate::host::{self, Host, Kind};

/// What the observer found: the receipt's version and the record's, and the payload files that
/// are missing or differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub found: Option<String>,
    pub recorded: Option<String>,
    pub files: Vec<String>,
}

/// An installed file's size, mode and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    size: u64,
    mode: u32,
    mtime: (i64, i64),
}

/// What the observer saw of each installed file, and whether it was its sealed copy then. One
/// per service, kept between heartbeats; a new one after the daemon starts hashes every file.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    files: BTreeMap<String, (Stat, bool)>,
    hashed: Vec<String>,
}

impl Seen {
    /// The files hashed by the last [`observe`], so a test can tell a hash from a reuse.
    pub fn hashed(&self) -> &[String] {
        &self.hashed
    }
}

/// The drift of the package `identifier` on `host`: `record` is its record, `dir` the recorded
/// version's directory (when it is on the host). `Ok(None)`: none.
pub fn observe(
    host: &Host,
    identifier: &str,
    record: Option<&PkgRecord>,
    dir: Option<&Path>,
    seen: &mut Seen,
) -> io::Result<Option<Drift>> {
    seen.hashed.clear();
    let found = read_receipt(host, identifier)?.map(|r| r.version);
    let recorded = record.filter(|r| !r.pending).map(|r| r.version.clone());
    let mut files = Vec::new();
    if let (Some(dir), Some(_)) = (dir, &recorded) {
        let tree_dir = dir.join(TREE);
        let tree = host::walk(&tree_dir, host.ids())?;
        for (path, meta) in tree.iter().filter(|(_, m)| m.kind != Kind::Dir) {
            if !intact(host, seen, &tree_dir, path, meta.kind)? {
                files.push(path.clone());
            }
        }
        seen.files.retain(|path, _| tree.contains_key(path));
    }
    let pending = record.is_some_and(|r| r.pending);
    match pending || (found == recorded && files.is_empty()) {
        true => Ok(None),
        false => Ok(Some(Drift {
            found,
            recorded,
            files,
        })),
    }
}

/// Whether the installed `path` is its sealed copy, hashing it only when its size, mode or time
/// changed since it was last seen.
fn intact(host: &Host, seen: &mut Seen, tree_dir: &Path, path: &str, kind: Kind) -> io::Result<bool> {
    let real = host.real(path);
    let stat = match fs::symlink_metadata(&real) {
        Ok(meta) => Stat {
            size: meta.size(),
            mode: meta.mode(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            seen.files.remove(path);
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    if let Some((last, ok)) = seen.files.get(path)
        && *last == stat
    {
        return Ok(*ok);
    }
    let ok = same(&tree_dir.join(path.trim_start_matches('/')), &real, kind)?;
    seen.hashed.push(path.to_owned());
    seen.files.insert(path.to_owned(), (stat, ok));
    Ok(ok)
}

/// The receipt file of `identifier`, `None` when there is none.
fn read_receipt(host: &Host, identifier: &str) -> io::Result<Option<Receipt>> {
    match fs::read(host.real(&receipt::receipt_file(identifier))) {
        Ok(bytes) => receipt::parse_receipt_file(&bytes)
            .map(Some)
            .map_err(|e| io::Error::other(format!("the receipt of {identifier} {e}"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
