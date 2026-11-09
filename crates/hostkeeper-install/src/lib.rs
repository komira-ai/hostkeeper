//! The daemon's built-in installers (packages design section 5.1): the code that turns a
//! verified artifact into installed software, and takes it away again, in four steps (`stage`,
//! `activate`, `retire`, `remove`) with the contract of a hook's: idempotent, bounded, ending in
//! an outcome with a class and a reason.
//!
//! - [`macos_pkg`]: `macos-pkg`, a service from a signed flat `.pkg` installed by `installer`
//!   (packages-repositories.md section 4.7), with its signers file, its receipts, its record and
//!   the observer's drift check.
//! - [`tools`]: the tools an installer runs by absolute path, behind the [`Tools`] port.
//! - [`host`]: the host's file system under a root, and the accounts its checks trust.
//! - [`lock`]: the host-wide installer lock.
//! - [`outcome`]: a step's failure, its class and its reason.
//!
//! Everything here is ports and logic measured on Linux against fakes; the real tools run on a
//! Mac only, in `crates/hostkeeper-privileged`'s `install_macos_pkg.rs` (the `macos-device` job).

pub mod host;
pub mod lock;
pub mod macos_pkg;
pub mod outcome;
pub mod tools;

pub use outcome::{Class, Failure, Outcome, Reason};
#[cfg(target_os = "macos")]
pub use tools::HostTools;
pub use tools::{Tool, Tools};
