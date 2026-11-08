//! `hostkeeper daemon` started from the packaged binary (docs/design/two-programs.md, section
//! 2.1): when the running image is one of the two paths a package installs it at
//! ([`PACKAGED_BINARIES`]: the deb's and the rpm's `/usr/bin/hostkeeper`, the pkg's
//! `/usr/local/bin/hostkeeper`), the start, before it reads its config or writes anything else:
//!
//! 1. seeds the binary directory of hostkeeper's own update with the running image, as
//!    `hostkeeper self-update-seed --binary <image>` does ([`crate::seed`]): nothing is written
//!    when `current` starts an executable file, and otherwise, under the state directory's lock
//!    (released before the next step), the image is put in as this binary's version and
//!    `current` points at it;
//! 2. on a host whose service manager starts the packaged binary at every start (OpenRC, whose
//!    init script runs it under supervise-daemon), judges whether the update of hostkeeper that
//!    waits for its restart has spent its starts, and if so puts the previous binary back, as the
//!    fallback job of a Mac does ([`crate::fallback::fallback_after_burst`], on the clock the
//!    caller gives): what it finds is said on standard error, and a fallback that cannot judge or
//!    write does not stop the start;
//! 3. `exec`s `current` with its own arguments, so the binary the self-update switched to runs
//!    as the daemon in this process: the process id stays, and with it what the service manager
//!    set (systemd's notify socket and watchdog, supervise-daemon's notify socket).
//!
//! The trigger is the image's path and nothing else. A binary that runs from
//! `versions/<version>` (through `current`, or by hand) is never one of the two, so it never
//! execs and the exec happens at most once. The seed or the exec that fails ends the start with
//! the error, nothing else run: the seed's exit status ([`crate::seed::SeedError::exit_code`],
//! [`crate::start::EXIT_LOCKED`] when a daemon holds the state directory), and
//! [`crate::start::EXIT_FAILED`] for an exec that returned.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_steps::{Clock, Layout};

use crate::fallback::{Judged, fallback_after_burst};
use crate::seed::{SeedError, seed};
use crate::selfupdate::binary_dir;
use crate::start::EXIT_FAILED;

/// Where a package installs the `hostkeeper` binary: the deb's and the rpm's path, and the
/// macOS pkg's.
pub const PACKAGED_BINARIES: [&str; 2] = ["/usr/bin/hostkeeper", "/usr/local/bin/hostkeeper"];

/// Runs a binary with arguments in place of this process. It returns only when that failed, with
/// the error.
pub type Exec<'a> = &'a dyn Fn(&Path, &[OsString]) -> io::Error;

/// Why a start from the packaged binary did not become the daemon `current` names.
#[derive(Debug, thiserror::Error)]
pub enum PackagedError {
    /// `current` could not be given a binary to start.
    #[error(transparent)]
    Seed(#[from] SeedError),
    /// `current` could not be run in place of this process.
    #[error("{} could not be run in place of the packaged binary: {source}", path.display())]
    Exec {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl PackagedError {
    /// The exit status of `hostkeeper daemon` for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            PackagedError::Seed(error) => error.exit_code(),
            PackagedError::Exec { .. } => EXIT_FAILED,
        }
    }
}

/// [`PACKAGED_BINARIES`] as paths.
pub fn host_binaries() -> [&'static Path; 2] {
    PACKAGED_BINARIES.map(Path::new)
}

/// The start of `hostkeeper daemon` of version `version` from the image `image` with `args`, the
/// arguments after the program's name: `None` when `image` is none of `packaged`, and the daemon
/// starts in this process. Otherwise the module's steps, with `exec` in place of the process's
/// own, the burst judged on `burst` when it is given (step 2: an OpenRC host) and not at all
/// otherwise: what returns is why they could not end in `current`.
pub fn start(
    image: &Path,
    packaged: &[&Path],
    state_dir: &Path,
    version: &str,
    args: &[OsString],
    burst: Option<&dyn Clock>,
    exec: Exec<'_>,
) -> Option<PackagedError> {
    if !packaged.contains(&image) {
        return None;
    }
    if let Err(error) = seed(state_dir, image, version) {
        return Some(error.into());
    }
    let dir = binary_dir(state_dir);
    if let Some(clock) = burst
        && let Some(said) = judged(&fallback_after_burst(&dir, clock))
    {
        eprintln!("hostkeeper daemon: {said}");
    }
    let current = Layout::new(&dir).current_path();
    let source = exec(&current, args);
    Some(PackagedError::Exec {
        path: current,
        source,
    })
}

/// The process's own exec: `path` run with `args` in place of this process, which keeps its id
/// and its environment. It returns only when the exec failed.
pub fn exec(path: &Path, args: &[OsString]) -> io::Error {
    Command::new(path).args(args).exec()
}

/// What the start says of the burst it judged: the previous binary put back, or why it could not
/// judge; nothing when nothing waits, the update has starts left, or the state directory is busy.
pub fn judged(judged: &Result<Judged, crate::fallback::FallbackError>) -> Option<String> {
    match judged {
        Ok(Judged::FellBack(_)) => Some(
            "the update of hostkeeper spent its starts: the previous binary is back".to_owned(),
        ),
        Ok(Judged::NotPending | Judged::Busy(_) | Judged::NotYet { .. }) => None,
        Err(error) => Some(format!(
            "the burst of the update of hostkeeper is not judged: {error}"
        )),
    }
}
