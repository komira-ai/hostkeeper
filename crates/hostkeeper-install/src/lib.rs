//! The host daemon's installers (docs/design/packages.md, section 5): the code that turns a
//! verified artifact into installed software and takes it away again, built into the daemon
//! behind one trait.
//!
//! - [`installer`]: the contract ([`InstallerSteps`]: `stage`, `activate`, `retire`, `remove`),
//!   the host's side of it ([`Host`]), and the registry of this build's installers
//!   ([`steps_of`]).
//! - [`announce`]: which installers the daemon announces, decided once at its start from its
//!   platform and the host's `os-release` (section 5.4).
//! - [`macos_pkg`]: `macos-pkg`, a service from a signed flat `.pkg` installed by `installer`
//!   (packages-repositories.md section 4.7), with its signers file, its receipts, its record and
//!   the observer's drift check, over the ports below.
//! - [`tools`]: the tools an installer runs by absolute path, behind the [`Tools`] port.
//! - [`host`]: the host's file system under a root, and the accounts its checks trust.
//! - [`lock`]: the host-wide installer lock.
//! - [`outcome`]: a step's failure, its class and its reason.
//!
//! An installer reads no byte of an artifact before the core has checked its size and its
//! digest, runs no script of the package while it stages, and writes, while it stages, only into
//! the staging directory of the version. `macos-pkg`'s ports and logic are measured on Linux
//! against fakes; its real tools run on a Mac only, in `crates/hostkeeper-privileged`'s
//! `install_macos_pkg.rs` (the `macos-device` job).

pub mod announce;
pub mod host;
pub mod installer;
pub mod lock;
pub mod macos_pkg;
pub mod outcome;
pub mod tools;

pub use announce::{Announcement, Family, announce};
pub use installer::{
    Archive, Host, HostError, InstallerSteps, REGISTERED, StageEnv, StageError, StageOrder,
    StagedVersion, steps_of,
};
pub use outcome::{Class, Failure, Outcome, Reason};
#[cfg(target_os = "macos")]
pub use tools::HostTools;
pub use tools::{Tool, Tools};
