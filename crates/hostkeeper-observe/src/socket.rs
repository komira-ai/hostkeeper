//! The registration socket kbf's agent served (`/run/updater/register.sock`), on which a service
//! said it was alive. hostkeeper does not carry it: the daemon observes each service itself.
//! A daemon that starts on a host moved from kbf's agent removes a socket file left there, before
//! it touches any unit (design sections 5.8 and 6.5), so no client connects to a dead socket and
//! takes the silence for an answer. The socket is never made again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The socket's file name in the run directory.
pub const REGISTER_SOCKET: &str = "register.sock";

/// The stale socket is there and could not be removed.
#[derive(Debug, thiserror::Error)]
#[error("cannot remove the stale registration socket {}: {source}", .path.display())]
pub struct UnlinkError {
    pub path: PathBuf,
    #[source]
    pub source: io::Error,
}

/// Removes [`REGISTER_SOCKET`] from `run_dir` (the daemon's run directory, from its config):
/// a socket, or any other file kind that can be unlinked. `Ok(true)` when one was removed,
/// `Ok(false)` when there was none; any other failure is `Err`, and the daemon refuses to start
/// on it.
pub fn unlink_stale_registration_socket(run_dir: &Path) -> Result<bool, UnlinkError> {
    let path = run_dir.join(REGISTER_SOCKET);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(UnlinkError { path, source }),
    }
}
