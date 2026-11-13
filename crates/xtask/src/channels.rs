//! `cargo xtask channels`: a published release reaching the apt and rpm repositories at
//! `packages.komira.ai`, after the GitHub release exists.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels`), with each package's name and paths made data ([`spec`]); to be
//! replaced by ucdk's package publishers. The modules below keep the shape, the names and the
//! protocols of the copy (the `If-Match` ETag protocol, the attempts and their conflict output,
//! `--last`, the number of versions kept, the `Release` fields an existing repository keeps,
//! `by-hash` copies, the invalidations), so a change to the original can be carried over; what
//! differs is listed in each module's documentation, and in [`spec`].
//!
//! - `channels gate --label <job> --setting NAME=VALUE... [--output <file>]` ([`gate`]): whether
//!   a job has the settings it needs. None set: `enabled=false` and a notice, the job ends green
//!   and writes nothing. All set: `enabled=true`. Some set: red, naming the missing ones.
//! - `channels assume --role-arn <arn> --session-name <name> --region <r>
//!   --token-url <url> --env-file <file>`
//!   ([`assume`]): the job's AWS credentials from its OIDC token, into the env file; the step
//!   `release.yml` runs where an action would otherwise do it.
//! - `channels apt read|index|promote|sign|write ...` ([`apt`]): the Debian packages of the
//!   product, the daemon's and the coordinator's together, into the apt repository the
//!   organisation's products share.
//! - `channels rpm sign|check-signed|promote|repo ...` ([`rpm`]): the same packages as RPMs,
//!   signed, into the rpm tree of the same repository.
//! - `channels pkg upload|promote|check ...` ([`pkg`]): the macOS installer (signed by the
//!   release, not notarized) into `pkg/hostkeeper/<version>/` of the same bucket, and the
//!   pointer `pkg/hostkeeper/stable.json` to the promoted one.
//! - `channels descriptor write|check ...` ([`descriptor`]): the descriptor `hostkeeper-repository.json`
//!   of the whole repository, merged with this product's facts, and the served one checked.
//! - `channels check-apt --package <p> ...` ([`check`]) and `channels check-rpm --package <p>
//!   ...` ([`check_rpm`]): a package installed from the suite `testing` on a clean machine and
//!   held to what [`spec::Package`] says it installs.
//! - `channels decide --result <stage>=<result>... [--output <file>]` ([`decide`]): whether the
//!   release goes on from `testing`.
//!
//! The programs that talk to the outside (`aws`, `gpg`, `curl`) run through the crate's
//! [`Exec`](crate::release::ports::Exec) port, which `lib.rs` hands the verbs, so a test hands a
//! stand-in ([`aws::Aws`], [`apt::sign::Gpg`], [`host::Host`]) and no test starts a process.

pub(crate) mod apt;
pub(crate) mod assume;
pub(crate) mod aws;
pub(crate) mod check;
pub(crate) mod check_rpm;
pub(crate) mod decide;
pub(crate) mod descriptor;
pub(crate) mod gate;
pub(crate) mod host;
pub(crate) mod pkg;
pub(crate) mod rpm;
pub(crate) mod spec;
pub(crate) mod sums;

#[cfg(test)]
pub(crate) mod fixtures;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::release::ports::Exec;

/// Why a channel verb failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// A program could not be started, or a file not read or written: what, and the system's
    /// reason.
    #[error("{what}: {why}")]
    Spawn { what: String, why: String },
    /// The verb refuses, for one reason.
    #[error("{0}")]
    Failed(String),
    /// The verb refuses, for every reason listed (one per line).
    #[error("{}", .0.join("\n"))]
    Refused(Vec<String>),
    /// Flags that do not fit together (exit status 2, like a clap error).
    #[error("{0}")]
    Usage(String),
}

impl Error {
    /// The error for a program that could not be started, or a file not read: `what` and the
    /// system's reason.
    pub(crate) fn spawn(what: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
        let what = what.to_string();
        move |e| Error::Spawn {
            what,
            why: e.to_string(),
        }
    }
}

/// Whether `text` is `<major>.<minor>.<patch>`: three whole numbers and nothing else.
pub(crate) fn is_plain_version(text: &str) -> bool {
    let is_number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<&str> = text.split('.').collect();
    (parts.len() == 3) & parts.iter().all(|part| is_number(part))
}

/// `<major>.<minor>.<patch>` as numbers; none for anything else, or a number too large.
pub(crate) fn numbers(version: &str) -> Option<(u64, u64, u64)> {
    if !is_plain_version(version) {
        return None;
    }
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Appends `lines` to `output` (on Actions, `GITHUB_OUTPUT`), one per line.
pub(crate) fn append_output(output: &Path, lines: &[String]) -> Result<(), Error> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output)
        .and_then(|mut f| writeln!(f, "{}", lines.join("\n")))
        .map_err(Error::spawn(format!("append to {}", output.display())))
}

/// Reads `path`, with the path in the error.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(Error::spawn(format!("read {}", path.display())))
}

/// Writes `bytes` to the new file `path`; a file already there is an error.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    std::fs::File::create_new(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(Error::spawn(format!("write {}", path.display())))
}

/// The `channels` verbs.
#[derive(Subcommand)]
pub(crate) enum Verb {
    /// Whether a channel job has its settings: none set is a green no-op, some set is red.
    Gate {
        /// The job, as the notice names it.
        #[arg(long)]
        label: String,
        /// One setting, `NAME=VALUE`; only whether VALUE is empty is read. The workflow passes
        /// `NAME=${VAR:+set}`, so a secret never reaches a command line.
        #[arg(long = "setting", required = true)]
        settings: Vec<String>,
        /// What an earlier job must have said, `NAME=VALUE` with VALUE its output: unless every
        /// one is `true` the job is off and green, and its settings are not judged.
        #[arg(long = "require")]
        requires: Vec<String>,
        /// Append `enabled=true` or `enabled=false` to this file (`GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// The job's AWS credentials from its OIDC token, appended to the env file (`GITHUB_ENV`).
    Assume {
        /// The role to assume.
        #[arg(long)]
        role_arn: String,
        /// The session name AWS records; it starts with the product's prefix.
        #[arg(long)]
        session_name: String,
        /// The region the later steps use.
        #[arg(long)]
        region: String,
        /// The token request URL the runner offers a job with `id-token: write`; the step
        /// passes `"$ACTIONS_ID_TOKEN_REQUEST_URL"`. It must be an `https` URL.
        #[arg(long)]
        token_url: String,
        /// Where the `AWS_*` lines are appended.
        #[arg(long)]
        env_file: PathBuf,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
    /// The apt repository: read the index, merge the packages, sign it, write it.
    Apt {
        #[command(subcommand)]
        verb: apt::Verb,
    },
    /// The rpm repository: sign the packages, read the `repodata`, merge, sign it, write it.
    Rpm {
        #[command(subcommand)]
        verb: rpm::Verb,
    },
    /// The descriptor of the repository (`hostkeeper-repository.json`): this product's facts
    /// merged into it, and the served one checked.
    Descriptor {
        #[command(subcommand)]
        verb: descriptor::Verb,
    },
    /// The macOS installer: upload it to `testing`, check it, promote it to `stable`.
    Pkg {
        #[command(subcommand)]
        verb: pkg::Verb,
    },
    /// A package installed from the apt suite `testing`, on this machine (a clean container),
    /// held to what the package promises.
    CheckApt {
        /// The package: `hostkeeper` or `hostkeeper-coordinator`.
        #[arg(long)]
        package: String,
        /// The Debian architecture of the machine, `amd64` or `arm64`.
        #[arg(long)]
        arch: String,
        /// `<major>.<minor>.<patch>`.
        #[arg(long)]
        version: String,
        /// The release's commit, 40 hex digits.
        #[arg(long)]
        commit: String,
        /// The apt repository.
        #[arg(long, default_value = "https://packages.komira.ai/apt")]
        apt_url: String,
        /// Where the checked system is.
        #[arg(long, default_value = "/")]
        root: PathBuf,
    },
    /// A package installed from the rpm tree `testing` in a container, held to what the package
    /// promises; the programs run with `docker exec`.
    CheckRpm {
        /// The package: `hostkeeper` or `hostkeeper-coordinator`.
        #[arg(long)]
        package: String,
        /// The container, started and left running by the job.
        #[arg(long)]
        container: String,
        /// The RPM architecture of the machine, `x86_64` or `aarch64`.
        #[arg(long)]
        arch: String,
        /// `<major>.<minor>.<patch>`.
        #[arg(long)]
        version: String,
        /// The release's commit, 40 hex digits.
        #[arg(long)]
        commit: String,
        /// The repository host.
        #[arg(long, default_value = "https://packages.komira.ai")]
        url: String,
        /// A directory of the container to download the package to.
        #[arg(long)]
        work: String,
        /// A directory of the runner the repository file is written to before `docker cp` copies
        /// it into the container (the workflow passes `$RUNNER_TEMP`).
        #[arg(long)]
        scratch: PathBuf,
        /// The `docker` program.
        #[arg(long, default_value = "docker")]
        docker: PathBuf,
    },
    /// Whether the release, which is in `testing`, goes on to the public channels.
    Decide {
        /// The result of one stage, `NAME=RESULT` (`needs.<job>.result`); every stage is given.
        #[arg(long = "result")]
        results: Vec<String>,
        /// Append `promote=true` to this file (`GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

/// Runs a verb: the lines to print.
///
/// # Errors
/// Whatever the verb refuses or fails on.
pub(crate) fn run(verb: Verb, exec: &dyn Exec) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Gate {
            label,
            settings,
            requires,
            output,
        } => gate::gate(&label, &settings, &requires, output.as_deref()),
        Verb::Assume {
            role_arn,
            session_name,
            region,
            token_url,
            env_file,
            aws,
        } => assume::assume(
            &aws::ExecAws::new(exec, aws),
            &mut std::io::stdout().lock(),
            &|| assume::oidc_from_env(&token_url),
            &assume::Request {
                role_arn: &role_arn,
                session_name: &session_name,
                region: &region,
                env_file: &env_file,
                https_only: true,
            },
        ),
        Verb::Apt { verb } => apt::run(verb, exec),
        Verb::Rpm { verb } => rpm::run(verb, exec),
        Verb::Descriptor { verb } => descriptor::run(verb, exec),
        Verb::Pkg { verb } => pkg::run(verb, exec),
        Verb::CheckApt {
            package,
            arch,
            version,
            commit,
            apt_url,
            root,
        } => check::check_apt(
            &check::Noninteractive(exec),
            &check::Apt {
                root: &root,
                package: check::package_flag(&package)?,
                arch: &arch,
                version: &version,
                commit: &commit,
                apt_url: &apt_url,
            },
        ),
        Verb::CheckRpm {
            package,
            container,
            arch,
            version,
            commit,
            url,
            work,
            scratch,
            docker,
        } => check_rpm::check_rpm(
            &check_rpm::Container {
                exec,
                docker,
                name: container,
            },
            &check_rpm::Rpm {
                package: check::package_flag(&package)?,
                arch: &arch,
                version: &version,
                commit: &commit,
                url: &url,
                work: &work,
                scratch: &scratch,
            },
        ),
        Verb::Decide { results, output } => {
            decide::decide(&spec::HOSTKEEPER, &results, output.as_deref())
        }
    }
}

#[cfg(test)]
mod tests;
