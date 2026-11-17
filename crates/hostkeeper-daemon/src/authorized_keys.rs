//! The managed block of the login account's `authorized_keys`, and the file it is in: where the
//! operator keys the coordinator sends are put in force (`operator-keys-v1`,
//! [`crate::operator_keys`]).
//!
//! **The block.** Two whole lines, [`BEGIN`] and [`END`], and between them the keys, one per
//! line. [`merged`] gives a file's text with the block replaced by exactly the keys it is given
//! and every other line kept byte for byte and in place; the empty set takes the block's lines
//! away and nothing else. A file with no block gets one at its end. A file whose markers do not
//! make one block (a [`BEGIN`] with no [`END`] after it, an [`END`] with no [`BEGIN`] before it, a
//! second [`BEGIN`]) is refused ([`BlockError`]): the daemon never guesses which lines are its
//! own. A marker line is compared without a trailing `\r`.
//!
//! **The file.** [`put`] makes `<home>/.ssh/authorized_keys` hold the block of a set. The file is
//! read as bytes, not text, so a hand line that is not UTF-8 (sshd takes one) is kept like any
//! other; only the keys of the block, which the wire rules make ASCII, are text. Every step is made
//! through a descriptor, never through a path twice, so that an account that owns its home (a Mac)
//! cannot swap a component for a link between the daemon's look and its write: the home is opened
//! (a link there is the account's home as the directory service says), `.ssh` is opened in it, and
//! the file and the temporary are opened and renamed in `.ssh`, each with `O_NOFOLLOW`.
//!
//! 1. The home must be a directory. `.ssh` is made, mode 0700 and the account's, when it is
//!    missing and the set is not empty; one that is there must be a directory (not a link) the
//!    account owns (checked on the open descriptor), so the daemon never takes over a directory
//!    another account owns, and its group and other write bits are cleared on that descriptor.
//! 2. `authorized_keys`, when there, must be a regular file (not a link; a pipe is not waited on).
//!    A file that already holds the text, owned by the account's user and group with no group or
//!    other bits in its mode, is not written; a missing file and an empty set make none.
//! 3. Otherwise a new file in `.ssh`, mode 0600, the account's, written and synced, is renamed over
//!    the old one inside the `.ssh` descriptor and `.ssh` is synced. No temporary file is left on
//!    any error.
//!
//! [`held`] says whether the file holds a block whose keys have a given digest, which the report of
//! every heartbeat asks, so a block edited or taken away by hand, or a file whose owner or mode
//! changed, is put back by the next set the coordinator sends. It opens the same way, with
//! `O_NOFOLLOW`. A key line of the block that is not UTF-8 is not a set: not held.
//!
//! Nothing here names a key in an error: an error names a path and what failed.

use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, fchown};
use std::path::{Path, PathBuf};

use hostkeeper_wire::operator_keys;
use rustix::fs::{AtFlags, CWD, Mode, OFlags};
use rustix::io::Errno;

use crate::login_account::Account;

/// The line that starts the managed block.
pub const BEGIN: &str =
    "# BEGIN hostkeeper operator keys: hostkeeper replaces this block; add keys by hand outside it";

/// The line that ends the managed block.
pub const END: &str = "# END hostkeeper operator keys";

/// The file in `.ssh`.
const FILE: &str = "authorized_keys";

/// The directory of the account's home.
const SSH_DIR: &str = ".ssh";

/// The name the temporary file starts with.
const TEMPORARY: &str = "hostkeeper-authorized-keys";

/// Why a file's block is not one managed block of keys. Lines are counted from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    /// A [`BEGIN`] with no [`END`] after it.
    #[error("the block that begins at line {begin} has no end line")]
    NoEnd {
        /// The line of the [`BEGIN`].
        begin: usize,
    },
    /// An [`END`] with no [`BEGIN`] before it, or after a block that is whole.
    #[error("line {line} ends a block that was not begun")]
    EndWithoutBegin {
        /// The line of the [`END`].
        line: usize,
    },
    /// A [`BEGIN`] while a block is open or after one that is whole.
    #[error("line {line} begins a second block")]
    SecondBegin {
        /// The line of the second [`BEGIN`].
        line: usize,
    },
    /// A line between the markers that is not UTF-8, so not a key of any set (only
    /// [`block_keys`] says so; [`merged`] replaces the block whatever its lines are).
    #[error("line {line} of the block is not text")]
    KeyNotText {
        /// The line.
        line: usize,
    },
}

/// Why [`put`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum KeysFileError {
    /// A path that must be a directory is not one (a link included).
    #[error("{} is not a directory", path.display())]
    NotADirectory { path: PathBuf },
    /// `.ssh` belongs to another account.
    #[error("{} is owned by user {owner}, not by the login account's user {expected}", path.display())]
    NotOwned {
        path: PathBuf,
        owner: u32,
        expected: u32,
    },
    /// `authorized_keys` is not a regular file (a link, a directory, a pipe).
    #[error("{} is not a regular file", path.display())]
    NotAFile { path: PathBuf },
    /// The file's markers do not make one block; the file is as it was.
    #[error("{}: {source}", path.display())]
    Block {
        path: PathBuf,
        #[source]
        source: BlockError,
    },
    /// A filesystem call failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What [`put`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    /// The file was made or replaced.
    Written,
    /// The file held the block already, as it should be owned, and was not written.
    Unchanged,
}

/// Whether `line` is the marker line `marker`, compared without its line ending: `"\n"` and then a
/// `"\r"`.
fn is_line(line: &[u8], marker: &str) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line) == marker.as_bytes()
}

/// A text read as lines, and where its block is.
struct Scanned<'a> {
    /// The lines, each with its line ending.
    lines: Vec<&'a [u8]>,
    /// The index of the block's [`BEGIN`] and of its [`END`].
    block: Option<(usize, usize)>,
}

/// `text` as its lines and where the block is in them.
fn scan(text: &[u8]) -> Result<Scanned<'_>, BlockError> {
    let lines: Vec<&[u8]> = text.split_inclusive(|byte| *byte == b'\n').collect();
    let mut begun: Option<usize> = None;
    let mut block: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        if is_line(line, BEGIN) {
            if begun.is_some() || block.is_some() {
                return Err(BlockError::SecondBegin { line: number });
            }
            begun = Some(index);
        } else if is_line(line, END) {
            match begun.take() {
                Some(begin) => block = Some((begin, index)),
                None => return Err(BlockError::EndWithoutBegin { line: number }),
            }
        }
    }
    match begun {
        Some(begin) => Err(BlockError::NoEnd { begin: begin + 1 }),
        None => Ok(Scanned { lines, block }),
    }
}

/// The text of the block of `keys`: its two markers and a line for each key.
fn block_text(keys: &[String]) -> Vec<u8> {
    let mut text = format!("{BEGIN}\n").into_bytes();
    for key in keys {
        text.extend_from_slice(key.as_bytes());
        text.push(b'\n');
    }
    text.extend_from_slice(END.as_bytes());
    text.push(b'\n');
    text
}

/// `current` with its block replaced by `keys`, one per line between the markers, and every other
/// line kept byte for byte and in place, whatever its bytes. With no block and keys, the block is
/// added at the end, after a newline when the text does not end with one; with no keys the block's
/// lines are taken away, and a text with no block is given back as it is.
///
/// # Errors
/// [`BlockError`] when the markers of `current` do not make one block.
pub fn merged(current: &[u8], keys: &[String]) -> Result<Vec<u8>, BlockError> {
    let Scanned { lines, block } = scan(current)?;
    match block {
        Some((begin, end)) => {
            let mut text: Vec<u8> = lines[..begin].concat();
            if !keys.is_empty() {
                text.extend_from_slice(&block_text(keys));
            }
            text.extend_from_slice(&lines[end + 1..].concat());
            Ok(text)
        }
        None if keys.is_empty() => Ok(current.to_vec()),
        None => {
            let mut text = current.to_vec();
            if !text.is_empty() && !text.ends_with(b"\n") {
                text.push(b'\n');
            }
            text.extend_from_slice(&block_text(keys));
            Ok(text)
        }
    }
}

/// The keys of `current`'s block, one per line as written (a trailing `\r` is part of a key's
/// line, so a block written with it holds no set the daemon sent); none when there is no block.
///
/// # Errors
/// [`BlockError`] when the markers of `current` do not make one block, or a line of the block is
/// not UTF-8.
pub fn block_keys(current: &[u8]) -> Result<Vec<String>, BlockError> {
    let Scanned { lines, block } = scan(current)?;
    let Some((begin, end)) = block else {
        return Ok(Vec::new());
    };
    lines[begin + 1..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            String::from_utf8(line.to_vec()).map_err(|_| BlockError::KeyNotText {
                line: begin + 2 + offset,
            })
        })
        .collect()
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> KeysFileError + '_ {
    |source| KeysFileError::Io {
        path: path.to_owned(),
        source,
    }
}

/// What a failed `openat` of `path` is: `refused` when the name is a link or not a directory (the
/// opens that follow no link and want a directory say so with these two), else the call's error.
fn open_error(errno: Errno, path: &Path, refused: fn(PathBuf) -> KeysFileError) -> KeysFileError {
    if matches!(errno, Errno::LOOP | Errno::NOTDIR) {
        refused(path.to_owned())
    } else {
        KeysFileError::Io {
            path: path.to_owned(),
            source: errno.into(),
        }
    }
}

/// The directory `path`, followed when it is a link: it is the account's home as the directory
/// service says.
fn open_home(path: &Path) -> Result<File, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    rustix::fs::openat(CWD, path, flags, Mode::empty()).map(File::from)
}

/// The directory `.ssh` of `home`, never through a link.
fn open_ssh(home: &File) -> Result<File, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(home, SSH_DIR, flags, Mode::empty()).map(File::from)
}

/// The file in `dir`, never through a link and without waiting on a pipe.
fn open_file(dir: &File) -> Result<File, Errno> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::openat(dir, FILE, flags, Mode::empty()).map(File::from)
}

/// Makes `.ssh` in `home`, mode 0700 and the account's, and opens it.
fn make_ssh(home: &File, account: &Account) -> io::Result<File> {
    rustix::fs::mkdirat(home, SSH_DIR, Mode::from_raw_mode(0o700))?;
    let dir = open_ssh(home)?;
    fchown(&dir, Some(account.uid), Some(account.gid))?;
    Ok(dir)
}

/// Makes `<home>/.ssh/authorized_keys` of `account` hold the block of `keys`, as the module's steps
/// say.
///
/// # Errors
/// [`KeysFileError`]; the file is as it was.
pub fn put(account: &Account, keys: &[String]) -> Result<Put, KeysFileError> {
    let home_path = &account.home;
    let home = open_home(home_path).map_err(|errno| {
        open_error(errno, home_path, |path| KeysFileError::NotADirectory {
            path,
        })
    })?;
    let dir_path = home_path.join(SSH_DIR);
    let not_a_directory = |path| KeysFileError::NotADirectory { path };
    let dir = match open_ssh(&home) {
        Ok(dir) => dir,
        Err(Errno::NOENT) if keys.is_empty() => return Ok(Put::Unchanged),
        Err(Errno::NOENT) => make_ssh(&home, account).map_err(io_error(&dir_path))?,
        Err(errno) => return Err(open_error(errno, &dir_path, not_a_directory)),
    };
    let meta = dir.metadata().map_err(io_error(&dir_path))?;
    if meta.uid() != account.uid {
        return Err(KeysFileError::NotOwned {
            path: dir_path,
            owner: meta.uid(),
            expected: account.uid,
        });
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o022 != 0 {
        dir.set_permissions(Permissions::from_mode(mode & !0o022))
            .map_err(io_error(&dir_path))?;
    }
    let target = dir_path.join(FILE);
    let (current, existing) = match open_file(&dir) {
        Ok(mut file) => {
            let meta = file.metadata().map_err(io_error(&target))?;
            if !meta.is_file() {
                return Err(KeysFileError::NotAFile { path: target });
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(io_error(&target))?;
            (bytes, Some(meta))
        }
        Err(Errno::NOENT) => (Vec::new(), None),
        Err(errno) => {
            return Err(open_error(errno, &target, |path| KeysFileError::NotAFile {
                path,
            }));
        }
    };
    let text = merged(&current, keys).map_err(|source| KeysFileError::Block {
        path: target.clone(),
        source,
    })?;
    let unchanged = match &existing {
        Some(meta) => {
            (text == current)
                & (meta.uid() == account.uid)
                & (meta.gid() == account.gid)
                & (meta.mode() & 0o077 == 0)
        }
        None => keys.is_empty(),
    };
    if unchanged {
        return Ok(Put::Unchanged);
    }
    replace(&dir, &text, account).map_err(io_error(&target))?;
    Ok(Put::Written)
}

/// Makes the file in `dir` hold `text`, mode 0600 and the account's, through a temporary file of
/// `dir` that is removed on any error.
fn replace(dir: &File, text: &[u8], account: &Account) -> io::Result<()> {
    let name = format!(".{TEMPORARY}.{}.tmp", std::process::id());
    unlink(dir, &name)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = File::from(rustix::fs::openat(
        dir,
        &name,
        flags,
        Mode::RUSR | Mode::WUSR,
    )?);
    let temporary = Temporary { dir, name: &name };
    fchown(&file, Some(account.uid), Some(account.gid))?;
    file.write_all(text)?;
    file.sync_all()?;
    rustix::fs::renameat(dir, &name, dir, FILE)?;
    drop(temporary);
    Ok(rustix::fs::fsync(dir)?)
}

/// Removes `name` from `dir`; a name that is not there is fine.
fn unlink(dir: &impl AsFd, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(errno) => Err(errno.into()),
    }
}

/// Removes the file `name` of `dir` when dropped, so that no failure leaves it behind; after a
/// rename there is nothing at the name and the removal finds nothing.
struct Temporary<'a> {
    dir: &'a File,
    name: &'a str,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        let _ = unlink(self.dir, self.name);
    }
}

/// Whether the login account's file holds a block whose keys have the digest `digest`
/// ([`operator_keys::digest`]) as the account's own: `.ssh` a directory (not a link) of the
/// account's user, the file, when there, a regular file of that user with no group or other
/// bits in its mode, both opened through descriptors with `O_NOFOLLOW`. A missing file holds the
/// empty set. Anything that cannot be read, markers that do not make one block, and a key line
/// that is not UTF-8 are not held.
#[must_use]
pub fn held(account: &Account, digest: &str) -> bool {
    file_bytes(account).is_some_and(|bytes| {
        block_keys(&bytes).is_ok_and(|keys| operator_keys::digest(&keys) == digest)
    })
}

/// The bytes of the account's file when it is as [`held`] needs it, and no bytes when there is no
/// file.
fn file_bytes(account: &Account) -> Option<Vec<u8>> {
    let home = open_home(&account.home).ok()?;
    let dir = match open_ssh(&home) {
        Ok(dir) => dir,
        Err(Errno::NOENT) => return Some(Vec::new()),
        Err(_) => return None,
    };
    if dir.metadata().ok()?.uid() != account.uid {
        return None;
    }
    let mut file = match open_file(&dir) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Some(Vec::new()),
        Err(_) => return None,
    };
    let meta = file.metadata().ok()?;
    let sound =
        meta.file_type().is_file() & (meta.uid() == account.uid) & (meta.mode() & 0o077 == 0);
    if !sound {
        return None;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
#[path = "authorized_keys_tests.rs"]
mod tests;
