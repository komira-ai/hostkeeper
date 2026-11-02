//! `cargo xtask <verb>`: the CI steps of this repository, as Rust with tests, so a workflow step
//! is one command.
//!
//! - [`fetch`]: download a pinned tool, check its SHA-256, install it.
//!
//! The workflows that call them are in `.github/workflows`; `tests/workflows` holds each of
//! them to the rules of this repository.

pub mod fetch;

#[cfg(test)]
mod testdir;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Why a verb failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Fetch(#[from] fetch::Error),
    /// Flags that do not fit together (exit status 2, like a clap error).
    #[error("{0}")]
    Usage(String),
}

#[derive(Parser)]
#[command(name = "cargo xtask", about = "The CI steps of this repository")]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Download a pinned file, check its SHA-256 and install it (mode 0755).
    Fetch {
        /// Where to download it from (HTTPS; redirects must stay on HTTPS).
        #[arg(long)]
        url: String,
        /// The SHA-256 of the download (before unpacking), 64 lowercase hex digits.
        #[arg(long)]
        sha256: fetch::Sha256,
        /// The file to install; its directory is created when missing.
        #[arg(long)]
        out: PathBuf,
        /// How the download is packed (default: it is the file).
        #[arg(long, value_enum)]
        extract: Option<fetch::Format>,
        /// The file to take out of a tar (`--extract tar.gz`): its path inside the archive.
        #[arg(long)]
        member: Option<PathBuf>,
    },
}

fn run(cli: Cli) -> Result<Vec<String>, Error> {
    match cli.verb {
        Verb::Fetch {
            url,
            sha256,
            out,
            extract,
            member,
        } => {
            let unpack = fetch::Unpack::new(extract, member).map_err(Error::Usage)?;
            let roots = fetch::host_roots();
            Ok(vec![fetch::fetch(&url, &sha256, &unpack, &out, &roots)?])
        }
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
                Error::Fetch(_) => ExitCode::FAILURE,
            }
        }
    }
}
