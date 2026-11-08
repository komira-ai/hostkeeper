//! `hostkeeper self-update-seed --state-dir <dir> --binary <file>`: the first binary of the
//! binary directory of hostkeeper's own update (design section 6.6), which the package unit
//! (`packaging/linux/units/hostkeeper.service`) starts through `current`
//! (`<state-dir>/hostkeeper/bin/current`, [`binary_dir`]). That unit runs it before every start
//! (`ExecStartPre=-`) with the binary the package installed; a host that has never updated
//! hostkeeper has no `current` yet, and its first start needs one. The bootstrap unit of the deb
//! and the rpm and a Mac's launchd job start the packaged binary instead, whose `hostkeeper
//! daemon` does the same seed itself before it execs `current` ([`crate::packaged`]).
//!
//! When `current` starts an executable file, the seed writes nothing, takes no lock and exits 0:
//! after the first start, and after every self-update, `current` is the update's, and the seed
//! never moves it. Otherwise, under the state directory's lock (a running daemon holds it, and
//! the seed then writes nothing and exits [`crate::start::EXIT_LOCKED`]):
//!
//! 1. the state directory of the service `hostkeeper` and the binary directory are made, mode
//!    0700 ([`Layout`]'s rule: root's and closed to group and others);
//! 2. a `current` that is there and starts nothing (a link to a version that is gone, or to what
//!    is not an executable file) is removed;
//! 3. `binary` is staged as the version of this binary, `versions/<version>`
//!    ([`Layout::stage`]), unless an executable file is there already;
//! 4. `current` is pointed at it ([`Layout::switch`]).
//!
//! Each step can run again, so a seed a crash cut short is finished by the next start. It exits
//! 0 when `current` starts a binary, and [`EXIT_FAILED`] with a message naming the cause when it
//! could not make it so (the binary cannot be read, which is checked before anything is changed;
//! the version is the one `previous` names but its file is gone).

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_steps::Layout;

use crate::files;
use crate::selfupdate::binary_dir;
use crate::start::{EXIT_FAILED, StartError, lock_state_dir};
use crate::verbs::force::SELF_SERVICE;

/// What the seed did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seeded {
    /// `current` started an executable file already: nothing was written.
    InPlace,
    /// The binary was put in as the version of this binary, and `current` points at it.
    Seeded,
}

/// Why the seed could not give `current` a binary to start.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    /// The state directory's lock is held by a daemon, or cannot be taken.
    #[error(transparent)]
    Lock(#[from] StartError),
    /// A directory or the binary could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SeedError {
    /// The exit status of the verb for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            SeedError::Lock(error) => error.exit_code(),
            SeedError::Io { .. } => EXIT_FAILED,
        }
    }
}

/// Whether `path`, followed through its links, is a file with an execute bit.
fn starts(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() & (meta.permissions().mode() & 0o111 != 0))
}

/// Seeds the binary directory under `state_dir` with `binary` as version `version` (the module's
/// steps), when its `current` starts nothing.
pub fn seed(state_dir: &Path, binary: &Path, version: &str) -> Result<Seeded, SeedError> {
    let layout = Layout::new(&binary_dir(state_dir));
    if starts(&layout.current_path()) {
        return Ok(Seeded::InPlace);
    }
    let _lock = lock_state_dir(state_dir)?;
    let at = |path: &Path| {
        let path = path.to_path_buf();
        move |source| SeedError::Io { path, source }
    };
    // The binary is read before anything is changed, so that one that is not there is named.
    fs::File::open(binary).map_err(at(binary))?;
    let service = state_dir.join(SELF_SERVICE);
    files::make_private(&service).map_err(at(&service))?;
    files::make_private(layout.dir()).map_err(at(layout.dir()))?;
    let current = layout.current_path();
    if fs::symlink_metadata(&current).is_ok() {
        fs::remove_file(&current).map_err(at(&current))?;
    }
    let staged = layout.version_path(version);
    if !starts(&staged) {
        layout.stage(version, binary).map_err(at(&staged))?;
    }
    layout.switch(version).map_err(at(&current))?;
    Ok(Seeded::Seeded)
}

/// The verb: [`seed`] with this binary's version, its error printed.
pub fn main(state_dir: &Path, binary: &Path) -> ExitCode {
    match seed(state_dir, binary, env!("CARGO_PKG_VERSION")) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hostkeeper self-update-seed: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
