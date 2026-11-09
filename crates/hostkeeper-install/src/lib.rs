//! The host daemon's installers (docs/design/packages.md, section 5): the code that turns a
//! verified artifact into installed software and takes it away again, built into the daemon
//! behind one trait.
//!
//! - [`installer`]: the contract ([`InstallerSteps`]: `stage`, `activate`, `retire`, `remove`),
//!   the host's side of it ([`Host`]), and the registry of this build's installers
//!   ([`steps_of`]), today `hostkeeper-archive` alone.
//! - [`announce`]: which installers the daemon announces, decided once at its start from its
//!   platform and the host's `os-release` (section 5.4).
//!
//! An installer reads no byte of an artifact before the core has checked its size, its digest
//! and its attestation (`hostkeeper-package`), runs no script of the package while it stages,
//! and writes, while it stages, only into the staging directory of the version.

pub mod announce;
pub mod installer;

pub use announce::{Announcement, Family, announce};
pub use installer::{
    Archive, Host, HostError, InstallerSteps, REGISTERED, StageEnv, StageError, StageOrder,
    StagedVersion, steps_of,
};
