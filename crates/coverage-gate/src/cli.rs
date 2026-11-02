//! The `coverage-gate` program: prints per-crate line and branch coverage from an lcov
//! tracefile and fails when a crate is under 99%.
//!
//! Exit status: 0 when every crate passes; 1 when the gate fails; 2 when an input cannot be
//! read or parsed (and for usage errors, as clap reports them).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use crate::{gate, lcov};

/// Per-crate coverage table and gate.
#[derive(Debug, Parser)]
#[command(name = "coverage-gate")]
pub struct Cli {
    /// The lcov tracefile `cargo llvm-cov --lcov` wrote.
    #[arg(long)]
    pub lcov: PathBuf,
    /// The workspace root: absolute, spelled as the tracefile's source paths begin, or
    /// relative to the working directory.
    #[arg(long)]
    pub root: PathBuf,
    /// Also append the table to this file (created if absent), as a CI job summary:
    /// `--summary-file "$GITHUB_STEP_SUMMARY"`.
    #[arg(long)]
    pub summary_file: Option<PathBuf>,
}

/// Why the gate could not judge the workspace (exit status 2).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        source: lcov::LcovError,
    },
    #[error("list crates under {path}: {source}")]
    List {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("--root {path}: {source}")]
    Root {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Unattributed(#[from] gate::Unattributed),
    #[error("append to {path}: {source}")]
    Summary {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The entry point of the `coverage-gate` binary.
pub fn main() -> ExitCode {
    match run(&Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("coverage-gate: {e}");
            ExitCode::from(2)
        }
    }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// `root` as the tracefile spells its source paths. `cargo llvm-cov` writes them absolute,
/// under the directory cargo found the workspace in (the process's working directory, symbolic
/// links resolved), so a relative `--root` (`.`, `..`) is resolved the same way. An absolute
/// one is taken as written, as CI passes it.
fn source_root(root: &Path) -> Result<PathBuf, Error> {
    if root.is_absolute() {
        return Ok(root.to_path_buf());
    }
    std::fs::canonicalize(root).map_err(|source| Error::Root {
        path: root.to_path_buf(),
        source,
    })
}

/// Prints the table (and appends it to `--summary-file`); returns whether the gate passed.
pub fn run(cli: &Cli) -> Result<bool, Error> {
    let text = std::fs::read_to_string(&cli.lcov).map_err(|source| Error::Read {
        path: cli.lcov.clone(),
        source,
    })?;
    let files = lcov::parse(&text).map_err(|source| Error::Parse {
        path: cli.lcov.clone(),
        source,
    })?;
    let crates = gate::workspace_crates(&cli.root).map_err(|source| Error::List {
        path: cli.root.clone(),
        source,
    })?;
    let measured = gate::by_crate(&source_root(&cli.root)?, &crates, &files)?;
    let rows = gate::rows(&measured);
    let report = gate::render(&rows);
    print!("{report}");
    if let Some(path) = &cli.summary_file {
        append(path, &report).map_err(|source| Error::Summary {
            path: path.clone(),
            source,
        })?;
    }
    Ok(!rows.iter().any(|r| r.status().fails()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches: an absolute root rewritten (CI passes the checkout path as the tracefile spells
    /// it), and a relative root that does not exist read as some other directory instead of
    /// being named in the error.
    #[test]
    fn source_root_keeps_an_absolute_root_and_names_a_missing_relative_one() {
        let absolute = Path::new("/w/not/resolved");
        assert_eq!(source_root(absolute).expect("absolute"), absolute);
        let missing = Path::new("coverage-gate-no-such-root/crates");
        let e = source_root(missing).expect_err("missing");
        // ENOENT, as `canonicalize` reports a path that does not exist.
        let enoent = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            e.to_string(),
            format!("--root coverage-gate-no-such-root/crates: {enoent}")
        );
    }
}
