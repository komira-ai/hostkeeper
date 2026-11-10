//! The host's record of what the daemon installed, and the links it switches
//! (docs/design/hostkeeper-design.md, section 4.7).
//!
//! The daemon keeps each service's `current` and `previous` links in its package directory, and
//! `placed.json`, which records each installed service's installer and its enablement links and
//! the tombstone of a service being uninstalled. A package manager puts a package's files where
//! they belong; nothing here links a package into the system, and the paths an earlier release
//! linked are only ever taken away.
//!
//! - [`roots`]: the directories the crate works in ([`Roots`], [`PackageDirs`]);
//! - [`fs`]: the filesystem port every read and write goes through ([`Fs`], [`RealFs`]);
//! - [`store`]: `placed.json` ([`PlacedRecord`], [`Store`]);
//! - [`archive`]: the paths an archive version of an earlier release placed, and their removal
//!   ([`Placer::take_away_archive_paths`], [`Placer::restore_archive_links`],
//!   [`Placer::forget_archive_paths`],
//!   [`Placer::remove_dangling_unit_links`]);
//! - [`placer`]: the switch of `current` and `previous`, the lock of `placed.json` and the
//!   service manager's reload ([`Placer`]);
//! - [`unit_control`]: the write side of the service manager ([`UnitControl`]);
//! - [`testing`]: the fakes ([`testing::FaultFs`], [`testing::FakeUnitControl`]).
//!
//! Nothing here reads a clock.

pub mod archive;
pub mod error;
pub mod fs;
pub mod placer;
pub mod roots;
pub mod store;
pub mod testing;
pub mod unit_control;

pub use archive::{ArchiveKind, ArchivePath, TakenAway};
pub use error::{Owner, PlacedError};
pub use fs::{FileKind, Fs, RealFs};
pub use placer::{PlacedLock, Placer};
pub use roots::{PackageDirs, Roots};
pub use store::{InstalledEntry, PlacedRecord, Store, Tombstone};
pub use unit_control::{CommandLauncher, Systemctl, UnitControl, UnitControlError};
