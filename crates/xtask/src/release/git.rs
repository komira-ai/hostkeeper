//! The git adapter: answers [`GitHistory`] by running `git` through an [`Exec`] in the current
//! directory. Every argument list, every parse and every error is here, so a test scripts the
//! [`Exec`] and checks them all without starting a process.
//!
//! A command that cannot start, or that ends unsuccessfully, is a [`PortError`] whose `what` is
//! the command line and whose `why` is the operating system's text or git's trimmed stderr.

use super::contract::Commit;
use super::ports::{Exec, GitHistory, PortError};

/// `program` and `args` as one command line text.
pub(crate) fn command_line(program: &str, args: &[String]) -> String {
    let mut words = vec![program];
    words.extend(args.iter().map(String::as_str));
    words.join(" ")
}

/// Runs `program` with `args` through `exec` and returns what it printed on stdout. A program
/// that cannot start gives the operating system's text; one that fails gives its stderr, trimmed.
pub(crate) fn stdout_of(
    exec: &dyn Exec,
    program: &str,
    args: &[String],
) -> Result<Vec<u8>, PortError> {
    let line = command_line(program, args);
    let ran = exec
        .spawn(program, args)
        .map_err(|e| PortError::new(line.clone(), e.to_string()))?;
    if ran.success {
        Ok(ran.stdout)
    } else {
        Err(PortError::new(
            line,
            String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
        ))
    }
}

/// The error for the git command with arguments `args`.
fn refuse(args: &[&str], why: impl Into<String>) -> PortError {
    PortError::new(format!("git {}", args.join(" ")), why)
}

/// The object id of the one blob a `git ls-tree -z` listing holds, none when the listing is not
/// exactly `<mode> blob <object>` and a tab and a path, ended by a NUL.
fn blob_object(listing: &str) -> Option<&str> {
    let record = listing.strip_suffix('\0')?;
    let (meta, _path) = record.split_once('\t')?;
    let [_mode, kind, object] = <[&str; 3]>::try_from(meta.split(' ').collect::<Vec<_>>()).ok()?;
    (kind == "blob").then_some(object)
}

/// Git, run through an [`Exec`].
pub(crate) struct GitCli<E> {
    exec: E,
}

impl<E: Exec> GitCli<E> {
    pub(crate) fn new(exec: E) -> GitCli<E> {
        GitCli { exec }
    }

    /// What `git <args>` printed on stdout.
    fn bytes(&self, args: &[&str]) -> Result<Vec<u8>, PortError> {
        let owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        stdout_of(&self.exec, "git", &owned)
    }

    /// What `git <args>` printed on stdout, as text.
    fn text(&self, args: &[&str]) -> Result<String, PortError> {
        String::from_utf8(self.bytes(args)?).map_err(|_| refuse(args, "the output is not UTF-8"))
    }
}

impl<E: Exec> GitHistory for GitCli<E> {
    fn is_shallow(&self) -> Result<bool, PortError> {
        let args = ["rev-parse", "--is-shallow-repository"];
        match self.text(&args)?.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(refuse(
                &args,
                format!("expected true or false, got {other:?}"),
            )),
        }
    }

    fn first_parents(&self, commit: &Commit) -> Result<Vec<Commit>, PortError> {
        let args = ["rev-list", "--first-parent", commit.as_str()];
        let chain = self
            .text(&args)?
            .lines()
            .map(|line| {
                line.parse::<Commit>()
                    .map_err(|e| refuse(&args, e.to_string()))
            })
            .collect::<Result<Vec<Commit>, PortError>>()?;
        if chain.is_empty() {
            Err(refuse(&args, "no commits"))
        } else {
            Ok(chain)
        }
    }

    fn file_at(&self, commit: &Commit, path: &str) -> Result<Option<String>, PortError> {
        let args = ["ls-tree", "-z", commit.as_str(), "--", path];
        let listing = self.text(&args)?;
        if listing.is_empty() {
            return Ok(None);
        }
        let object = blob_object(&listing)
            .ok_or_else(|| refuse(&args, format!("not one file: {listing:?}")))?;
        self.text(&["cat-file", "blob", object]).map(Some)
    }

    fn changed_paths(
        &self,
        commit: &Commit,
        parent: Option<&Commit>,
    ) -> Result<Vec<String>, PortError> {
        let text = match parent {
            Some(parent) => self.text(&[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                parent.as_str(),
                commit.as_str(),
            ])?,
            None => self.text(&["ls-tree", "-r", "-z", "--name-only", commit.as_str()])?,
        };
        Ok(text.split_terminator('\0').map(str::to_owned).collect())
    }

    fn commit_time(&self, commit: &Commit) -> Result<u64, PortError> {
        let args = ["show", "-s", "--format=%ct", commit.as_str()];
        let text = self.text(&args)?;
        text.trim()
            .parse()
            .map_err(|_| refuse(&args, format!("not a time: {:?}", text.trim())))
    }
}

#[cfg(test)]
mod tests;
