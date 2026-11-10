//! The reading of the files of a release's directory that `release targets` checks: that a
//! native package is a regular file, and that its attestation bundle lists its SHA-256.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::release::Error;

/// The most bytes of an attestation bundle: the daemon refuses a longer one (`not_attested`).
pub(super) const MAX_BUNDLE_BYTES: u64 = 1 << 20;

/// What is at a path.
enum Found {
    Missing,
    File,
    Other,
}

/// What is at `path`, not following a link.
fn found(path: &Path) -> Result<Found, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Found::File),
        Ok(_) => Ok(Found::Other),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Found::Missing),
        Err(e) => Err(Error::io(format!("read {}", path.display()))(e)),
    }
}

/// At most `cap + 1` bytes of the file at `path`: more than `cap` means it is over the cap.
pub(super) fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, Error> {
    let what = format!("read {}", path.display());
    let file = File::open(path).map_err(Error::io(&what))?;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io(&what))?;
    Ok(bytes)
}

/// The SHA-256 digests of the subjects of the in-toto statement in an attestation bundle, or what
/// the bundle lacks. Nothing here is believed: the signature is the daemon's to check.
fn subjects(bundle: &[u8]) -> Result<Vec<String>, &'static str> {
    let value: Value = serde_json::from_slice(bundle).map_err(|_| "not JSON")?;
    let payload = value
        .pointer("/dsseEnvelope/payload")
        .and_then(Value::as_str)
        .ok_or("no DSSE payload")?;
    let statement: Value = STANDARD
        .decode(payload)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .ok_or("the DSSE payload is not a JSON statement in base64")?;
    Ok(statement
        .get("subject")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|subject| subject.pointer("/digest/sha256")?.as_str())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default())
}

/// What is wrong with `bundle`, the bundle of the package `name` whose SHA-256 is `sha256`, if
/// anything: over the daemon's cap, not a bundle, or not listing that SHA-256.
pub(super) fn bundle_problem(bundle: &[u8], name: &str, sha256: &str) -> Option<String> {
    if bundle.len() as u64 > MAX_BUNDLE_BYTES {
        return Some(format!(
            "longer than {MAX_BUNDLE_BYTES} bytes, the most the daemon reads of a bundle"
        ));
    }
    match subjects(bundle) {
        Ok(list) if list.iter().any(|s| s == sha256) => None,
        Ok(_) => Some(format!("does not list the SHA-256 {sha256} of {name}")),
        Err(why) => Some(format!("not an attestation bundle: {why}")),
    }
}

/// Whether `name` in `from` is a regular file; when not, why, in `problems` (`missing` when
/// nothing is there).
pub(super) fn regular_file(
    from: &Path,
    name: &str,
    missing: &str,
    problems: &mut Vec<String>,
) -> Result<bool, Error> {
    let why = match found(&from.join(name))? {
        Found::File => return Ok(true),
        Found::Missing => missing,
        Found::Other => "not a regular file",
    };
    problems.push(format!("{name}: {why}"));
    Ok(false)
}

#[cfg(test)]
mod tests;
