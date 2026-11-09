//! The macOS installer channel: the installers of a release, `hostkeeper-<version>-macos-arm64.pkg`
//! and hello's `hello-<version>-macos-arm64.pkg` ([`spec::Product::pkg_files`]), each with its
//! attestation bundle, on `packages.komira.ai`, in the bucket of the apt and rpm repositories.
//!
//! The installers are **not signed** (no Developer ID signature, no notarization) until the owner
//! has the certificate; nothing in these verbs signs it, and a user who installs it gets
//! Gatekeeper's warning. What vouches for it is the release's `SHA256SUMS` and the build
//! provenance bundle published beside it.
//!
//! Two channels, as for the Linux packages:
//!
//! - **testing** is the directory `pkg/hostkeeper/<version>/` ([`spec::Product::pkg_prefix`]):
//!   the installers, the `.sigstore.json` bundle of each, and a `SHA256SUMS` of exactly these
//!   files. A Mac's store of this channel is rooted at `pkg/` and fetches an installer by
//!   `hostkeeper/<version>/<file>` ([`spec::Product::pkg_locator`]), the location of hello's
//!   `macos-pkg` entry (`release targets`).
//!   `channels pkg upload` puts each file once, with `If-None-Match: *`, so a version's file is
//!   never replaced. It never touches the pointer.
//! - **stable** is the pointer `pkg/hostkeeper/stable.json` ([`spec::Product::pkg_stable_key`]),
//!   `{"version","tag","commit","files":{<name>:{"sha256","size"}}}`. `channels pkg promote`
//!   reads the directory back from the bucket, holds every file to the digests of the release's
//!   `SHA256SUMS`, and only then moves the pointer, with `If-Match` on the ETag it read, and only
//!   to a higher version. Then the CDN is told that one path changed.
//!
//! `channels pkg check` is the install check of the installers: it downloads each installer, its
//! bundle and the directory's `SHA256SUMS` from the public host over HTTPS and holds them to the
//! release's `SHA256SUMS`.
//!
//! - `channels pkg upload --dir <dir> --tag <tag> --commit <sha> --bucket <b> --work <dir>`
//!   ([`upload`]). Job `testing-pkg-upload`: the cloud role, no key.
//! - `channels pkg promote --dir <dir> --tag <tag> --commit <sha> --bucket <b> --work <dir>
//!   [--distribution-id <id>] [--attempts <n>]` ([`promote`]). Job `promote-pkg`.
//! - `channels pkg check --sums <file> --version <V> --url <url> --work <dir>` ([`check`]). Job
//!   `check-pkg`.
//!
//! The programs that talk to the outside (`aws`, `curl`) are flags or ports, so a test hands a
//! stand-in ([`super::aws::Aws`], [`super::host::Host`]).
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/upload.rs`: `upload --no-latest` and `latest`), with `releases/`,
//! `latest.json` and the files of a version replaced by the installer's own, and the download
//! check added.

pub(crate) mod check;
pub(crate) mod promote;
pub(crate) mod upload;

#[cfg(test)]
pub(crate) mod testing;

use std::path::{Path, PathBuf};

use clap::Subcommand;

use super::aws::{Answer, ExecAws, is_bucket, is_distribution_id};
use super::host::ExecHost;
use super::sums::{self, SUMS, Sha256};
use super::{Error, is_plain_version, numbers, read, spec};
use crate::release::contract::Commit;
use crate::release::ports::Exec;

/// How often the pointer is read and decided again after a concurrent write, by default.
pub(crate) const ATTEMPTS: u32 = 3;

/// The cache header of a file of a version: it never changes under its URL.
pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The cache header of the pointer: a promotion is seen within a minute.
pub(crate) const POINTER_CACHE: &str = "public, max-age=60";

/// The suffix of a Sigstore bundle's file name.
pub(crate) const BUNDLE_SUFFIX: &str = ".sigstore.json";

/// The `channels pkg` verbs.
#[derive(Subcommand)]
pub(crate) enum Verb {
    /// The installer and its bundle into `pkg/<product>/<version>/`, each once.
    Upload {
        /// The release's files (the `release-assets` artifact).
        #[arg(long)]
        dir: PathBuf,
        /// `v<major>.<minor>.<patch>`.
        #[arg(long)]
        tag: String,
        /// The release's commit, 40 hex digits.
        #[arg(long)]
        commit: String,
        /// The repository's bucket.
        #[arg(long)]
        bucket: String,
        /// A directory for the files the verb makes.
        #[arg(long)]
        work: PathBuf,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
    /// The pointer to this release, after the bucket is read back against the release's sums.
    Promote {
        /// The release's files (the `release-assets` artifact).
        #[arg(long)]
        dir: PathBuf,
        /// `v<major>.<minor>.<patch>`.
        #[arg(long)]
        tag: String,
        /// The release's commit, 40 hex digits.
        #[arg(long)]
        commit: String,
        /// The repository's bucket.
        #[arg(long)]
        bucket: String,
        /// The CDN distribution to invalidate the pointer in; empty for none.
        #[arg(long, default_value = "")]
        distribution_id: String,
        /// A directory for the files the verb makes.
        #[arg(long)]
        work: PathBuf,
        /// How often the pointer is read and decided again after a concurrent write.
        #[arg(long, default_value_t = ATTEMPTS)]
        attempts: u32,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
    /// The installer as the public host serves it, against the release's sums.
    Check {
        /// The release's `SHA256SUMS`.
        #[arg(long)]
        sums: PathBuf,
        /// `<major>.<minor>.<patch>`.
        #[arg(long)]
        version: String,
        /// The repository host, `https://packages.komira.ai`.
        #[arg(long)]
        url: String,
        /// A directory for the downloads.
        #[arg(long)]
        work: PathBuf,
        /// The `curl` program.
        #[arg(long, default_value = "curl")]
        curl: PathBuf,
    },
}

/// Runs a verb: the lines to print.
///
/// # Errors
/// Whatever the verb refuses or fails on.
pub(crate) fn run(verb: Verb, exec: &dyn Exec) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Upload {
            dir,
            tag,
            commit,
            bucket,
            work,
            aws,
        } => upload::upload(
            &ExecAws::new(exec, aws),
            &Request {
                dir: &dir,
                tag: &tag,
                commit: &commit,
                bucket: &bucket,
                distribution_id: "",
                work: &work,
                attempts: ATTEMPTS,
            },
        ),
        Verb::Promote {
            dir,
            tag,
            commit,
            bucket,
            distribution_id,
            work,
            attempts,
            aws,
        } => promote::promote(
            &ExecAws::new(exec, aws),
            &Request {
                dir: &dir,
                tag: &tag,
                commit: &commit,
                bucket: &bucket,
                distribution_id: &distribution_id,
                work: &work,
                attempts,
            },
        ),
        Verb::Check {
            sums,
            version,
            url,
            work,
            curl,
        } => check::check(
            &ExecHost(exec),
            &check::Request {
                sums: &sums,
                version: &version,
                url: &url,
                work: &work,
                curl: &curl,
            },
        ),
    }
}

/// What `upload` and `promote` are asked.
pub(crate) struct Request<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) tag: &'a str,
    pub(crate) commit: &'a str,
    pub(crate) bucket: &'a str,
    /// The CDN distribution to invalidate the pointer in; empty for none.
    pub(crate) distribution_id: &'a str,
    pub(crate) work: &'a Path,
    pub(crate) attempts: u32,
}

#[derive(Debug)]
/// One file of the version's directory: its name there, where it is, and what it is.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
}

/// The version of `tag` and its numbers, after the flags are held to their forms.
pub(crate) fn check_request(r: &Request) -> Result<(String, (u64, u64, u64)), Error> {
    let version = r
        .tag
        .strip_prefix('v')
        .filter(|v| is_plain_version(v))
        .ok_or_else(|| {
            Error::Usage(format!("--tag {:?} is not v<major>.<minor>.<patch>", r.tag))
        })?;
    r.commit
        .parse::<Commit>()
        .map_err(|e| Error::Usage(format!("--commit {:?}: {e}", r.commit)))?;
    if !is_bucket(r.bucket) {
        return Err(Error::Usage(format!(
            "--bucket {:?} is not a bucket name",
            r.bucket
        )));
    }
    if !r.distribution_id.is_empty() && !is_distribution_id(r.distribution_id) {
        return Err(Error::Usage(format!(
            "--distribution-id {:?} is not a distribution id",
            r.distribution_id
        )));
    }
    if r.attempts == 0 {
        return Err(Error::Usage(
            "--attempts is 0; it must be 1 or more".to_owned(),
        ));
    }
    // `is_plain_version` held above; a number too large for a u64 is the only miss.
    let ours = numbers(version)
        .ok_or_else(|| Error::Usage(format!("--tag {:?} holds a number too large", r.tag)))?;
    Ok((version.to_owned(), ours))
}

/// The installers and their bundles in `dir`, sorted by name, each held to the digest the
/// release's `SHA256SUMS` lists (a bundle is not listed there: it is made after the sums).
pub(crate) fn collect(dir: &Path, version: &str) -> Result<Vec<Entry>, Error> {
    let listed = sums::read_sums(&dir.join(SUMS))?;
    let mut names: Vec<String> = spec::HOSTKEEPER
        .pkg_files(version)
        .into_iter()
        .flat_map(|installer| [format!("{installer}{BUNDLE_SUFFIX}"), installer])
        .collect();
    names.sort();
    let mut problems = Vec::new();
    let mut entries = Vec::new();
    for name in names {
        let path = dir.join(&name);
        if !path.is_file() {
            problems.push(format!("{} is not a file", path.display()));
            continue;
        }
        let bytes = read(&path)?;
        let sha256 = Sha256::of(&bytes);
        let line = listed.iter().find(|l| l.name == name);
        if let Some(line) = line
            && line.sha256 != sha256
        {
            problems.push(format!(
                "{name} is {sha256}, but {SUMS} lists {}",
                line.sha256
            ));
        }
        if line.is_none() && !name.ends_with(BUNDLE_SUFFIX) {
            problems.push(format!("{SUMS} does not list {name}"));
        }
        entries.push(Entry {
            name,
            path,
            sha256,
            size: bytes.len() as u64,
        });
    }
    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(Error::Refused(problems))
    }
}

/// `SHA256SUMS` of the version's directory: exactly `entries`.
pub(crate) fn sums_text(entries: &[Entry]) -> String {
    sums::render(
        &entries
            .iter()
            .map(|e| sums::Line {
                sha256: e.sha256,
                name: e.name.clone(),
            })
            .collect::<Vec<_>>(),
    )
}

/// The `Content-Type` a file of `name` is served as.
pub(crate) fn content_type(name: &str) -> &'static str {
    if name == SUMS {
        "text/plain; charset=utf-8"
    } else if name.ends_with(BUNDLE_SUFFIX) {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

/// A refused `aws` call, as a failure that says what was asked and what AWS answered.
pub(crate) fn refusal(what: &str, answer: &Answer) -> Error {
    Error::Failed(format!("{what}: {}", answer.stderr.trim_end()))
}

/// Writes the files `upload` and `promote` make in `work`: the directory's `SHA256SUMS`. The
/// work directory is made if it is not there.
pub(crate) fn write_sums(work: &Path, entries: &[Entry]) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(work).map_err(Error::spawn(format!("create {}", work.display())))?;
    let path = work.join(SUMS);
    std::fs::write(&path, sums_text(entries))
        .map_err(Error::spawn(format!("write {}", path.display())))?;
    Ok(path)
}

#[cfg(test)]
mod tests;
