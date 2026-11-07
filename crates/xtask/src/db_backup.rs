//! `db-backup --into <dir> --keep <n> -- <command>...`: one backup of the coordinator's database
//! (`docs/design/coordinator-storage.md`, section 10), run by the timer of the compose deployment
//! (`docs/deploy/coordinator/hostkeeper-db-backup.{timer,service}`).
//!
//! It runs `<command>` (the deployment's `docker compose exec -T postgres pg_dump
//! --format=custom ...`) with its standard output into a new file of `--into`, and keeps the
//! newest `--keep` dumps. In this order:
//!
//! 1. `--into` must be a directory, not a symbolic link, closed to group and others (a dump holds
//!    every row of the database, the enrollment store's among them).
//! 2. A `hostkeeper-db-<n>.dump.partial` there is what a run that did not finish left (the unit
//!    is a oneshot, so two runs never overlap); it is removed.
//! 3. The dump is numbered one past the highest `hostkeeper-db-<n>.dump` there (1 in an empty
//!    directory). The number, not the time, orders the dumps, so this reads no clock and a clock
//!    set back never makes a new dump look older than the ones it should replace; the file's
//!    modification time says when it was taken.
//! 4. The command's output goes to `hostkeeper-db-<n>.dump.partial`, created exclusively with
//!    mode 0600. The dump is refused, and the partial file removed, when the command cannot be
//!    started, ends with anything but success, or writes nothing.
//! 5. Otherwise the file is flushed to disk, renamed to `hostkeeper-db-<n>.dump`, and the
//!    directory flushed, so a dump either is there whole or is not there.
//! 6. Only then are the oldest dumps beyond the newest `--keep` removed. A run that fails
//!    removes none, so failing runs never eat the good dumps.
//!
//! Every other file in the directory is left alone. The command's standard error is the
//! verb's, so the journal shows what `pg_dump` said.

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The flags of `db-backup`.
#[derive(Debug, clap::Args)]
pub(crate) struct Flags {
    /// The directory of the dumps: a directory closed to group and others.
    #[arg(long)]
    pub(crate) into: PathBuf,
    /// How many dumps to keep, the new one among them (at least 1).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) keep: u32,
    /// The command that writes one dump to its standard output, after `--`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub(crate) command: Vec<OsString>,
}

/// Why no dump was taken, or the old ones were not all removed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// `--into` is not a directory this verb writes dumps into.
    #[error("--into {path}: {why}")]
    Directory { path: String, why: String },
    /// No command was given after `--`.
    #[error("no dump command was given after --; no dump was written")]
    NoCommand,
    /// The dump command could not be started.
    #[error("the dump command {program} could not be started: {why}; no dump was written")]
    Start { program: String, why: String },
    /// The dump command ended otherwise than with success.
    #[error("the dump command ended with {status}; no dump was written")]
    Failed { status: String },
    /// The dump command succeeded and wrote nothing.
    #[error("the dump command wrote nothing; no dump was written")]
    Empty,
    /// A file of the directory could not be read, written, renamed or removed.
    #[error("{path}: {why}")]
    Io { path: String, why: String },
}

/// `result`, its error as [`Error::Io`] naming `path`.
fn at<T>(path: &Path, result: std::io::Result<T>) -> Result<T, Error> {
    match result {
        Ok(value) => Ok(value),
        Err(why) => Err(Error::Io {
            path: path.display().to_string(),
            why: why.to_string(),
        }),
    }
}

/// What writes one dump into the file it is given, and closes it. A trait object, not a generic,
/// so the steps around it are one function whatever writes the dump.
pub(crate) type Dump<'a> = dyn FnOnce(File) -> Result<(), Error> + 'a;

/// The name of dump `n`.
fn dump_name(n: u64) -> String {
    format!("hostkeeper-db-{n}.dump")
}

/// The number of a dump's name (`hostkeeper-db-<n>.dump`), or of a partial dump's
/// (`hostkeeper-db-<n>.dump.partial`) when `partial`. A number is written in decimal with no
/// leading zero, so each number has one name.
fn number_of(name: &str, partial: bool) -> Option<u64> {
    let rest = name.strip_prefix("hostkeeper-db-")?;
    let digits = if partial {
        rest.strip_suffix(".dump.partial")?
    } else {
        rest.strip_suffix(".dump")?
    };
    let canonical = !digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit());
    digits.parse().ok().filter(|_| canonical)
}

/// Flushes the directory `dir` to disk, so a rename in it lasts.
fn sync_dir(dir: &Path) -> Result<(), Error> {
    at(dir, File::open(dir).and_then(|d| d.sync_all()))
}

/// The verb: one dump by `flags.command`, then the old ones beyond `flags.keep` removed.
///
/// # Errors
/// [`Error`]: see the module.
pub(crate) fn run(flags: &Flags) -> Result<Vec<String>, Error> {
    let (program, args) = flags.command.split_first().ok_or(Error::NoCommand)?;
    backup(
        &flags.into,
        flags.keep as usize,
        Box::new(|out| {
            let started = |e: std::io::Error| Error::Start {
                program: program.to_string_lossy().into_owned(),
                why: e.to_string(),
            };
            let status = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::from(out))
                .status()
                .map_err(started)?;
            if status.success() {
                Ok(())
            } else {
                Err(Error::Failed {
                    status: status.to_string(),
                })
            }
        }),
    )
}

/// One dump into `dir` by `dump`, which writes it to the file it is given (and closes it), then
/// the dumps
/// beyond the newest `keep` removed; answers the lines the verb prints.
///
/// # Errors
/// [`Error`]: see the module.
pub(crate) fn backup(dir: &Path, keep: usize, dump: Box<Dump<'_>>) -> Result<Vec<String>, Error> {
    let meta = at(dir, std::fs::symlink_metadata(dir))?;
    let refuse = |why: String| Error::Directory {
        path: dir.display().to_string(),
        why,
    };
    if !meta.is_dir() {
        return Err(refuse("not a directory".to_owned()));
    }
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(refuse(format!(
            "it is open to group or others (mode {mode:04o}); a dump holds every row of the \
             database, so its directory must be its owner's alone"
        )));
    }

    let mut lines = Vec::new();
    let mut numbers = Vec::new();
    for entry in at(dir, std::fs::read_dir(dir))? {
        let entry = at(dir, entry)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(n) = number_of(&name, false) {
            numbers.push(n);
        } else if number_of(&name, true).is_some() {
            let path = entry.path();
            at(&path, std::fs::remove_file(&path))?;
            lines.push(format!(
                "db-backup: removed {name}, left by a run that did not finish"
            ));
        }
    }
    let next = numbers.iter().max().map_or(1, |n| n.saturating_add(1));
    if numbers.contains(&next) {
        return Err(refuse(format!(
            "it holds {}, the highest number a dump can have",
            dump_name(next)
        )));
    }

    let name = dump_name(next);
    let path = dir.join(&name);
    let partial = dir.join(format!("{name}.partial"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let file = at(&partial, options.open(&partial))?;
    let written = dump(file).and_then(|()| {
        // Opened again: the command was given the first handle, and closed it when it ended.
        let file = at(&partial, File::open(&partial))?;
        let bytes = at(&partial, file.metadata())?.len();
        if bytes == 0 {
            return Err(Error::Empty);
        }
        at(&partial, file.sync_all())?;
        Ok(bytes)
    });
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(why) => {
            // The partial file goes with the refusal; one left behind is removed by the next run.
            let _ = std::fs::remove_file(&partial);
            return Err(why);
        }
    };
    at(&path, std::fs::rename(&partial, &path))?;
    sync_dir(dir)?;
    lines.push(format!("db-backup: wrote {name}, {bytes} bytes"));

    numbers.push(next);
    numbers.sort_unstable();
    let old = numbers.len().saturating_sub(keep);
    for n in &numbers[..old] {
        let gone = dir.join(dump_name(*n));
        at(&gone, std::fs::remove_file(&gone))?;
        lines.push(format!("db-backup: removed {}", dump_name(*n)));
    }
    lines.push(format!(
        "db-backup: {} dumps kept, the newest {name}",
        numbers.len() - old
    ));
    Ok(lines)
}
