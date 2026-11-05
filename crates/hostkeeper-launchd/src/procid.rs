//! The observer's process probe on a Mac: who a pid is (pid and start time), and the size,
//! modification time and digest of the file it runs. The pid lookups sit behind [`PidInfo`]
//! and the device and inode reads behind [`FileIdentity`], so the logic is measured on Linux.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{ExeStat, ObserveError, ProcessIdentity, ProcessProbe};

/// A pid lookup that failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("process {pid}: {why}")]
pub struct PidInfoError {
    /// The pid that was asked about.
    pub pid: u32,
    /// What the system said.
    pub why: String,
}

/// What the system can say about a pid.
pub trait PidInfo: Send + Sync {
    /// The path of the executable `pid` runs (`proc_pidpath` on macOS).
    fn path(&self, pid: u32) -> Result<PathBuf, PidInfoError>;
    /// When `pid` started, in microseconds since the epoch (`proc_pidinfo` BSD info on macOS).
    fn start_time(&self, pid: u32) -> Result<u64, PidInfoError>;
}

/// The device and inode of a file, by path and by open descriptor.
pub trait FileIdentity: Send + Sync {
    /// The (device, inode) of `path`, following links.
    fn device_inode(&self, path: &Path) -> io::Result<(u64, u64)>;
    /// The (device, inode) of an open file.
    fn device_inode_of(&self, file: &File) -> io::Result<(u64, u64)>;
}

/// [`FileIdentity`] over `std::fs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFileIdentity;

impl FileIdentity for StdFileIdentity {
    fn device_inode(&self, path: &Path) -> io::Result<(u64, u64)> {
        let meta = fs::metadata(path)?;
        Ok((meta.dev(), meta.ino()))
    }

    fn device_inode_of(&self, file: &File) -> io::Result<(u64, u64)> {
        let meta = file.metadata()?;
        Ok((meta.dev(), meta.ino()))
    }
}

/// The process probe over [`PidInfo`] and [`FileIdentity`].
#[derive(Clone, Debug)]
pub struct LaunchdProcessProbe<P, F> {
    pids: P,
    files: F,
}

impl<P: PidInfo, F: FileIdentity> LaunchdProcessProbe<P, F> {
    /// A probe that asks `pids` about processes and `files` about their executables.
    pub fn new(pids: P, files: F) -> Self {
        LaunchdProcessProbe { pids, files }
    }

    fn exe_path(&self, pid: u32) -> Result<PathBuf, ObserveError> {
        self.pids
            .path(pid)
            .map_err(|e| ObserveError::Process { pid, why: e.why })
    }
}

fn because(pid: u32, path: &Path) -> impl Fn(io::Error) -> ObserveError + '_ {
    move |e| ObserveError::Process {
        pid,
        why: format!("{}: {e}", path.display()),
    }
}

impl<P: PidInfo, F: FileIdentity> ProcessProbe for LaunchdProcessProbe<P, F> {
    fn identity(&self, pid: u32) -> Result<ProcessIdentity, ObserveError> {
        let start_time = self
            .pids
            .start_time(pid)
            .map_err(|e| ObserveError::Process { pid, why: e.why })?;
        Ok(ProcessIdentity { pid, start_time })
    }

    fn exe_stat(&self, pid: u32) -> Result<ExeStat, ObserveError> {
        let path = self.exe_path(pid)?;
        let meta = fs::metadata(&path).map_err(because(pid, &path))?;
        Ok(ExeStat {
            size: meta.size(),
            mtime_secs: meta.mtime(),
            mtime_nanos: meta.mtime_nsec(),
        })
    }

    fn exe_digest(&self, pid: u32) -> Result<[u8; 32], ObserveError> {
        let path = self.exe_path(pid)?;
        let before = self
            .files
            .device_inode(&path)
            .map_err(because(pid, &path))?;
        let file = File::open(&path).map_err(because(pid, &path))?;
        let digest = digest_open_file(&file).map_err(because(pid, &path))?;
        let after = self
            .files
            .device_inode_of(&file)
            .map_err(because(pid, &path))?;
        if before != after {
            return Err(ObserveError::Process {
                pid,
                why: format!("{} was replaced while it was read", path.display()),
            });
        }
        Ok(digest)
    }
}

/// `DarwinPidInfo`: `PidInfo` over `proc_pidpath` and `proc_pidinfo` of the `libproc` crate;
/// compiled on macOS only.
#[cfg(target_os = "macos")]
pub mod darwin {
    use super::{PidInfo, PidInfoError};
    use libproc::{bsd_info::BSDInfo, proc_pid};
    use std::path::PathBuf;
    #[derive(Clone, Copy, Debug, Default)]
    pub struct DarwinPidInfo;
    impl PidInfo for DarwinPidInfo {
        fn path(&self, pid: u32) -> Result<PathBuf, PidInfoError> {
            proc_pid::pidpath(pid as i32)
                .map(Into::into)
                .map_err(|why| PidInfoError { pid, why })
        }
        fn start_time(&self, pid: u32) -> Result<u64, PidInfoError> {
            proc_pid::pidinfo::<BSDInfo>(pid as i32, 0)
                .map(|info| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
                .map_err(|why| PidInfoError { pid, why })
        }
    }
}
