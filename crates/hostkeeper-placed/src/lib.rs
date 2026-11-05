//! Placed-path linking (docs/design/hostkeeper-design.md, section 4.7, "Placed paths").
//!
//! The daemon, not the hook, puts a package into the system: each file of `units/` and each
//! `[[files]]` entry becomes a symlink at its system path pointing into the service's `current`
//! directory, so one switch of `current` re-points all of them. `placed.json` records which
//! service owns each placed path, and is written ahead of every link so that a crash never
//! leaves a link the record does not list.
//!
//! - [`roots`]: the directories the crate works in ([`Roots`], [`PackageDirs`]);
//! - [`fs`]: the filesystem port every read and write goes through ([`Fs`], [`RealFs`]);
//! - [`store`]: `placed.json` ([`PlacedRecord`], [`Store`]);
//! - [`claim`]: the paths a package claims ([`Claims`]) and the rules on them;
//! - [`lint`]: the load rules that belong to linking (`[units] run`, `ExecStart=`, `WantedBy=`);
//! - [`check`]: the `path_conflict` refusal, callable before anything changes;
//! - [`placer`]: the write-ahead protocol, removal, enabling and the start-up reconciliation
//!   ([`Placer`]);
//! - [`unit_control`]: the write side of the service manager ([`UnitControl`]);
//! - [`testing`]: the fakes ([`testing::FaultFs`], [`testing::FakeUnitControl`]).
//!
//! Nothing here reads a clock.

pub mod check;
pub mod claim;
pub mod error;
pub mod fs;
pub mod lint;
pub mod placer;
pub mod roots;
pub mod store;
pub mod testing;
pub mod unit_control;

mod enable;
mod reconcile;
mod remove;

pub use check::check;
pub use claim::{Claim, Claims};
pub use error::{ClaimError, Owner, PlacedError};
pub use fs::{FileKind, Fs, RealFs};
pub use lint::LintError;
pub use placer::{Linking, PlacedLock, Placer};
pub use reconcile::{BootView, Conflict, ReconcileReport, Side, SwitchIntent, Unresolved};
pub use remove::Removal;
pub use roots::{PackageDirs, Roots};
pub use store::{Entry, PathKind, PathState, PlacedRecord, Store, Tombstone};
pub use unit_control::{CommandLauncher, Systemctl, UnitControl, UnitControlError};
