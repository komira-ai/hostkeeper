//! The held halt generation of each service (design sections 6.2 and 10): the halt generation
//! a halt of the service is held under, persisted beside its step record so that a restart cannot
//! lift a halt. A generation strictly above it lifts the halt.
//!
//! The file is `<state-dir>/<service>/held-halt`: a decimal number and a newline, mode 0600,
//! written with a temporary file, fsync, rename and a directory fsync. It exists only while the
//! service's step record holds a halt (`Finished{Halt}`): the first reply that names the
//! service's generation after the halt writes the generation the reply before it named
//! ([`crate::verbs::clear_halt::heard`]), and every
//! lift removes it, the record cleared first ([`crate::verbs::clear_halt::lift`]). A service with
//! no halt has none, whatever generation the coordinator sends; one left with no halt is removed
//! at the next reply that names the service. A missing file is 0. A file that does not hold a
//! number is an error, never read as 0, and the executor runs no order of that service until it
//! reads. While the file exists its generation is only ever raised ([`HaltStore::raise`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use hostkeeper_config::names::ServiceName;

use crate::files::{self, WriteStep};

/// The file's name in the service's state directory.
pub const HELD_HALT_FILE: &str = "held-halt";

/// Why a held halt generation could not be read or raised.
#[derive(Debug, thiserror::Error)]
pub enum HaltError {
    #[error("the held halt generation of {service} cannot be read or written: {source}")]
    Io {
        service: String,
        #[source]
        source: io::Error,
    },
    #[error("the held halt generation of {service} is not a number: {text:?}")]
    Garbage { service: String, text: String },
    #[error("the held halt generation of {service} is {held}; it is never lowered to {asked}")]
    Lower {
        service: String,
        held: u64,
        asked: u64,
    },
}

/// A test's way to stop a write of the file at a [`WriteStep`], as a crash would.
pub type HaltGate = Arc<dyn Fn(WriteStep) -> bool + Send + Sync>;

/// The held halt generations, one file per service under the state directory.
pub struct HaltStore {
    state_dir: PathBuf,
    gate: Option<HaltGate>,
}

impl HaltStore {
    pub fn new(state_dir: &Path) -> HaltStore {
        HaltStore {
            state_dir: state_dir.to_path_buf(),
            gate: None,
        }
    }

    /// The same store, its writes stopped wherever `gate` says.
    pub fn with_gate(mut self, gate: HaltGate) -> HaltStore {
        self.gate = Some(gate);
        self
    }

    fn dir(&self, service: &ServiceName) -> PathBuf {
        self.state_dir.join(service.as_str())
    }

    /// The generation `service` is held under; 0 when it has none.
    pub fn get(&self, service: &ServiceName) -> Result<u64, HaltError> {
        self.read(service).map(|held| held.unwrap_or(0))
    }

    /// The generation written for `service`; `None` when no file is written.
    pub fn read(&self, service: &ServiceName) -> Result<Option<u64>, HaltError> {
        let unreadable = |source: io::Error| HaltError::Io {
            service: service.to_string(),
            source,
        };
        let text = match fs::read_to_string(self.dir(service).join(HELD_HALT_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unreadable(e)),
        };
        let digits = |t: &&str| !t.is_empty() & t.bytes().all(|b| b.is_ascii_digit());
        let number = text.strip_suffix('\n').filter(digits);
        number
            .and_then(|t| t.parse().ok())
            .map(Some)
            .ok_or_else(|| HaltError::Garbage {
                service: service.to_string(),
                text,
            })
    }

    /// When the generation of `service` was last written, in unix seconds (the file's
    /// modification time, which [`HaltStore::raise`] sets); `None` when none is written, or the
    /// time cannot be read or is before 1970.
    pub fn written_unix(&self, service: &ServiceName) -> Option<u64> {
        let modified = fs::metadata(self.dir(service).join(HELD_HALT_FILE))
            .and_then(|meta| meta.modified())
            .ok()?;
        modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs())
    }

    /// Raises the generation of `service` to `generation`, durably; the same generation again
    /// changes nothing, and a lower one is refused ([`HaltError::Lower`]) and changes nothing.
    pub fn raise(&self, service: &ServiceName, generation: u64) -> Result<(), HaltError> {
        let held = self.get(service)?;
        if generation < held {
            return Err(HaltError::Lower {
                service: service.to_string(),
                held,
                asked: generation,
            });
        }
        if generation == held {
            return Ok(());
        }
        self.hold(service, generation)
    }

    /// Writes `generation` as the one `service` is held under, durably, whatever the file held:
    /// a halt with no file written is held under it from now on.
    pub fn hold(&self, service: &ServiceName, generation: u64) -> Result<(), HaltError> {
        let dir = self.dir(service);
        let gate = |step: WriteStep| self.gate.as_ref().is_some_and(|gate| gate(step));
        let text = format!("{generation}\n");
        files::make_private(&dir)
            .and_then(|()| files::write_atomic(&dir, HELD_HALT_FILE, text.as_bytes(), &gate))
            .map_err(|source| HaltError::Io {
                service: service.to_string(),
                source,
            })
    }

    /// Removes the file of `service`, durably (the directory is synced): its halt is lifted, or it
    /// holds none. No file is no error.
    pub fn remove(&self, service: &ServiceName) -> Result<(), HaltError> {
        let dir = self.dir(service);
        let failed = |source: io::Error| HaltError::Io {
            service: service.to_string(),
            source,
        };
        match fs::remove_file(dir.join(HELD_HALT_FILE)) {
            Ok(()) => fs::File::open(&dir)
                .and_then(|d| d.sync_all())
                .map_err(failed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(failed(e)),
        }
    }
}
