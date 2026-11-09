//! `channels pkg upload`: the macOS installer and its bundle in the directory of its version,
//! `pkg/<product>/<version>/`, the `testing` channel of the installer.
//!
//! Before anything is put, each file's digest must be the one the release's `SHA256SUMS` lists.
//! Each file is put with one `aws s3api put-object ... --if-none-match '*'`, a single-part
//! upload, the only kind the role may make. A file that exists answers `412`; the stored file is
//! then read back, and the answer is a success only if its digest is the local file's: a re-run
//! of the job passes, and a different file at a version's key is never written over (the role
//! cannot, and this refuses to pretend). The `SHA256SUMS` of the directory, which lists exactly
//! the installer and its bundle, goes last. The pointer `stable.json` is not touched: `promote`
//! moves it, after the install check.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/upload.rs`: `upload` with `--no-latest`), adapted to this
//! repository (see [`super`]).

use std::path::Path;

use super::{IMMUTABLE, Request, check_request, collect, content_type, refusal, write_sums};
use crate::channels::apt::{Failure, classify, strings};
use crate::channels::aws::Aws;
use crate::channels::sums::{SUMS, Sha256};
use crate::channels::{Error, read, spec};

/// Puts `path` at `key` once. A `412` passes only if the stored file's digest is `sha256`.
fn put_once(
    aws: &dyn Aws,
    r: &Request,
    key: &str,
    path: &Path,
    sha256: &Sha256,
) -> Result<String, Error> {
    let name = key.rsplit('/').next().unwrap_or(key);
    let put = aws.run(&strings(&[
        "s3api",
        "put-object",
        "--bucket",
        r.bucket,
        "--key",
        key,
        "--body",
        &path.display().to_string(),
        "--if-none-match",
        "*",
        "--content-type",
        content_type(name),
        "--cache-control",
        IMMUTABLE,
    ]))?;
    if put.ok {
        return Ok(format!("{key}: written"));
    }
    if classify(&put.stderr) != Failure::Precondition {
        return Err(refusal(&format!("put {key}"), &put));
    }
    let existing = r.work.join("existing");
    let got = aws.run(&strings(&[
        "s3api",
        "get-object",
        "--bucket",
        r.bucket,
        "--key",
        key,
        &existing.display().to_string(),
    ]))?;
    if !got.ok {
        return Err(refusal(
            &format!("{key} exists, and reading it back failed"),
            &got,
        ));
    }
    let stored = Sha256::of(&read(&existing)?);
    if stored == *sha256 {
        return Ok(format!("{key}: already there with the same bytes"));
    }
    Err(Error::Failed(format!(
        "{key} exists with other bytes (sha256 {stored}, this release's file is {sha256}): a file of a version is never replaced"
    )))
}

/// The `upload` verb: the lines to print.
///
/// # Errors
/// A flag out of its form, a file missing or not the one `SHA256SUMS` lists, a refusal by AWS, or
/// a file of the version that exists with other bytes.
pub(crate) fn upload(aws: &dyn Aws, r: &Request) -> Result<Vec<String>, Error> {
    let (version, _) = check_request(r)?;
    let entries = collect(r.dir, &version)?;
    let sums_file = write_sums(r.work, &entries)?;
    let prefix = spec::HOSTKEEPER.pkg_prefix(&version);
    let mut said = Vec::new();
    for entry in &entries {
        let key = format!("{prefix}{}", entry.name);
        said.push(put_once(aws, r, &key, &entry.path, &entry.sha256)?);
    }
    let digest = Sha256::of(&read(&sums_file)?);
    said.push(put_once(
        aws,
        r,
        &format!("{prefix}{SUMS}"),
        &sums_file,
        &digest,
    )?);
    Ok(said)
}

#[cfg(test)]
mod tests;
