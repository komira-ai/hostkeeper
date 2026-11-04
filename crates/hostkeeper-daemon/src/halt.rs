//! The held halt generation of each service (design sections 6.2 and 10): the generation of the
//! coordinator's halt clears the service is held under, persisted beside its step record so that
//! a restart cannot lift a halt.
//!
//! The file is `<state-dir>/<service>/held-halt`: a decimal number and a newline, mode 0600,
//! written with a temporary file, fsync, rename and a directory fsync. A missing file is 0. A
//! file that does not hold a number is an error, never read as 0, and the executor runs no order
//! of that service until it reads. The generation is only ever raised ([`HaltStore::raise`]);
//! nothing lowers it. Nothing in this crate raises it yet: the heartbeat loop, `clear_halt` and
//! `force` will.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        let unreadable = |source: io::Error| HaltError::Io {
            service: service.to_string(),
            source,
        };
        let text = match fs::read_to_string(self.dir(service).join(HELD_HALT_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(unreadable(e)),
        };
        let digits = |t: &&str| !t.is_empty() & t.bytes().all(|b| b.is_ascii_digit());
        let number = text.strip_suffix('\n').filter(digits);
        number
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| HaltError::Garbage {
                service: service.to_string(),
                text,
            })
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
}
