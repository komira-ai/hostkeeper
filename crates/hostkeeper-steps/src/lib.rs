//! The step runner of the generic host updater: the daemon's engine for one update of one
//! service (docs/design/hostkeeper-design.md, 6.2).
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
//!   of versions with `current` and `previous` that it and every package share. The update waits
//!   in its step record for the restart until the new binary has run ([`settle`]), and a
//!   version that fell back is kept in [`bad`], the local list of bad versions. On macOS the
//!   fallback decides the burst of failed starts itself, from the daemon's [`starts`] log, with
//!   [`should_fall_back`], the rule systemd's start limit keeps on Linux. [`handover`] is what a
//!   binary's start reads of a hand-over, the way a later release updates the updater without
//!   moving `current` first (docs/design/two-programs.md, 4.3), and the settlement confirms one.
//! - [`OLDEST_READER`] is the oldest release of hostkeeper that reads every file this one
//!   writes: the floor of hostkeeper's own package, and what the daemon refuses an older release
//!   of hostkeeper by ([`reader`]).
//! - [`prune`] deletes the versions of a package directory that a confirmed switch leaves
//!   behind: all but the ones `current` and `previous` name and the ones the caller protects
//!   ([`prune_plan`], [`Layout::prune`]).
//! - [`seal`] is the seal file of an unpacked package version, and [`tree_digest`] the digest
//!   it holds (both public formats, docs/package-format.md).
//! - [`force`] is the journal of a hookless `force` ([`ForceRecord`]), an optional field of the
//!   step record; a force that stops a running update flags its record `forced`, and the runner
//!   then closes it `forced_over` and launches nothing more.
//! - [`SystemdLauncher`] is the [`StepLauncher`] of the host: one transient systemd unit per
//!   step, with `RuntimeMaxSec=` and nothing else (a tool run is offline unless a fetch, and may
//!   be confined to listed paths, [`StepLauncher::run_tool`]); [`capture`] bounds what is kept of a hook's output.
//!
//! The crate names no service and reads no manifest: the runner is given the order and its
//! bounds, and runs each step through the [`StepLauncher`] it is given.

pub mod artifact;
pub mod bad;
pub mod capture;
pub mod clock;
pub mod deadman;
pub mod force;
pub mod handover;
pub mod launcher;
pub mod prune;
pub mod reader;
pub mod record;
pub mod resume;
pub mod runner;
pub mod seal;
pub mod selfupdate;
pub mod starts;
pub mod systemd;

pub use artifact::{ArtifactError, EntryKind, Staged, Tree, TreeEntry, read_tree, tree_digest};
pub use bad::{BAD_VERSIONS_FILE, BadVersions, BadVersionsError, MAX_BAD_VERSIONS};
pub use capture::{LAST_LINE_MAX, RING_BYTES, Ring};
pub use clock::{
    BootId, BootIdError, BootIdSource, Clock, PROC_BOOT_ID, ProcBootId, Stamp, SystemClock,
};
pub use deadman::{DeadMan, Watch};
pub use force::{ForceFailure, ForcePhase, ForceRecord, ForceVerdict};
pub use launcher::{
    AdditiveCall, BUILTIN_EXIT, Bound, LaunchError, StepCall, StepExit, StepLauncher, ToolCall,
    ToolExit, ToolNetwork,
};
pub use prune::{
    PRUNE_PREFIX, Protected, PruneInputs, PrunePlan, PruneReport, PruneStep, prune_plan,
};
pub use reader::OLDEST_READER;
pub use record::{CrashGate, CrashPoint, Loaded, Record, RecordError, RecordStore, WriteFault};
pub use resume::{REASON_REBOOTED, ResumeAction, resume, resume_action};
pub use runner::{Bounds, Finished, RunError, StepRunner, Switch, Toward, UpdateOrder};
pub use seal::{MAX_SEAL_BYTES, SEAL_SCHEMA, Seal, SealError, read_seal};
pub use selfupdate::{
    FollowOn, Layout, SelfUpdateError, Settled, follow_on, settle, settle_with, should_fall_back,
};
pub use starts::{DaemonStart, StartLog, StartLogError};
pub use systemd::{Captured, Program, Spawner, StepLog, SystemdLauncher, SystemdRunProcess};
