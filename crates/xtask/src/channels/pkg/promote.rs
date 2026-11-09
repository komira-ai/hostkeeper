//! `channels pkg promote`: the pointer `pkg/<product>/stable.json` moved to this release, the
//! `stable` channel of the macOS installer.
//!
//! Every file of the version's directory and its `SHA256SUMS` are read from the bucket and held
//! to the digests of the release's `SHA256SUMS` (the files that were published are exactly the
//! ones that were checked; nothing is uploaded again), and only then is the pointer replaced.
//!
//! The pointer is read with its ETag and written with `--if-match` on that ETag, only when its
//! version is lower than this release's (none yet: with `--if-none-match '*'`); a `412` reads it
//! again and decides again, up to `--attempts` times. A re-run of an older release writes
//! nothing; a re-run of the release the pointer names writes nothing either, and invalidates the
//! path again, since the first run may have stopped before it. The path
//! `/pkg/<product>/stable.json` is then invalidated, if `--distribution-id` is given.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/upload.rs`: `latest`), adapted to this repository (see [`super`]).

use std::cmp::Ordering;

use serde_json::{Value, json};

use super::{Entry, POINTER_CACHE, Request, check_request, collect, refusal, write_sums};
use crate::channels::apt::read::etag_of;
use crate::channels::apt::{Failure, classify, strings};
use crate::channels::aws::Aws;
use crate::channels::sums::{SUMS, Sha256};
use crate::channels::{Error, numbers, read, spec};

/// The pointer as stored: its ETag, its version and the version's numbers.
type Stored = (String, String, (u64, u64, u64));

/// The text of the pointer for this release: the files of the directory except `SHA256SUMS`.
fn pointer_text(version: &str, tag: &str, commit: &str, entries: &[Entry]) -> String {
    let files: serde_json::Map<String, Value> = entries
        .iter()
        .map(|e| {
            (
                e.name.clone(),
                json!({"sha256": e.sha256.to_string(), "size": e.size}),
            )
        })
        .collect();
    let pointer = json!({"version": version, "tag": tag, "commit": commit, "files": files});
    format!("{pointer}\n")
}

/// Reads `key` back into `--work` and holds it to `sha256`.
fn check_published(
    aws: &dyn Aws,
    r: &Request,
    key: &str,
    sha256: &Sha256,
) -> Result<String, Error> {
    let held = r.work.join("published");
    let got = aws.run(&strings(&[
        "s3api",
        "get-object",
        "--bucket",
        r.bucket,
        "--key",
        key,
        &held.display().to_string(),
    ]))?;
    if !got.ok {
        return Err(refusal(&format!("{key} could not be read back"), &got));
    }
    let stored = Sha256::of(&read(&held)?);
    if stored != *sha256 {
        return Err(Error::Failed(format!(
            "{key} holds sha256 {stored}, but this release's file is {sha256}"
        )));
    }
    Ok(format!("{key}: sha256 {stored}"))
}

/// The pointer as stored, or none when the bucket has no such key (`NoSuchKey`, or
/// `AccessDenied`: the publish role may not list the bucket, so S3 answers a missing key with
/// `AccessDenied`; a wrong grant shows at the write instead).
fn read_pointer(aws: &dyn Aws, r: &Request, key: &str) -> Result<Option<Stored>, Error> {
    let current = r.work.join("pointer.current");
    let got = aws.run(&strings(&[
        "s3api",
        "get-object",
        "--bucket",
        r.bucket,
        "--key",
        key,
        &current.display().to_string(),
    ]))?;
    if !got.ok {
        return match classify(&got.stderr) {
            Failure::NotFound | Failure::Denied => Ok(None),
            _ => Err(refusal(&format!("read {key}"), &got)),
        };
    }
    let etag = etag_of(&got.stdout)
        .ok_or_else(|| Error::Failed(format!("read {key}: aws printed no ETag")))?;
    let stored: Value = serde_json::from_slice(&read(&current)?)
        .map_err(|e| Error::Failed(format!("{key} in the bucket is not JSON: {e}")))?;
    let (version, theirs) = stored["version"]
        .as_str()
        .and_then(|v| Some((v, numbers(v)?)))
        .ok_or_else(|| {
            Error::Failed(format!(
                "{key} in the bucket has no <major>.<minor>.<patch> version"
            ))
        })?;
    Ok(Some((etag, version.to_owned(), theirs)))
}

/// Invalidates `/<key>`, when a distribution is given.
fn invalidate(aws: &dyn Aws, r: &Request, key: &str) -> Result<Option<String>, Error> {
    if r.distribution_id.is_empty() {
        return Ok(None);
    }
    let path = format!("/{key}");
    let made = aws.run(&strings(&[
        "cloudfront",
        "create-invalidation",
        "--distribution-id",
        r.distribution_id,
        "--paths",
        &path,
    ]))?;
    if !made.ok {
        return Err(refusal(&format!("invalidate {path}"), &made));
    }
    Ok(Some(format!("{path}: invalidated")))
}

/// Replaces the pointer when its version is lower than `version`.
fn replace_pointer(
    aws: &dyn Aws,
    r: &Request,
    key: &str,
    version: &str,
    ours: (u64, u64, u64),
    pointer: &str,
) -> Result<Vec<String>, Error> {
    for _ in 0..r.attempts {
        let stored = read_pointer(aws, r, key)?;
        let mut condition = strings(&["--if-none-match", "*"]);
        if let Some((etag, stored_version, theirs)) = &stored {
            match theirs.cmp(&ours) {
                Ordering::Greater => {
                    return Ok(vec![format!(
                        "{key}: at {stored_version}, newer than {version}: left as it is"
                    )]);
                }
                Ordering::Equal => {
                    let mut said = vec![format!("{key}: already at {version}")];
                    said.extend(invalidate(aws, r, key)?);
                    return Ok(said);
                }
                Ordering::Less => condition = vec!["--if-match".to_owned(), etag.clone()],
            }
        }
        let mut args = strings(&[
            "s3api",
            "put-object",
            "--bucket",
            r.bucket,
            "--key",
            key,
            "--body",
            pointer,
            "--content-type",
            "application/json",
            "--cache-control",
            POINTER_CACHE,
        ]);
        args.extend(condition);
        let put = aws.run(&args)?;
        if put.ok {
            let mut said = vec![format!("{key}: now {version}")];
            said.extend(invalidate(aws, r, key)?);
            return Ok(said);
        }
        if classify(&put.stderr) != Failure::Precondition {
            return Err(refusal(&format!("put {key}"), &put));
        }
    }
    Err(Error::Failed(format!(
        "{key} was written by another release {} times in a row; run the job again",
        r.attempts
    )))
}

/// The `promote` verb: the pointer to this release, after the bucket is read back.
///
/// # Errors
/// A flag out of its form, a file of the release that is missing or not the one `SHA256SUMS`
/// lists, a file of the version that is not in the bucket or holds other bytes, a refusal by AWS,
/// or the pointer changed under every attempt.
pub(crate) fn promote(aws: &dyn Aws, r: &Request) -> Result<Vec<String>, Error> {
    let (version, ours) = check_request(r)?;
    let entries = collect(r.dir, &version)?;
    let sums_file = write_sums(r.work, &entries)?;
    let pointer_file = r.work.join("stable.json");
    std::fs::write(
        &pointer_file,
        pointer_text(&version, r.tag, r.commit, &entries),
    )
    .map_err(Error::spawn(format!("write {}", pointer_file.display())))?;
    let prefix = spec::HOSTKEEPER.pkg_prefix(&version);
    let mut said = Vec::new();
    for entry in &entries {
        said.push(check_published(
            aws,
            r,
            &format!("{prefix}{}", entry.name),
            &entry.sha256,
        )?);
    }
    let digest = Sha256::of(&read(&sums_file)?);
    said.push(check_published(
        aws,
        r,
        &format!("{prefix}{SUMS}"),
        &digest,
    )?);
    said.extend(replace_pointer(
        aws,
        r,
        &spec::HOSTKEEPER.pkg_stable_key(),
        &version,
        ours,
        &pointer_file.display().to_string(),
    )?);
    Ok(said)
}

#[cfg(test)]
mod tests;
