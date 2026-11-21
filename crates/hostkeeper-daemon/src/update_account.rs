//! The update account of a Mac (the maintenance-updates design, section 5.5, proposed in
//! komira-ai/hostkeeper#447): the local administrator whose password authorises a macOS update, and
//! the one call that uses it.
//!
//! **The file.** The account's name and password are in [`UPDATE_ACCOUNT_FILE`], written by the
//! Mac's bootstrap: lines of `key=value`, the key `name` once and the key `password` once, in
//! either order, blank lines ignored:
//!
//! ```text
//! name=hkupdate
//! password=<the password>
//! ```
//!
//! The value is the rest of its line after the first `=`, taken byte for byte (no trimming), so a
//! password may hold any byte but a newline. A name is ASCII letters, digits, `_`, `.` and `-`,
//! and does not start with `-` (it goes in an argument). The file is at most [`MAX_BYTES`].
//!
//! **The checks.** [`UpdateAccount::read`] opens the file without following a link, without waiting
//! on a pipe and without taking a terminal as its controlling terminal, and judges the file it
//! opened: a regular file, owned by the owner it is given (root, [`ROOT_UID`], on a host), with
//! mode 0600 exactly. Anything else is refused with an [`UpdateAccountError`] naming the file and
//! the problem. No error holds a byte of the file: a line that cannot be read is named by its
//! number alone.
//!
//! **The password** ([`Password`]) is not treated as a secret (section 5.5), but it goes to one
//! place: the standard input of the update call ([`UpdateAccount::run`], over the [`StdinRun`]
//! port), followed by a newline, and of the two `sysadminctl` calls that make the account at a
//! Mac's start (the daemon's `update_account_make`). It is never in an argument, the program's
//! environment, a log line or an error: the type has no `Display`, its `Debug` prints
//! `Password(withheld)`, and the call's error and log line name the step and the exit code alone.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_launchd::stdin_process::run_ok;
use hostkeeper_launchd::{LaunchctlOutput, StdinError, StdinRun};
use hostkeeper_link::Log;

/// Where a Mac keeps its update account.
pub const UPDATE_ACCOUNT_FILE: &str = "/Library/Application Support/hostkeeper/update-account";

/// The owner the file must have on a host: root.
pub const ROOT_UID: u32 = 0;

/// The one mode the file may have.
pub const MODE: u32 = 0o600;

/// The largest file read.
pub const MAX_BYTES: u64 = 4096;

/// The update account's password. It has no `Display`, and its `Debug` prints none of it.
pub struct Password(Vec<u8>);

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(withheld)")
    }
}

/// The update account, as its file holds it.
#[derive(Debug)]
pub struct UpdateAccount {
    name: String,
    password: Password,
}

/// What is wrong with a line of the file. None holds a byte of the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LineFault {
    #[error("is not key=value")]
    NotKeyValue,
    #[error("has a key other than name and password")]
    UnknownKey,
    #[error("repeats a key")]
    Repeated,
    #[error("has a name that is not an account name")]
    BadName,
    #[error("has an empty password")]
    EmptyPassword,
}

/// What is wrong with the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("is missing")]
    Missing,
    #[error("is a symbolic link")]
    Link,
    #[error("is not a regular file")]
    NotRegular,
    #[error("is owned by uid {found}, not uid {wanted}")]
    Owner { found: u32, wanted: u32 },
    #[error("has mode {mode:04o}, not 0600")]
    Mode { mode: u32 },
    #[error("is larger than {} bytes", MAX_BYTES)]
    TooLarge,
    #[error("could not be read: {0}")]
    Unreadable(io::ErrorKind),
    #[error("has no name line")]
    NoName,
    #[error("has no password line")]
    NoPassword,
    #[error("line {line} {fault}")]
    Line { line: usize, fault: LineFault },
}

/// Why the update account could not be read: the file and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the update-account file {} {problem}", path.display())]
pub struct UpdateAccountError {
    pub path: PathBuf,
    pub problem: Problem,
}

/// The problem an error of opening, inspecting or reading the file is.
fn problem(error: io::Error) -> Problem {
    match error.kind() {
        io::ErrorKind::NotFound => Problem::Missing,
        // `O_NOFOLLOW` refuses a link with ELOOP, on Linux and on macOS.
        _ if error.raw_os_error() == Some(libc::ELOOP) => Problem::Link,
        kind => Problem::Unreadable(kind),
    }
}

/// Whether `name` is an account name the file may hold.
fn account_name(name: &[u8]) -> bool {
    let allowed = |byte: &u8| byte.is_ascii_alphanumeric() | b"_.-".contains(byte);
    !name.is_empty() & !name.starts_with(b"-") & name.iter().all(allowed)
}

/// Puts `value` in `slot`, which must be empty.
fn put(slot: &mut Option<Vec<u8>>, value: &[u8]) -> Result<(), LineFault> {
    if slot.is_some() {
        return Err(LineFault::Repeated);
    }
    *slot = Some(value.to_vec());
    Ok(())
}

/// The name and the password of the file's `bytes`.
fn parse(bytes: &[u8]) -> Result<(String, Vec<u8>), Problem> {
    let mut name = None;
    let mut password = None;
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let fault = |fault| Problem::Line {
            line: index + 1,
            fault,
        };
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, |byte| *byte == b'=');
        let key = parts.next().unwrap_or_default();
        let value = parts.next().ok_or(fault(LineFault::NotKeyValue))?;
        match key {
            b"name" if !account_name(value) => return Err(fault(LineFault::BadName)),
            b"name" => put(&mut name, value).map_err(fault)?,
            b"password" if value.is_empty() => return Err(fault(LineFault::EmptyPassword)),
            b"password" => put(&mut password, value).map_err(fault)?,
            _ => return Err(fault(LineFault::UnknownKey)),
        }
    }
    let name = name.ok_or(Problem::NoName)?;
    let password = password.ok_or(Problem::NoPassword)?;
    Ok((String::from_utf8_lossy(&name).into_owned(), password))
}

/// The checks of the module's doc on the file at `path`, and its bytes.
fn read_checked(path: &Path, owner: u32) -> Result<Vec<u8>, Problem> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(problem)?;
    let meta = file.metadata().map_err(problem)?;
    if !meta.is_file() {
        return Err(Problem::NotRegular);
    }
    if meta.uid() != owner {
        return Err(Problem::Owner {
            found: meta.uid(),
            wanted: owner,
        });
    }
    let mode = meta.mode() & 0o7777;
    if mode != MODE {
        return Err(Problem::Mode { mode });
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(problem)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Problem::TooLarge);
    }
    Ok(bytes)
}

impl UpdateAccount {
    /// The update account in the file at `path` ([`UPDATE_ACCOUNT_FILE`] on a host), which must
    /// be owned by `owner` ([`ROOT_UID`] on a host).
    ///
    /// # Errors
    /// [`UpdateAccountError`], naming `path` and the [`Problem`].
    pub fn read(path: &Path, owner: u32) -> Result<UpdateAccount, UpdateAccountError> {
        let refused = |problem| UpdateAccountError {
            path: path.to_owned(),
            problem,
        };
        let bytes = read_checked(path, owner).map_err(refused)?;
        let (name, password) = parse(&bytes).map_err(refused)?;
        Ok(UpdateAccount {
            name,
            password: Password(password),
        })
    }

    /// The account's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The account's password, for the one place it goes besides [`UpdateAccount::run`]: the
    /// standard input of the calls that make the account at a Mac's start
    /// (the daemon's `update_account_make`).
    pub(crate) fn password(&self) -> &[u8] {
        &self.password.0
    }

    /// The update call: runs `args` through `runner`, the password and a newline on its standard
    /// input, and logs one line, which names `step` and the exit code alone. An exit code other
    /// than 0 is an error ([`run_ok`]).
    ///
    /// # Errors
    /// [`StdinError`], which names `step` and the exit code alone.
    pub fn run(
        &self,
        runner: &dyn StdinRun,
        step: &str,
        args: &[String],
        log: &dyn Log,
    ) -> Result<LaunchctlOutput, StdinError> {
        let mut input = self.password.0.clone();
        input.push(b'\n');
        let result = run_ok(runner, step, args, &input);
        match &result {
            Ok(_) => log.line(&format!("{step}: exited with code 0")),
            Err(error) => log.line(&error.to_string()),
        }
        result
    }
}

#[cfg(test)]
#[path = "update_account_tests.rs"]
mod tests;
