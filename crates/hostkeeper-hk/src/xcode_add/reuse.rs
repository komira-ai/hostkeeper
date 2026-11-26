//! The record that saves an expansion (docs/design/xcode-one-action.md section 2.3 step 5):
//! `~/.hostkeeper/xcode/<sha256>.json`, a cache of a check this program made itself, keyed by
//! the bytes' SHA-256, which the run has just recomputed.
//!
//! [`release_of`] takes the version and the build from a record only when the file is a regular
//! file (not a link) of the user running `hk`, with mode 0600, naming this SHA-256 and size, whose
//! `verdict` is `pass`, and which holds a version and a build that are an Xcode release's. Any
//! other record is ignored, and replaced after a fresh expansion; except that a record of
//! `hk xcode verify` whose verdict is `pass` is never replaced ([`keeps`]). A record of a check a
//! farm Mac made ([`crate::xcode_farm`]) is of that kind, with the same `archive`, `verdict` and
//! `expansion`, so a pass of it is read here like any other: its archive and its version and
//! build are the ones the Mac checked.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use serde_json::Value;

use crate::xcode_push::archive::{is_build, is_version};
use crate::xcode_record::{PASS, VERIFY};

/// The mode a record of the user's own has.
const MODE: u32 = 0o600;

/// The record at `path` as JSON, when it is a file that parses.
fn read(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The text at `pointer` of `record`.
fn text<'a>(record: &'a Value, pointer: &str) -> Option<&'a str> {
    record.pointer(pointer)?.as_str()
}

/// The version and the build that the record at `path` gives for the archive whose SHA-256 and
/// size are `sha256` and `size`, when the record may be used; `uid` is the user running `hk`.
pub(crate) fn release_of(
    path: &Path,
    uid: u32,
    sha256: &str,
    size: u64,
) -> Option<(String, String)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o777 != MODE {
        return None;
    }
    let record = read(path)?;
    if text(&record, "/archive/sha256")? != sha256
        || record.pointer("/archive/size")?.as_u64()? != size
        || text(&record, "/verdict")? != PASS
    {
        return None;
    }
    let version = text(&record, "/expansion/version")?;
    let build = text(&record, "/expansion/build")?;
    (is_version(version.as_bytes()) && is_build(build.as_bytes()))
        .then(|| (version.to_owned(), build.to_owned()))
}

/// Whether the file at `path` is a record of `hk xcode verify` whose verdict is `pass`: the one
/// record `hk xcode add` never replaces.
pub(crate) fn keeps(path: &Path) -> bool {
    read(path).is_some_and(|record| {
        text(&record, "/record") == Some(VERIFY) && text(&record, "/verdict") == Some(PASS)
    })
}
