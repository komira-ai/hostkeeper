//! The step runner of the generic host updater: the root part's engine for one update of one
//! service (docs/design/host-updates.md, 3.2 to 3.5).
//!
//! - [`StepRunner`] runs the five steps of the hook contract through a [`StepLauncher`], writes
//!   the [`record::Record`] before and after each step, resumes from it after a crash, and reads
//!   exit codes as outcomes through [`hostkeeper_hook::next`].
//! - [`Staged`] is the verified artifact: nothing from a release runs unless the tree still has
//!   the digest it had when it was verified.
//! - [`DeadMan`] enforces the bound of the running step on a thread of its own.
//! - [`selfupdate`] is the updater's own update with its fallback.
//! - [`SystemdLauncher`] is the [`StepLauncher`] of the host: one transient systemd unit per
//!   step, with `RuntimeMaxSec=` and nothing else; [`capture`] bounds what is kept of a hook's
//!   output.
//!
//! The crate names no service and reads no manifest: the runner is given the order and its
//! bounds, and runs each step through the [`StepLauncher`] it is given.

pub mod artifact;
pub mod capture;
pub mod clock;
pub mod deadman;
pub mod launcher;
pub mod record;
pub mod runner;
pub mod selfupdate;
pub mod systemd;

pub use artifact::{ArtifactError, Staged, tree_digest, tree_digest_with};
pub use capture::{LAST_LINE_MAX, RING_BYTES, Ring};
pub use clock::{Clock, Stamp, SystemClock};
pub use deadman::{DeadMan, Watch};
pub use launcher::{Bound, LaunchError, StepCall, StepExit, StepLauncher};
pub use record::{CrashGate, CrashPoint, Loaded, Record, RecordError, RecordStore};
pub use runner::{Bounds, Finished, RunError, StepRunner, UpdateOrder};
pub use systemd::{Captured, Program, Spawner, StepLog, SystemdLauncher, SystemdRunProcess};
