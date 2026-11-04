//! The admin socket on the host: `/run/hostkeeper/admin.sock` by default, mode 0600 in a
//! directory of mode 0700, and the peer of each connection from the kernel (`SO_PEERCRED`,
//! through tokio's `peer_cred`).

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::Path;

use tokio::net::UnixStream;
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{Stream, StreamExt};

use crate::admin::Peer;
use crate::files;

/// Binds the admin socket at `path`: its directory is made, or set, mode 0700; a socket left at
/// `path` by a daemon that is gone is removed (the caller holds the state directory's lock, so
/// no live daemon owns it); anything else at `path` is refused and left as it is; the new
/// socket is mode 0600.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    files::make_private(path.parent().unwrap_or(path))?;
    let stale = match fs::symlink_metadata(path) {
        Ok(meta) => Some(meta.file_type().is_socket()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    match stale {
        None => {}
        Some(true) => fs::remove_file(path)?,
        Some(false) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a socket; it is left as it is", path.display()),
            ));
        }
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The connections of `listener`, each with the peer the kernel reports for it. Needs a tokio
/// runtime.
pub fn incoming(
    listener: UnixListener,
) -> io::Result<impl Stream<Item = io::Result<(UnixStream, Peer)>>> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;
    Ok(UnixListenerStream::new(listener).map(|accepted| accepted.map(with_peer)))
}

fn with_peer(stream: UnixStream) -> (UnixStream, Peer) {
    let uid = stream.peer_cred().ok().map(|cred| cred.uid());
    (stream, Peer { uid })
}
