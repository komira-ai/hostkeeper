//! The real process probe over `/proc` (Linux).
//!
//! - Identity: field 22 of `<root>/<pid>/stat`, the start time in clock ticks since boot, read
//!   after the last `)` so a command name holding spaces or parentheses cannot shift the fields.
//! - The executable: `<root>/<pid>/exe` is opened (or stat'ed) through the link, which reaches
//!   the inode the process runs even when the file at its path was replaced or removed; the
//!   digest is read from that open descriptor ([`digest_open_file`]) and never from the path.
//!
//! The root is `/proc` in the daemon; the tests give a directory of their own.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::ObserveError;
use crate::ports::{ExeStat, ProcessIdentity, ProcessProbe};

/// How many bytes one read of the executable takes.
const CHUNK: usize = 64 * 1024;

/// The process probe over a `/proc` tree.
#[derive(Clone, Debug)]
pub struct ProcProbe {
    root: PathBuf,
}

impl ProcProbe {
    /// A probe of the `/proc` tree at `root`.
    pub fn new(root: &Path) -> ProcProbe {
        ProcProbe {
            root: root.to_path_buf(),
        }
    }

    fn entry(&self, pid: u32, name: &str) -> PathBuf {
        self.root.join(pid.to_string()).join(name)
    }
}

/// The error of `pid` for an I/O failure on `path`.
fn because(pid: u32, path: &Path) -> impl Fn(io::Error) -> ObserveError + '_ {
    move |e| ObserveError::Process {
        pid,
        why: format!("{}: {e}", path.display()),
    }
}

impl ProcessProbe for ProcProbe {
    fn identity(&self, pid: u32) -> Result<ProcessIdentity, ObserveError> {
        let path = self.entry(pid, "stat");
        let text = fs::read_to_string(&path).map_err(because(pid, &path))?;
        let start_time = start_time(&text).ok_or_else(|| {
            because(pid, &path)(io::Error::new(
                io::ErrorKind::InvalidData,
                "no start time in field 22",
            ))
        })?;
        Ok(ProcessIdentity { pid, start_time })
    }

    fn exe_stat(&self, pid: u32) -> Result<ExeStat, ObserveError> {
        let path = self.entry(pid, "exe");
        let meta = fs::metadata(&path).map_err(because(pid, &path))?;
        Ok(ExeStat {
            size: meta.size(),
            mtime_secs: meta.mtime(),
            mtime_nanos: meta.mtime_nsec(),
        })
    }

    fn exe_digest(&self, pid: u32) -> Result<[u8; 32], ObserveError> {
        let path = self.entry(pid, "exe");
        let file = File::open(&path).map_err(because(pid, &path))?;
        digest_open_file(&file).map_err(because(pid, &path))
    }
}

/// Field 22 of a `/proc/<pid>/stat` line: the 20th field after the command name's closing `)`.
fn start_time(stat: &str) -> Option<u64> {
    let (_, after_name) = stat.rsplit_once(')')?;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

/// The SHA-256 of the whole of an open file, read by offset from its start through the
/// descriptor: the inode that was opened is the one hashed, whatever its path names now.
pub fn digest_open_file(file: &File) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut offset = 0u64;
    loop {
        let n = file.read_at(&mut buf, offset)?;
        if n == 0 {
            let mut digest = [0u8; 32];
            digest.copy_from_slice(&hasher.finalize());
            return Ok(digest);
        }
        hasher.update(&buf[..n]);
        offset += n as u64;
    }
}
