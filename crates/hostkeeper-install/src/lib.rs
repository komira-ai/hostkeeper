//! The host daemon's installers (docs/design/packages.md, section 5): the code that turns a
//! verified package into installed software and takes it away again, built into the daemon.
//!
//! - [`installer`]: the order a native installer's `stage` is given ([`StageOrder`]), and the
//!   registry of this build's installers ([`backend_of`]).
//! - [`announce`]: which installers the daemon announces, decided once at its start from its
//!   platform and the host's `os-release` (section 5.4).
//! - [`macos_pkg`]: `macos-pkg`, a service from a signed flat `.pkg` installed by `installer`
//!   (packages-repositories.md section 4.7), with its signers file, its receipts, its record and
//!   the observer's drift check, over the ports below.
//! - [`repository`]: the repository installer kind and its `apt` backend (packages-repositories.md):
//!   the host's repository entries, stage, activate, rollback, remove and drift.
//! - [`tools`]: the tools an installer runs by absolute path, behind the [`Tools`] port.
//! - [`host`]: the host's file system under a root, and the accounts its checks trust.
//! - [`lock`]: the host-wide installer lock.
//! - [`outcome`]: a step's failure, its class and its reason.
//! - [`version`]: the sealed version directory of a package on the host, published from a staging
//!   directory an installer filled, and the orphans a crashed publish left.
//! - [`mac_verify`]: the port of the macOS checks of a package's files (the quarantine attribute,
//!   the code signature).
//!
//! An installer reads no byte of a package before the core has checked its size and its
//! digest, runs no script of the package while it stages, and writes, while it stages, only into
//! the staging directory of the version. `macos-pkg`'s ports and logic are measured on Linux
//! against fakes; its real tools run on a Mac only, in `crates/hostkeeper-privileged`'s
//! `install_macos_pkg.rs` (the `macos-device` job).

pub mod announce;
pub mod host;
pub mod installer;
pub mod lock;
pub mod mac_verify;
pub mod macos_pkg;
pub mod outcome;
pub mod program;
pub mod repository;
pub mod tools;
pub mod version;

pub use announce::{Announcement, Family, announce};
pub use installer::{REGISTERED, StageOrder, backend_of};
pub use outcome::{Class, Failure, Outcome, Reason};
#[cfg(target_os = "macos")]
pub use tools::HostTools;
pub use tools::{Tool, Tools};
