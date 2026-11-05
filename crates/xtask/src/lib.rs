//! `cargo xtask <verb>`: the CI steps of this repository, as Rust with tests, so a workflow step
//! is one command.
//!
//! - `install-tool`: check a downloaded tool against its pinned SHA-256 and install it
//!   (`install`).
//! - `release <verb>`: the release steps: the version of a commit, the hostkeeper package, the
//!   release plan, and the check and publication of a release's assets (`release`).
//!
//! The workflows that call them are in `.github/workflows`; `tests/workflows` holds each of
//! them to the rules of this repository. The crate's one export is [`main`], the entry point of
//! the binary; with the feature `test-support`, which only this crate's own tests turn on, it
//! also exports `test_support`, what its integration tests need of its internals.

mod install;
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
        /// The downloaded file (the workflow's `curl --proto =https` step wrote it).
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
                Error::Usage(_) => ExitCode::from(2),
                Error::Install(_) | Error::Release(_) => ExitCode::FAILURE,
            }
        }
    }
}
