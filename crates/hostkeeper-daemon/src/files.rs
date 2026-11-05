//! The daemon's own directories and small files: made closed to group and others, written
//! durably.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Makes `dir` mode 0700 (its missing parents 0755, so that what lies beside it stays reachable),
/// or sets an existing one to 0700. Something at `dir` that is not a directory (a link
/// included) is an error.
pub fn make_private(dir: &Path) -> io::Result<()> {
    let parent = dir.parent().unwrap_or(dir);
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(parent)?;
    let exists = |e: io::Error| {
        (e.kind() == io::ErrorKind::AlreadyExists)
            .then_some(())
            .ok_or(e)
    };
    DirBuilder::new().mode(0o700).create(dir).or_else(exists)?;
    let meta = fs::symlink_metadata(dir)?;
    match meta.is_dir() {
        true => fs::set_permissions(dir, fs::Permissions::from_mode(0o700)),
        false => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory", dir.display()),
        )),
    }
}

/// Where a durable write is, for a test that stops the daemon there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStep {
    /// The temporary file is written and fsynced; the rename has not happened.
    BeforeRename,
    /// The rename and the directory's fsync have happened.
    AfterRename,
}

/// Writes `bytes` to `dir/name`, mode 0600: a temporary file, fsync, rename, directory fsync.
/// `stop` is asked at each [`WriteStep`]; `true` stops the write there with an error, as a crash
/// would.
pub fn write_atomic(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    stop: &dyn Fn(WriteStep) -> bool,
) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    crossed(stop, WriteStep::BeforeRename)?;
    fs::rename(&tmp, dir.join(name))?;
    File::open(dir)?.sync_all()?;
    crossed(stop, WriteStep::AfterRename)
}

pub fn crossed(stop: &dyn Fn(WriteStep) -> bool, step: WriteStep) -> io::Result<()> {
    match stop(step) {
        true => Err(io::Error::other(format!("stopped at {step:?}"))),
        false => Ok(()),
    }
}
