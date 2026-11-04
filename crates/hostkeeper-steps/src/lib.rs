//! The step runner of the generic host updater: the root part's engine for one update of one
//! service (docs/design/host-updates.md, 3.2 to 3.5).
//!
//! - [`StepRunner`] runs the five steps of the hook contract through a [`StepLauncher`], writes
//!   the [`record::Record`] before and after each step, resumes from it after a crash, and reads
//!   exit codes as outcomes through [`hostkeeper_hook::next`].
//! - [`Staged`] is the verified artifact: nothing from a release runs unless the tree still has
//!   the digest it had when it was verified.
//! - [`resume_action`] is the table of what becomes of an unfinished update at the daemon's
//!   start, after a death of the daemon or a reboot, and [`resume`] rewrites the record for it.
//! - [`DeadMan`] enforces the bound of the running step on a thread of its own.
//! - [`selfupdate`] is the updater's own update with its fallback, and [`Layout`] the directory
//!   of versions with `current` and `previous` that it and every package share.
//! - [`seal`] is the seal file of an unpacked package version, and [`tree_digest`] the digest
//!   it holds (both public formats, docs/package-format.md).
//!
//! The crate names no service and reads no declaration: the launcher that runs a step under the
//! rights of a declaration, and the loader of declarations, plug in through [`StepLauncher`].

pub mod artifact;
pub mod clock;
pub mod deadman;
pub mod launcher;
pub mod record;
pub mod resume;
pub mod runner;
pub mod seal;
pub mod selfupdate;

pub use artifact::{ArtifactError, EntryKind, Staged, Tree, TreeEntry, read_tree, tree_digest};
pub use clock::{Clock, Stamp, SystemClock};
pub use deadman::{DeadMan, Watch};
pub use launcher::{AdditiveCall, LaunchError, StepCall, StepExit, StepLauncher};
pub use record::{CrashGate, CrashPoint, Loaded, Record, RecordError, RecordStore, WriteFault};
pub use resume::{REASON_REBOOTED, ResumeAction, resume, resume_action};
pub use runner::{Bounds, Finished, RunError, StepRunner, Switch, Toward, UpdateOrder};
pub use seal::{MAX_SEAL_BYTES, SEAL_SCHEMA, Seal, SealError, read_seal};
pub use selfupdate::Layout;
