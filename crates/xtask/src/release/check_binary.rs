//! `release check-binary <binary> <version> <commit>`: runs a built binary with `--version` and
//! refuses it unless it prints exactly the stamp of that version and commit ([`stamp_of`]). The
//! binary's file name says which shipped binary it is (one of [`STAMPED`]: the shipped binaries and
//! `mdm`, the mdm package's program), and so the program
//! name the stamp starts with; any other file name is refused before anything runs. The binary
//! is run through [`VersionOf`], the adapter of the [`RunVersion`] port.

use std::ffi::OsStr;
use std::path::Path;

use super::Error;
use super::contract::{Commit, STAMPED, Version, stamp_of};
use super::ports::{Exec, PortError, RunVersion, Said};

/// Runs a binary with `--version` through an [`Exec`].
pub(crate) struct VersionOf<E> {
    exec: E,
}

impl<E> VersionOf<E> {
    /// The adapter running binaries through `exec`.
    pub(crate) fn new(exec: E) -> VersionOf<E> {
        VersionOf { exec }
    }
}

impl<E: Exec> RunVersion for VersionOf<E> {
    fn version_of(&self, binary: &Path) -> Result<Said, PortError> {
        let program = binary.to_str().ok_or_else(|| {
            PortError::new(
                format!("run {} --version", binary.display()),
                "the path is not UTF-8",
            )
        })?;
        let ran = self
            .exec
            .spawn(program, &["--version".to_owned()])
            .map_err(|e| PortError::new(format!("{program} --version"), e.to_string()))?;
        Ok(Said {
            success: ran.success,
            stdout: String::from_utf8_lossy(&ran.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&ran.stderr).into_owned(),
        })
    }
}

/// Runs `binary`, whose file name is one of [`STAMPED`], with `--version` and checks that it
/// exited successfully and printed that program's stamp of `version` and `commit`, followed by
/// at most one line break.
pub(crate) fn check_binary(
    run: &dyn RunVersion,
    binary: &Path,
    version: &Version,
    commit: &Commit,
) -> Result<Vec<String>, Error> {
    let program = binary
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| STAMPED.contains(name))
        .ok_or_else(|| {
            Error::Failed(format!(
                "{} is not a shipped binary: its file name is none of {STAMPED:?}",
                binary.display()
            ))
        })?;
    let said = run.version_of(binary)?;
    if !said.success {
        return Err(Error::Failed(format!(
            "{} --version failed: {}",
            binary.display(),
            said.stderr.trim()
        )));
    }
    let expected = stamp_of(program, version, commit);
    let printed = said.stdout.strip_suffix('\n').unwrap_or(&said.stdout);
    if printed != expected {
        return Err(Error::Failed(format!(
            "{} --version printed {:?}, not {:?}",
            binary.display(),
            said.stdout,
            expected
        )));
    }
    Ok(vec![format!("{}: {expected}", binary.display())])
}

#[cfg(test)]
mod tests;
