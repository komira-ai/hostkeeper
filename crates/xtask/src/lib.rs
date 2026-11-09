//! `cargo xtask <verb>`: the CI steps of this repository, as Rust with tests, so a workflow step
//! is one command.
//!
//! - `install-tool`: check a downloaded tool against its pinned SHA-256 and install it
//!   (`install`).
//! - `release <verb>`: the release steps: the version of a commit, the hostkeeper package, the
//!   release plan, the check and publication of a release's assets, the staging of one
//!   version of a service into a release channel's tree, and the sync of new published releases
//!   into that tree (`release`).
//! - `advisories`: the security advisories against the dependencies in `Cargo.lock`, kept in
//!   one GitHub issue that it opens, rewrites and closes (`advisories`).
//! - `mdm-signing-test <verb>`: the device test of pkg signing of the MDM design (task M11): the
//!   kit's pkgs, built on a Mac, and the sends of its steps to an enrolled Mac through NanoHUB,
//!   with what each did (`mdm_signing_test`). Not a CI step: an operator runs it
//!   (`docs/operations/pkg-signing-test.md`).
//! - `netinstall-report`: the measurement of network install (item N11 of
//!   `docs/design/network-install.md`): one machine's events from power-on to serving and the
//!   intervals between them, from the times `hostkeeper-pxe` and the coordinator recorded
//!   (`netinstall_report`). The end-to-end job `net-install` runs it; an operator
//!   runs it once on the first real machine (`docs/operations/network-install.md`).
//! - `channels <verb>`: a release's Linux packages (deb and rpm, the daemon's and the
//!   coordinator's) and its macOS installer (signed by the release, not notarized) reaching the repositories at
//!   `packages.komira.ai`: testing first, the install checks, then the decision to promote and
//!   the promotion. Copied from komira-ai/tierstore (`channels`).
//! - `db-backup`: one dump of the coordinator's database into a directory of dumps, keeping the
//!   newest of them (`db_backup`). Not a CI step: the backup timer of the compose deployment runs
//!   it (`docs/deploy/coordinator/`).
//!
//! The workflows that call them are in `.github/workflows`; `tests/workflows` holds each of
//! them to the rules of this repository. The crate's one export is [`main`], the entry point of
//! the binary; with the feature `test-support`, which only this crate's own tests turn on, it
//! also exports `test_support`, what its integration tests need of its internals.

mod advisories;
mod channels;
mod db_backup;
mod install;
mod mdm_signing_test;
mod netinstall_report;
mod release;

#[cfg(feature = "test-support")]
pub use release::test_support;

#[cfg(test)]
mod testdir;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Why a verb failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Install(#[from] install::Error),
    /// Flags that do not fit together (exit status 2, like a clap error).
    #[error(transparent)]
    Usage(#[from] install::UnpackFlags),
    #[error(transparent)]
    Release(#[from] release::Error),
    #[error(transparent)]
    Advisories(#[from] advisories::Error),
    #[error(transparent)]
    SigningTest(#[from] mdm_signing_test::Error),
    #[error(transparent)]
    NetinstallReport(#[from] netinstall_report::Error),
    #[error(transparent)]
    DbBackup(#[from] db_backup::Error),
    #[error(transparent)]
    Channels(#[from] channels::Error),
}

#[derive(Parser)]
#[command(name = "cargo xtask", about = "The CI steps of this repository")]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Check a downloaded file against its pinned SHA-256 and install it (mode 0755).
    InstallTool {
        /// The downloaded file (the workflow's `curl --proto =https` step wrote it, or the
        /// actions cache restored it).
        #[arg(long)]
        download: PathBuf,
        /// The SHA-256 of the download (before unpacking), 64 lowercase hex digits.
        #[arg(long)]
        sha256: install::Sha256,
        /// The file to install; its directory is created when missing.
        #[arg(long)]
        out: PathBuf,
        /// How the download is packed (default: it is the file).
        #[arg(long, value_enum)]
        extract: Option<install::Format>,
        /// The file to take out of a tar (`--extract tar.gz`): its path inside the archive.
        #[arg(long)]
        member: Option<PathBuf>,
    },
    /// The release steps: version, package, plan, check and publish.
    Release {
        #[command(subcommand)]
        verb: release::Verb,
    },
    /// Run `cargo deny check advisories` and keep its findings in one issue: open it, rewrite
    /// its body, or comment and close it when none remain. Exits 0 whatever it finds.
    Advisories {
        /// The repository of the issue, `<owner>/<repo>` (on Actions, `github.repository`).
        #[arg(long)]
        repo: release::contract::Repo,
    },
    /// The device test of pkg signing (MDM design, task M11): build the kit on a Mac, send its
    /// steps to an enrolled Mac through NanoHUB, record and report what each did.
    MdmSigningTest {
        #[command(subcommand)]
        verb: mdm_signing_test::Verb,
    },
    /// The measurement of network install: one machine's events from power-on to serving and
    /// the intervals between them, with the person's wait apart from the machine's time, from
    /// the log of `hostkeeper-pxe serve` and the operator API's answers about its request and
    /// host, as canonical JSON. Reads no clock.
    NetinstallReport(netinstall_report::Flags),
    /// One backup of the coordinator's database: run the dump command after `--` with its
    /// output into a new numbered file of `--into` (mode 0600), and keep the newest `--keep`
    /// dumps. A dump that fails or writes nothing is not kept, and removes no older one.
    DbBackup(db_backup::Flags),
    /// A release's Linux packages and macOS installer reaching `packages.komira.ai`: the settings
    /// gate of those jobs, the job's AWS role, the read, merge, sign and write of each
    /// repository, the installer's upload and promotion, the install checks, and the decision to
    /// promote.
    Channels {
        #[command(subcommand)]
        verb: channels::Verb,
    },
}

fn run(cli: Cli) -> Result<Vec<String>, Error> {
    match cli.verb {
        Verb::InstallTool {
            download,
            sha256,
            out,
            extract,
            member,
        } => {
            let unpack = install::Unpack::new(extract, member)?;
            Ok(vec![install::install_tool(
                &download, &sha256, &unpack, &out,
            )?])
        }
        Verb::Release { verb } => Ok(release::run(verb, release::real::connect)?),
        Verb::Advisories { repo } => {
            let adapters = release::real::connect();
            let ports = release::ports::Adapters::ports(&adapters);
            Ok(advisories::sync(ports.advisories, ports.runs, &repo)?)
        }
        Verb::MdmSigningTest { verb } => Ok(mdm_signing_test::run(
            verb,
            &release::real::exec(),
            mdm_signing_test::NanoHubHttp::new,
        )?),
        Verb::NetinstallReport(flags) => Ok(netinstall_report::run(&flags)?),
        Verb::DbBackup(flags) => Ok(db_backup::run(&flags)?),
        Verb::Channels { verb } => Ok(channels::run(verb, &release::real::exec())?),
    }
}

/// The entry point of the `xtask` binary.
pub fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xtask: {e}");
            match e {
                Error::Usage(_) | Error::Channels(channels::Error::Usage(_)) => ExitCode::from(2),
                Error::Install(_)
                | Error::Release(_)
                | Error::Advisories(_)
                | Error::SigningTest(_)
                | Error::NetinstallReport(_)
                | Error::DbBackup(_)
                | Error::Channels(_) => ExitCode::FAILURE,
            }
        }
    }
}
