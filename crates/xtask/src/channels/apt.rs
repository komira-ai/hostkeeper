//! The apt channel: `<package>_<version>_{amd64,arm64}.deb` of each package of the product
//! ([`super::spec`]) into the apt repository the organisation's products share,
//! `packages.komira.ai`. `.github/workflows/release.yml` runs these verbs in three jobs, split so
//! that no job holds both the signing key and the cloud role. The text the verbs write is the form
//! the other products' tools write into the same repository.
//!
//! Every verb but `sign` takes `--suite stable|testing|testing-fail` (default `stable`): a release
//! is published into `testing` first, and `promote` lists the same packages in `stable` once the
//! checks passed. `testing-fail` lists hello's failing build alone, under the Debian version
//! `X.Y.Z~fail.ready`, and is never promoted; its pool files are named by that version
//! (`hello_X.Y.Z~fail.ready_<arch>.deb`) and read from the release's assets named by the service
//! version (`hello_X.Y.Z-fail-ready_<arch>.deb`, [`package_files`]).
//!
//! - `channels apt read --bucket <b> --out <dir>` ([`read`]): the current index and the ETag of
//!   each of its objects, from S3 (never through the CDN, which caches the index and would hand
//!   back a stale ETag). Jobs `testing-apt-read`, `testing-fail-apt-read` and
//!   `stable-apt-read`: the cloud role, no key.
//! - `channels apt index --current <dir> --out <dir> --debs <dir> --version <V> ...`
//!   ([`build`]): the packages merged into that index, every other product's entries kept as they
//!   are. Reads no cloud and no key.
//! - `channels apt promote --testing <dir> --stable <dir> --out <dir> --version <V> --sums <file>
//!   ...` ([`promote`]): the entries of a release `testing` lists, merged into the index of
//!   `stable`; reads no cloud and no key. The write that follows is `apt write --suite stable
//!   --no-pool`.
//! - `channels apt sign --dir <dir> --work <dir> --fingerprint <fpr>` ([`sign`]):
//!   `InRelease` and `Release.gpg`, verified; the key and passphrase come from `APT_GPG_PRIVATE_KEY`
//!   and `APT_GPG_PASSPHRASE`. Jobs `testing-apt-index`, `testing-fail-apt-index` and `stable-apt-index`: the key,
//!   no cloud role.
//! - `channels apt write --bucket <b> --distribution-id <id> --hostname <h> --next <dir> --debs
//!   <dir> --version <V>` ([`write`]): the packages into the pool with `If-None-Match`, the index
//!   with `If-Match` on the ETags that were read (`--no-pool` instead of `--debs` and `--version`:
//!   the index only). Jobs `testing-apt-write`, `testing-fail-apt-write` and `stable-apt-write`: the cloud
//!   role, no key.
//!
//! The programs that talk to the outside (`aws`, `gpg`) are flags, so a test hands a stand-in
//! ([`super::aws::Aws`], [`sign::Gpg`]).
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/apt.rs`), adapted to this repository (see [`crate::channels`]).
//! This repository added the suite `testing-fail` and the package files it takes.

pub mod build;
pub mod control;
pub mod deb;
pub mod index;
pub mod promote;
pub mod read;
pub mod sign;
pub mod state;
pub mod write;

#[cfg(test)]
pub(crate) mod testing;

use std::path::{Path, PathBuf};

use clap::Subcommand;

pub use state::Suite;

use super::aws::{Answer, ExecAws};
use super::{Error, is_plain_version, spec};
use crate::release::ports::Exec;

/// The `channels apt` verbs.
#[derive(Subcommand)]
pub enum Verb {
    /// The current index and the ETag of each of its objects, from S3.
    Read {
        /// The suite of the repository: `stable`, `testing`, where a release is published
        /// before its install checks and promoted from, or `testing-fail`, which lists hello's
        /// failing build alone.
        #[arg(long, value_enum, default_value_t = Suite::Stable)]
        suite: Suite,
        /// The apt repository's bucket.
        #[arg(long)]
        bucket: String,
        /// Where the copy and `etags.txt` are written.
        #[arg(long)]
        out: PathBuf,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
    /// The packages merged into the index that was read.
    Index {
        /// The suite of the repository: `stable`, `testing`, where a release is published
        /// before its install checks and promoted from, or `testing-fail`, which lists hello's
        /// failing build alone.
        #[arg(long, value_enum, default_value_t = Suite::Stable)]
        suite: Suite,
        /// The directory `apt read` wrote.
        #[arg(long)]
        current: PathBuf,
        /// Where the new unsigned index and `etags.txt` are written.
        #[arg(long)]
        out: PathBuf,
        /// The release's files (the `release-assets` artifact).
        #[arg(long)]
        debs: PathBuf,
        /// `<major>.<minor>.<patch>`.
        #[arg(long)]
        version: String,
        /// The `Origin` of a first `Release`; an existing one keeps its own.
        #[arg(long)]
        origin: String,
        /// The `Label` of a first `Release`.
        #[arg(long)]
        label: String,
        /// The `Description` of a first `Release`.
        #[arg(long)]
        description: String,
        /// The `Date` of the `Release`, seconds since the epoch.
        #[arg(long)]
        date: u64,
        /// How many versions of a package and architecture the index keeps.
        #[arg(long, default_value_t = build::KEEP)]
        keep: usize,
    },
    /// `InRelease` and `Release.gpg`, verified.
    Sign {
        /// `dists/<suite>` of the new index.
        #[arg(long)]
        dir: PathBuf,
        /// Where the key files and the private keyring are made; they are removed at the end.
        #[arg(long)]
        work: PathBuf,
        /// The signing subkey's fingerprint, or several separated by commas (a key rotation).
        #[arg(long)]
        fingerprint: String,
        /// The `gpg` program.
        #[arg(long, default_value = "gpg")]
        gpg: PathBuf,
        /// Append `signed=true` to this file (`GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// The entries of a release that `testing` lists, merged into the index of `stable`.
    Promote {
        /// The directory `apt read --suite testing` wrote.
        #[arg(long)]
        testing: PathBuf,
        /// The directory `apt read --suite stable` wrote.
        #[arg(long)]
        stable: PathBuf,
        /// Where the new unsigned index of `stable` and `etags.txt` are written.
        #[arg(long)]
        out: PathBuf,
        /// `<major>.<minor>.<patch>`.
        #[arg(long)]
        version: String,
        /// The release's `SHA256SUMS`: the entries promoted are held to its digests.
        #[arg(long)]
        sums: PathBuf,
        /// The `Origin` of a first `Release`; an existing one keeps its own.
        #[arg(long)]
        origin: String,
        /// The `Label` of a first `Release`.
        #[arg(long)]
        label: String,
        /// The `Description` of a first `Release`.
        #[arg(long)]
        description: String,
        /// The `Date` of the `Release`, seconds since the epoch.
        #[arg(long)]
        date: u64,
        /// How many versions of a package and architecture the index keeps.
        #[arg(long, default_value_t = build::KEEP)]
        keep: usize,
    },
    /// The packages and the signed index into the repository.
    Write {
        /// The suite of the repository: `stable`, `testing`, where a release is published
        /// before its install checks and promoted from, or `testing-fail`, which lists hello's
        /// failing build alone.
        #[arg(long, value_enum, default_value_t = Suite::Stable)]
        suite: Suite,
        /// The apt repository's bucket.
        #[arg(long)]
        bucket: String,
        /// The CDN distribution to invalidate `/apt/dists/*` in.
        #[arg(long)]
        distribution_id: String,
        /// The repository's host name, for the closing message.
        #[arg(long)]
        hostname: String,
        /// The directory `apt index` wrote, signed by `apt sign`.
        #[arg(long)]
        next: PathBuf,
        /// The release's files (the `release-assets` artifact).
        #[arg(long, required_unless_present = "no_pool", conflicts_with = "no_pool")]
        debs: Option<PathBuf>,
        /// `<major>.<minor>.<patch>`.
        #[arg(long, required_unless_present = "no_pool", conflicts_with = "no_pool")]
        version: Option<String>,
        /// Write the index only: the packages are in the pool already (a promotion from
        /// `testing`).
        #[arg(long)]
        no_pool: bool,
        /// This is the last attempt: a lost race fails instead of asking for another.
        #[arg(long)]
        last: bool,
        /// Append `conflict=true` or `conflict=false` to this file (`GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
}

/// Runs a verb: the lines to print.
///
/// # Errors
/// Whatever the verb refuses or fails on.
pub fn run(verb: Verb, exec: &dyn Exec) -> Result<Vec<String>, Error> {
    run_with(verb, exec, &sign::secrets_from_env)
}

/// [`run`], getting the signing key files below `--work` from `secrets` (the tests write fixed
/// strings in place of the environment).
pub(crate) fn run_with(
    verb: Verb,
    exec: &dyn Exec,
    secrets: &dyn Fn(&Path) -> Result<sign::Secrets, Error>,
) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Read {
            suite,
            bucket,
            out,
            aws,
        } => read::read(&ExecAws::new(exec, aws), suite, &bucket, &out),
        Verb::Index {
            suite,
            current,
            out,
            debs,
            version,
            origin,
            label,
            description,
            date,
            keep,
        } => build::build(&build::Request {
            suite,
            current: &current,
            out: &out,
            debs: &debs,
            version: &version,
            origin: &origin,
            label: &label,
            description: &description,
            date,
            keep,
        }),
        Verb::Sign {
            dir,
            work,
            fingerprint,
            gpg,
            output,
        } => {
            let secrets = secrets(&work)?;
            sign::sign(
                &sign::ExecGpg::new(exec, gpg),
                &sign::Request {
                    dir: &dir,
                    work: &work,
                    fingerprints: &fingerprint,
                    key_file: &secrets.key_file,
                    passphrase_file: &secrets.passphrase_file,
                },
                output.as_deref(),
            )
        }
        Verb::Promote {
            testing,
            stable,
            out,
            version,
            sums,
            origin,
            label,
            description,
            date,
            keep,
        } => promote::promote(&promote::Request {
            testing: &testing,
            stable: &stable,
            out: &out,
            version: &version,
            sums: &sums,
            origin: &origin,
            label: &label,
            description: &description,
            date,
            keep,
        }),
        Verb::Write {
            suite,
            bucket,
            distribution_id,
            hostname,
            next,
            debs,
            version,
            no_pool,
            last,
            output,
            aws,
        } => {
            let pool = match (&debs, &version, no_pool) {
                (Some(debs), Some(version), false) => Some(write::Pool { debs, version }),
                (None, None, true) => None,
                _ => {
                    return Err(Error::Usage(
                        "give --debs and --version, or --no-pool and neither".to_owned(),
                    ));
                }
            };
            write::write(
                &ExecAws::new(exec, aws),
                &write::Request {
                    suite,
                    bucket: &bucket,
                    distribution_id: &distribution_id,
                    hostname: &hostname,
                    next: &next,
                    pool,
                    last,
                },
                output.as_deref(),
            )
        }
    }
}

/// What an `aws` call that failed was, from its standard error.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The object is not there (`NoSuchKey`).
    NotFound,
    /// S3 answers `AccessDenied` for a key that does not exist when the caller may not list the
    /// bucket, as the publish role may not.
    Denied,
    /// A conditional write lost: the version it named is not the current one (`412`), or another
    /// write to the same key is in flight (`409`).
    Precondition,
    /// Anything else.
    Other,
}

/// What a failed `aws` call was.
pub fn classify(stderr: &str) -> Failure {
    let has = |words: &[&str]| words.iter().any(|w| stderr.contains(w));
    if has(&["PreconditionFailed", "ConditionalRequestConflict", "(412)"]) {
        Failure::Precondition
    } else if has(&["NoSuchKey", "(404)", "Not Found"]) {
        Failure::NotFound
    } else if has(&["AccessDenied", "(403)", "Forbidden"]) {
        Failure::Denied
    } else {
        Failure::Other
    }
}

/// `aws <args>` as the verbs ask it.
pub(crate) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// The words of a failed call for a message.
pub(crate) fn why(answer: &Answer) -> String {
    answer.stderr.trim().to_owned()
}

/// The packages of `version` that `suite` lists, `(pool file name, release asset name)`, sorted:
/// every package of the product for every Debian architecture in `stable` and `testing`, where
/// both names are `<package>_<version>_<arch>.deb`; hello's failing build alone in `testing-fail`
/// ([`spec::Product::fail_ready_debs`]).
///
/// # Errors
/// A version that is not `<major>.<minor>.<patch>`.
fn files_of(version: &str, suite: Suite) -> Result<Vec<(String, String)>, Error> {
    if !is_plain_version(version) {
        return Err(Error::Usage(format!(
            "--version {version:?} is not <major>.<minor>.<patch>"
        )));
    }
    Ok(match suite {
        Suite::Stable | Suite::Testing => spec::HOSTKEEPER
            .deb_files(version)
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect(),
        Suite::TestingFail => spec::HOSTKEEPER.fail_ready_debs(version),
    })
}

/// The pool file names of the packages of release `version` in `suite`, sorted.
///
/// # Errors
/// A version that is not `<major>.<minor>.<patch>`.
pub fn package_names(version: &str, suite: Suite) -> Result<Vec<String>, Error> {
    Ok(files_of(version, suite)?
        .into_iter()
        .map(|(pool, _)| pool)
        .collect())
}

/// The package files of release `version` in `dir` that `suite` lists, `(pool file name, path)`,
/// sorted by pool name. The path is the release asset: its name is the pool name, but for the
/// failing build, whose asset is named by the service version.
///
/// # Errors
/// A version that is not `<major>.<minor>.<patch>`, or a package that is not there.
pub fn package_files(
    dir: &Path,
    version: &str,
    suite: Suite,
) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut files = Vec::new();
    for (pool, asset) in files_of(version, suite)? {
        let path = dir.join(&asset);
        if !path.is_file() {
            return Err(Error::Failed(format!(
                "{asset} is not in {}",
                dir.display()
            )));
        }
        files.push((pool, path));
    }
    Ok(files)
}

#[cfg(test)]
mod fail_tests;
#[cfg(test)]
mod tests;
