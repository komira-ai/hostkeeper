//! The host's identity copy: the certificate and key the coordinator's CA issued to the host,
//! kept at one fixed place that the services of the host may read (docs/design/host-identity.md,
//! section 2).
//!
//! [`HostIdentity::after_step`] runs after every step of the enrollment loop. It reads the
//! current generation of the pki directory through the heartbeat loop's own certificate source
//! (`current -> gen-<n>`: `cert.pem` and the key from one generation, which that source checks
//! for a pair, the key as PKCS#8 PEM) and makes [`IDENTITY_DIR`] say the same:
//!
//! - The directory is made when it is missing, owned by `owner` and the group, mode 0750 (0700
//!   without the group); one that is there is opened with `O_NOFOLLOW` (a link is refused, never
//!   followed) and its owner, group and mode are put right.
//! - `host.key` ([`KEY_FILE`], 0640 with the group, 0600 without) is made first, `host.crt`
//!   ([`CERT_FILE`], 0644) second. A file whose bytes, owner, group and mode are already as they
//!   should be is not written. Otherwise it is written to a new file of the directory (created
//!   with `O_EXCL` and `O_NOFOLLOW`, mode 0600, owned and moded before any byte is written),
//!   synced, and renamed over the old one, and the directory is synced; the temporary is removed
//!   on any error, and one a crash left is removed first.
//! - A host with no current pair gets nothing, and what an earlier enrollment left stays.
//! - The group ([`crate::identity_group`]) is looked up each time, and made when the host has
//!   none. When it cannot be found or made the files are written for `owner` alone, with the
//!   group `root_gid` and the key 0600, and the reason is logged.
//!
//! A failure is logged and changes nothing else: the next step tries again. One line is logged
//! when the copy is first made and each time what it came to changes, not at every step.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, fchown};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_link::{CertSource, CertState, Log};
use rustix::fs::{AtFlags, CWD, Mode, OFlags};
use rustix::io::Errno;

use crate::identity_group::IdentityGroup;

/// The directory of the copy.
pub const IDENTITY_DIR: &str = "/var/lib/hostkeeper/identity";

/// The certificate in [`IDENTITY_DIR`], PEM.
pub const CERT_FILE: &str = "host.crt";

/// The key in [`IDENTITY_DIR`], PKCS#8 PEM.
pub const KEY_FILE: &str = "host.key";

/// The line logged when the copy is made for the group, before the directory.
pub const IDENTITY_KEPT: &str = "the host identity is kept for the group in ";

/// The line logged when the copy could not be made, before the reason.
pub const IDENTITY_NOT_KEPT: &str = "error: the host identity copy is not made: ";

/// The line logged when the copy is made for root alone, before the reason the group is not there.
pub const IDENTITY_ROOT_ONLY: &str =
    "error: the host identity copy is for root alone, since its group is not there: ";

const DIR_MODE: u32 = 0o750;
const DIR_MODE_ROOT_ONLY: u32 = 0o700;
const KEY_MODE: u32 = 0o640;
const KEY_MODE_ROOT_ONLY: u32 = 0o600;
const CERT_MODE: u32 = 0o644;
/// The mode of a directory made on the way to [`IDENTITY_DIR`].
const PARENT_MODE: u32 = 0o755;

/// Why the copy was not made.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The directory of the copy is not a directory (a link included).
    #[error("{} is not a directory", path.display())]
    NotADirectory { path: PathBuf },
    /// A filesystem call failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What one export came to.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The host has no current pair.
    NoPair,
    /// The copy is as it should be, for the group.
    ForGroup,
    /// The copy is as it should be, for root alone; why the group is not there.
    ForRootOnly(String),
}

/// Keeps the host's identity copy current.
pub struct HostIdentity {
    certs: Arc<dyn CertSource>,
    dir: PathBuf,
    group: IdentityGroup,
    owner: u32,
    root_gid: u32,
    /// The line last logged; none at a host with no pair.
    logged: Option<String>,
}

/// What a file should be.
#[derive(Clone, Copy)]
struct Want {
    uid: u32,
    gid: u32,
    mode: u32,
}

impl HostIdentity {
    /// The copy of the pair `certs` gives, in `dir`, owned by `owner`; the group of the files is
    /// `group`'s, or `root_gid` when the group cannot be found or made.
    #[must_use]
    pub fn new(
        certs: Arc<dyn CertSource>,
        dir: PathBuf,
        group: IdentityGroup,
        owner: u32,
        root_gid: u32,
    ) -> HostIdentity {
        HostIdentity {
            certs,
            dir,
            group,
            owner,
            root_gid,
            logged: None,
        }
    }

    /// Makes the copy as the module says, and logs what it came to to `log` when that is not what
    /// was logged last.
    pub fn after_step(&mut self, log: &dyn Log) {
        let line = match self.export() {
            Ok(Outcome::NoPair) => {
                self.logged = None;
                return;
            }
            Ok(Outcome::ForGroup) => format!("{IDENTITY_KEPT}{}", self.dir.display()),
            Ok(Outcome::ForRootOnly(why)) => format!("{IDENTITY_ROOT_ONLY}{why}"),
            Err(error) => format!("{IDENTITY_NOT_KEPT}{error}"),
        };
        if self.logged.as_deref() != Some(line.as_str()) {
            log.line(&line);
            self.logged = Some(line);
        }
    }

    fn export(&self) -> Result<Outcome, IdentityError> {
        let CertState::Enrolled(pair) = self.certs.current() else {
            return Ok(Outcome::NoPair);
        };
        let (gid, why) = match self.group.ensure() {
            Ok(gid) => (gid, None),
            Err(error) => (self.root_gid, Some(error.to_string())),
        };
        let shared = why.is_none();
        let want = |mode_shared, mode_alone| Want {
            uid: self.owner,
            gid,
            mode: if shared { mode_shared } else { mode_alone },
        };
        let dir = open_dir(&self.dir, want(DIR_MODE, DIR_MODE_ROOT_ONLY))?;
        let io_error = |name: &str| {
            let path = self.dir.join(name);
            move |source| IdentityError::Io { path, source }
        };
        put(
            &dir,
            KEY_FILE,
            pair.key(),
            want(KEY_MODE, KEY_MODE_ROOT_ONLY),
        )
        .map_err(io_error(KEY_FILE))?;
        put(
            &dir,
            CERT_FILE,
            pair.certificate(),
            want(CERT_MODE, CERT_MODE),
        )
        .map_err(io_error(CERT_FILE))?;
        Ok(why.map_or(Outcome::ForGroup, Outcome::ForRootOnly))
    }
}

/// The directory `path`, made when missing and put right when it is there, never through a link
/// at its own name.
fn open_dir(path: &Path, want: Want) -> Result<File, IdentityError> {
    let io_error = |source| IdentityError::Io {
        path: path.to_owned(),
        source,
    };
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(IdentityError::NotADirectory {
            path: path.to_owned(),
        });
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PARENT_MODE)
        .create(parent)
        .map_err(io_error)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let parent = rustix::fs::openat(CWD, parent, flags, Mode::empty())
        .map(File::from)
        .map_err(|errno| io_error(errno.into()))?;
    let flags = flags | OFlags::NOFOLLOW;
    let dir = match rustix::fs::openat(&parent, name, flags, Mode::empty()) {
        Ok(dir) => dir,
        Err(Errno::NOENT) => {
            rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o700))
                .map_err(|errno| io_error(errno.into()))?;
            rustix::fs::openat(&parent, name, flags, Mode::empty())
                .map_err(|errno| io_error(errno.into()))?
        }
        Err(Errno::LOOP | Errno::NOTDIR) => {
            return Err(IdentityError::NotADirectory {
                path: path.to_owned(),
            });
        }
        Err(errno) => return Err(io_error(errno.into())),
    };
    let dir = File::from(dir);
    let meta = dir.metadata().map_err(io_error)?;
    if (meta.uid(), meta.gid()) != (want.uid, want.gid) {
        fchown(&dir, Some(want.uid), Some(want.gid)).map_err(io_error)?;
    }
    if meta.mode() & 0o7777 != want.mode {
        rustix::fs::fchmod(&dir, Mode::from_raw_mode(want.mode))
            .map_err(|errno| io_error(errno.into()))?;
    }
    Ok(dir)
}

/// Makes the file `name` of `dir` hold `bytes` as `want` says; a file that does is left alone.
fn put(dir: &File, name: &str, bytes: &[u8], want: Want) -> io::Result<()> {
    if holds(dir, name, bytes, want) {
        return Ok(());
    }
    replace(dir, name, bytes, want)
}

/// Whether the file `name` of `dir` is a regular file (not a link) with `bytes` as `want` says.
fn holds(dir: &File, name: &str, bytes: &[u8], want: Want) -> bool {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let Ok(file) = rustix::fs::openat(dir, name, flags, Mode::empty()).map(File::from) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let sound = meta.file_type().is_file()
        & (meta.uid() == want.uid)
        & (meta.gid() == want.gid)
        & (meta.mode() & 0o7777 == want.mode);
    if !sound {
        return false;
    }
    let mut held = Vec::new();
    let limit = u64::try_from(bytes.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit).read_to_end(&mut held).is_ok() && held == bytes
}

/// Makes the file `name` of `dir` hold `bytes`, through a temporary file of `dir`, as the module's
/// steps say.
fn replace(dir: &File, name: &str, bytes: &[u8], want: Want) -> io::Result<()> {
    let temporary_name = format!(".{name}.{}.tmp", std::process::id());
    unlink(dir, &temporary_name)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = File::from(rustix::fs::openat(
        dir,
        &temporary_name,
        flags,
        Mode::RUSR | Mode::WUSR,
    )?);
    let temporary = Temporary {
        dir,
        name: &temporary_name,
    };
    fchown(&file, Some(want.uid), Some(want.gid))?;
    rustix::fs::fchmod(&file, Mode::from_raw_mode(want.mode))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    rustix::fs::renameat(dir, &temporary_name, dir, name)?;
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

#[cfg(test)]
#[path = "host_identity_tests.rs"]
mod tests;
