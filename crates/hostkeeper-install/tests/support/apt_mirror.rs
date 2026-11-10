//! The fake apt's read of a mirror on disk: a `file:` URI no test published to is read as apt
//! reads one, from `dists/<suite>/InRelease` and the uncompressed
//! `dists/<suite>/<component>/binary-<arch>/Packages` files below the directory, and a package
//! is downloadable only when the file its stanza's `Filename` names is there with the stanza's
//! SHA-256. The package files' contents come from the fake's own models, found by that digest.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{Deb, Entry, Health, Repo};

/// The directory a `file:` URI names.
fn dir_of(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file:").map(PathBuf::from)
}

/// The sub-directories of `dir`, sorted.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|item| item.path())
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    found
}

/// The repository the directory of `uri` holds: none when `uri` is no `file:` URI or no suite
/// there has an `InRelease`.
pub fn read(uri: &str, debs: &[Deb]) -> Option<Repo> {
    let dir = dir_of(uri)?;
    let mut released = false;
    let mut entries = Vec::new();
    for suite in subdirs(&dir.join("dists")) {
        if !suite.join("InRelease").is_file() {
            continue;
        }
        released = true;
        for component in subdirs(&suite) {
            for binary in subdirs(&component) {
                let Ok(text) = fs::read_to_string(binary.join("Packages")) else {
                    continue;
                };
                entries.extend(
                    text.split("\n\n")
                        .filter_map(|stanza| entry_of(stanza, debs)),
                );
            }
        }
    }
    released.then_some(Repo {
        health: Health::Up,
        entries,
    })
}

/// The value of `key` in `stanza`.
fn field<'a>(stanza: &'a str, key: &str) -> Option<&'a str> {
    stanza
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
}

/// The entry a stanza of an index lists, when the fake holds a model with its SHA-256.
fn entry_of(stanza: &str, debs: &[Deb]) -> Option<Entry> {
    let sha256 = field(stanza, "SHA256")?.to_owned();
    let deb = debs.iter().find(|deb| deb.pin() == sha256)?.clone();
    Some(Entry {
        package: field(stanza, "Package")?.to_owned(),
        version: field(stanza, "Version")?.to_owned(),
        arch: field(stanza, "Architecture")?.to_owned(),
        size: field(stanza, "Size")?.parse().ok()?,
        filename: field(stanza, "Filename").map(str::to_owned),
        sha256,
        deb,
    })
}

/// Whether the directory of `uri` holds the package file `entry` lists, with its SHA-256.
pub fn holds(uri: &str, entry: &Entry) -> bool {
    let (Some(dir), Some(filename)) = (dir_of(uri), entry.filename.as_ref()) else {
        return false;
    };
    fs::read(dir.join(filename))
        .is_ok_and(|bytes| hex::encode(Sha256::digest(bytes)) == entry.sha256)
}
