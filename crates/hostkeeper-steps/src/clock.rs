//! Time as an input: the runner and the dead-man read the clock through this
//! trait, so a test drives them with a clock that moves only when told to.
//!
//! The boot id the clock is read under comes from a [`BootIdSource`], the port of the host's
//! kernel: [`ProcBootId`] on Linux here, `kern.bootsessionuuid` on a Mac in `hostkeeper-launchd`.
//! A boot id is never empty: when it cannot be read the clock says so ([`BootIdError`]), and a
//! stamp taken then is of an unknown boot, which is never the same boot as any other.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A monotonic clock with a boot id.
pub trait Clock: Send + Sync {
    /// Whole seconds on a monotonic clock. Comparable only under the same [`Clock::boot_id`].
    fn mono_secs(&self) -> u64;
    /// An id that changes when the host boots (the kernel's boot id), or why it cannot be read.
    fn boot_id(&self) -> Result<BootId, BootIdError>;
    /// Waits for `secs` seconds.
    fn sleep_secs(&self, secs: u64);
}

/// The port of the host's boot id: where [`SystemClock`] reads it.
pub trait BootIdSource: Send + Sync {
    /// This boot's id, or why it cannot be read.
    fn read(&self) -> Result<BootId, BootIdError>;
}

/// The id of one boot of the host: not empty, without surrounding white space.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootId(String);

impl BootId {
    /// `text` trimmed, read from `from`: refused when nothing is left.
    pub fn new(from: &'static str, text: &str) -> Result<BootId, BootIdError> {
        let id = text.trim();
        match id.is_empty() {
            true => Err(BootIdError::Empty { from }),
            false => Ok(BootId(id.to_owned())),
        }
    }

    /// `text` trimmed, read from `from`: refused when it is empty or not a UUID
    /// (`8-4-4-4-12` hexadecimal digits, either case), the form of the kernels' boot ids.
    pub fn uuid(from: &'static str, text: &str) -> Result<BootId, BootIdError> {
        let id = BootId::new(from, text)?;
        match is_uuid(id.as_str()) {
            true => Ok(id),
            false => Err(BootIdError::NotUuid { from, text: id.0 }),
        }
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BootId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `text` is a UUID in its text form: five groups of 8, 4, 4, 4 and 12 hexadecimal
/// digits joined by `-`.
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    (lengths == [8, 4, 4, 4, 12])
        & groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Why the boot id cannot be had. The caller decides what an unknown boot means; a stamp of one
/// is never of the same boot as another ([`Stamp::same_boot`]).
#[derive(Debug, thiserror::Error)]
pub enum BootIdError {
    /// The source could not be read.
    #[error("the boot id cannot be read from {from}: {source}")]
    Read {
        from: &'static str,
        #[source]
        source: io::Error,
    },
    /// The source holds nothing but white space.
    #[error("the boot id read from {from} is empty")]
    Empty { from: &'static str },
    /// The source holds something that is not a UUID.
    #[error("the boot id read from {from} is not a UUID: {text:?}")]
    NotUuid { from: &'static str, text: String },
}

/// The file of Linux's boot id.
pub const PROC_BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// The boot id of Linux: the UUID in a file, [`PROC_BOOT_ID`] on the host.
#[derive(Clone, Debug)]
pub struct ProcBootId {
    path: PathBuf,
}

impl ProcBootId {
    /// The host's: [`PROC_BOOT_ID`].
    pub fn host() -> ProcBootId {
        ProcBootId::at(Path::new(PROC_BOOT_ID))
    }

    /// The boot id in the file at `path`, for a test's fixture.
    pub fn at(path: &Path) -> ProcBootId {
        ProcBootId {
            path: path.to_owned(),
        }
    }
}

impl BootIdSource for ProcBootId {
    fn read(&self) -> Result<BootId, BootIdError> {
        let from = "the kernel's boot_id file";
        let text = std::fs::read_to_string(&self.path)
            .map_err(|source| BootIdError::Read { from, source })?;
        BootId::uuid(from, &text)
    }
}

/// A point on the monotonic clock, with the boot it was read in: `None` when the boot id could
/// not be read then. On disk an unknown boot is the empty `boot_id` older releases wrote on a
/// Mac, so the format is unchanged and their stamps read as of an unknown boot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    #[serde(with = "boot_id_text")]
    pub boot_id: Option<BootId>,
    pub mono_secs: u64,
}

impl Stamp {
    /// Now, on `clock`; of an unknown boot when its boot id cannot be read.
    pub fn now(clock: &dyn Clock) -> Stamp {
        Stamp {
            boot_id: clock.boot_id().ok(),
            mono_secs: clock.mono_secs(),
        }
    }

    /// Whether this stamp and `other` were read in the same boot: both boot ids are known and
    /// equal. A stamp of an unknown boot may be of any boot, so it is of the same boot as none.
    pub fn same_boot(&self, other: &Stamp) -> bool {
        matches!((&self.boot_id, &other.boot_id), (Some(a), Some(b)) if a == b)
    }

    /// Seconds from this stamp to now, `None` when the host may have booted since: the boot ids
    /// differ, or either is unknown. The time that passed before a reboot is unknown; a caller
    /// that bounds a window by this stamp restarts the window.
    pub fn elapsed_secs(&self, clock: &dyn Clock) -> Option<u64> {
        let now = Stamp::now(clock);
        self.same_boot(&now)
            .then(|| now.mono_secs.saturating_sub(self.mono_secs))
    }
}

/// [`Stamp::boot_id`] on disk: the id, or `""` for an unknown boot.
mod boot_id_text {
    use super::*;

    pub fn serialize<S: Serializer>(id: &Option<BootId>, out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(id.as_ref().map_or("", BootId::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Option<BootId>, D::Error> {
        let text = String::deserialize(input)?;
        Ok(BootId::new("a stamp", &text).ok())
    }
}

/// The host's clock: the monotonic clock of the kernel, under the boot id of `boot`.
pub struct SystemClock {
    boot: Box<dyn BootIdSource>,
}

impl SystemClock {
    /// The host's monotonic clock, its boot id read from `boot`.
    pub fn new(boot: Box<dyn BootIdSource>) -> SystemClock {
        SystemClock { boot }
    }
}

impl Clock for SystemClock {
    fn mono_secs(&self) -> u64 {
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        u64::try_from(now.tv_sec).unwrap_or(0)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        self.boot.read()
    }

    fn sleep_secs(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}
