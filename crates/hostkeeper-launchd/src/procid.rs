//! The observer's process probe on a Mac: who a pid is (pid and start time), and the size,
//! modification time and digest of the file it runs. The pid lookups sit behind [`PidInfo`]
//! and the device and inode reads behind [`FileIdentity`], so the logic is measured on Linux.
//!
//! `proc_pidpath` names a path, and a path can be renamed over while the process runs: the file
//! at the path is then not the one the process runs. So the probe also asks [`PidInfo::image`]
//! for the device and inode of the mapped executable itself, and a path whose (device, inode)
//! differs from it is an error (the observer's `unobserved`, `proc_error`), never a stat or a
//! digest of the new file. A Linux probe needs none of this: `/proc/<pid>/exe` opens the inode
//! the process runs, even after the path was replaced.

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
    /// The (device, inode) of the executable file `pid` runs: the vnode of its mapped image
    /// (`proc_pidinfo` region path info on macOS), whatever its path names now.
    fn image(&self, pid: u32) -> Result<(u64, u64), PidInfoError>;
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

    /// `Ok` when `at_path`, the (device, inode) the path names, is the image the process runs.
    fn runs(&self, pid: u32, path: &Path, at_path: (u64, u64)) -> Result<(), ObserveError> {
        let image = self
            .pids
            .image(pid)
            .map_err(|e| ObserveError::Process { pid, why: e.why })?;
        same_image(pid, path, at_path, image)
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
        self.runs(pid, &path, (meta.dev(), meta.ino()))?;
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
        self.runs(pid, &path, before)?;
        let file = File::open(&path).map_err(because(pid, &path))?;
        let digest = digest_open_file(&file).map_err(because(pid, &path))?;
        let after = self
            .files
            .device_inode_of(&file)
            .map_err(because(pid, &path))?;
        same_file(pid, &path, before, after)?;
        Ok(digest)
    }
}

/// `Ok` when the path and the open descriptor name the same (device, inode); otherwise the file
/// at `path` was replaced between the lookup and the read. Not generic, so its two outcomes are
/// one function's, whatever ports the probe is built with.
fn same_file(
    pid: u32,
    path: &Path,
    before: (u64, u64),
    after: (u64, u64),
) -> Result<(), ObserveError> {
    if before != after {
        return Err(ObserveError::Process {
            pid,
            why: format!("{} was replaced while it was read", path.display()),
        });
    }
    Ok(())
}

/// `Ok` when the path names the image the process runs; otherwise the path was replaced (renamed
/// over, or unlinked and written again) after the process started, and the file there is not
/// the one that runs. Not generic, for the same reason as [`same_file`].
fn same_image(
    pid: u32,
    path: &Path,
    at_path: (u64, u64),
    image: (u64, u64),
) -> Result<(), ObserveError> {
    if at_path != image {
        return Err(ObserveError::Process {
            pid,
            why: format!(
                "{} is not the file the process runs: the path is device {} inode {}, the process runs device {} inode {}",
                path.display(),
                at_path.0,
                at_path.1,
                image.0,
                image.1
            ),
        });
    }
    Ok(())
}

/// `DarwinPidInfo`: `PidInfo` over `proc_pidpath` and `proc_pidinfo` of the `libproc` crate;
/// compiled on macOS only.
#[cfg(target_os = "macos")]
pub mod darwin {
    use super::{PidInfo, PidInfoError};
    use libproc::{
        bsd_info::BSDInfo,
        proc_pid::{self, PIDInfo, PidInfoFlavor},
    };
    use std::path::PathBuf;

    /// The most memory regions [`DarwinPidInfo::image`] walks before it gives up. The main
    /// executable's text is among the first regions of a process.
    const MAX_REGIONS: usize = 4096;
    /// The file type bits of a mode, and the type of a regular file (<sys/stat.h>).
    const S_IFMT: u32 = 0o170000;
    const S_IFREG: u32 = 0o100000;

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
        /// The (device, inode) of the lowest memory region backed by a regular file: the main
        /// executable's text, which the kernel maps below dyld and the shared cache. Read from
        /// the mapped vnode, not from a path, so a file renamed over the path (or one unlinked)
        /// does not change it.
        fn image(&self, pid: u32) -> Result<(u64, u64), PidInfoError> {
            let mut address = 0u64;
            for _ in 0..MAX_REGIONS {
                let region = proc_pid::pidinfo::<RegionWithPath>(pid as i32, address)
                    .map_err(|why| PidInfoError { pid, why })?;
                let stat = &region.vnode.info.stat;
                if (u32::from(stat.mode) & S_IFMT == S_IFREG) & (stat.ino != 0) {
                    return Ok((u64::from(stat.dev), stat.ino));
                }
                let next = region
                    .region
                    .address
                    .saturating_add(region.region.size.max(1));
                if next <= address {
                    break;
                }
                address = next;
            }
            Err(PidInfoError {
                pid,
                why: "no mapped region is backed by a regular file".to_owned(),
            })
        }
    }

    // The structures `proc_pidinfo(PROC_PIDREGIONPATHINFO)` fills, as <sys/proc_info.h>
    // declares them (`proc_regionwithpathinfo` and its members). `libproc` fills a zeroed value
    // of the type it is asked for, and reads only as many bytes as the type holds.

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[expect(
        dead_code,
        reason = "the kernel's layout: each field holds its place, and only some are read"
    )]
    struct RegionInfo {
        protection: u32,
        max_protection: u32,
        inheritance: u32,
        flags: u32,
        offset: u64,
        behavior: u32,
        user_wired_count: u32,
        user_tag: u32,
        pages_resident: u32,
        pages_shared_now_private: u32,
        pages_swapped_out: u32,
        pages_dirtied: u32,
        ref_count: u32,
        shadow_depth: u32,
        share_mode: u32,
        private_pages_resident: u32,
        shared_pages_resident: u32,
        obj_id: u32,
        depth: u32,
        address: u64,
        size: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[expect(
        dead_code,
        reason = "the kernel's layout: each field holds its place, and only some are read"
    )]
    struct VinfoStat {
        dev: u32,
        mode: u16,
        nlink: u16,
        ino: u64,
        uid: u32,
        gid: u32,
        atime: i64,
        atimensec: i64,
        mtime: i64,
        mtimensec: i64,
        ctime: i64,
        ctimensec: i64,
        birthtime: i64,
        birthtimensec: i64,
        size: i64,
        blocks: i64,
        blksize: i32,
        flags: u32,
        generation: u32,
        rdev: u32,
        qspare: [i64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[expect(
        dead_code,
        reason = "the kernel's layout: each field holds its place, and only some are read"
    )]
    struct VnodeInfo {
        stat: VinfoStat,
        kind: i32,
        pad: i32,
        fsid: [i32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[expect(
        dead_code,
        reason = "the kernel's layout: each field holds its place, and only some are read"
    )]
    struct VnodeInfoPath {
        info: VnodeInfo,
        // `char vip_path[MAXPATHLEN]`, read as bytes.
        path: [u8; 1024],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RegionWithPath {
        region: RegionInfo,
        vnode: VnodeInfoPath,
    }

    impl PIDInfo for RegionWithPath {
        fn flavor() -> PidInfoFlavor {
            PidInfoFlavor::RegionPathInfo
        }
    }
}
