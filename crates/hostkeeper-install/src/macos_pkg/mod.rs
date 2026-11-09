//! The `macos-pkg` installer (packages-repositories.md section 4.7): a service from a signed flat
//! `.pkg`, installed by `installer` as root.
//!
//! An artifact installer, as `tarball` is: the core fetches and proves the `.pkg` (size, digest,
//! attestation) before any step here reads a byte of it, gives `stage` the verified file and a
//! staging directory, then walks, checks (`MacVerify`) and seals what `stage` made, and switches
//! `current` after `activate`. The steps:
//!
//! | Step | Here |
//! |---|---|
//! | [`MacosPkg::stage`] | The identifier no other service records; `pkgutil --check-signature` against the host's [`signers`]; `pkgutil --expand-full`; the [`shape`] of the expansion; the `Bom` against the payload, and the jobs ([`payload`]); `artifact`, `tree/` and `macos-pkg/{PackageInfo,Bom}` in the staging directory |
//! | [`MacosPkg::activate`] | Under the [`InstallerLock`]: the parents of the payload paths safe, no payload file another receipt lists, the record pending, `installer -allowUntrusted -pkg <dir>/artifact -target /`, the receipt's version and every installed file proved against `tree/`, the files of the version it replaces that this one does not list deleted, then the created directories left empty, the `run` labels enabled and a dropped one disabled, the record done. A rollback is the `activate` of the kept previous version. Drift found on the way is reported, and repaired by the install |
//! | [`MacosPkg::retire`] | The version directory and its seal deleted; no tool runs |
//! | [`MacosPkg::remove`] | Under the lock: every file and link the receipt or a kept version's `Bom` lists, changed or not, but one another receipt lists or one under a linked parent; the created directories left empty, deepest first; `pkgutil --forget` |
//!
//! [`drift`] is the observer's check at every heartbeat, with no tool run.

pub mod activate;
pub mod drift;
pub mod payload;
pub mod receipt;
pub mod record;
pub mod remove;
pub mod shape;
pub mod signature;
pub mod signers;
pub mod stage;
pub mod xml;

use std::path::Path;
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_launchd::Label;

use crate::host::Host;
use crate::lock::InstallerLock;
use crate::outcome::{Failure, Reason};
use crate::tools::{Tool, ToolOutput, Tools};
use record::Records;
use signers::Signers;

pub use activate::Activated;
pub use drift::{Drift, Seen};

/// The kept, verified `.pkg` in a version directory.
pub const ARTIFACT: &str = "artifact";
/// The component's payload in a version directory, standing for `/`.
pub const TREE: &str = "tree";
/// The component's `PackageInfo` and `Bom` in a version directory.
pub const META: &str = "macos-pkg";

/// What an order says that `macos-pkg` acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub service: ServiceName,
    /// The order's `desired.version`, which the package's `PackageInfo` must carry.
    pub version: Version,
    /// The entry's manifest's `package`: the component's identifier.
    pub identifier: String,
    /// The entry's manifest's `run`: the launchd labels of the jobs the package installs.
    pub run: Vec<Label>,
    pub bounds: Bounds,
}

/// The bounds of the installer's steps (the manifest's `installer_bounds`); each tool run of a step
/// is waited for at most the step's bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub stage: Duration,
    pub activate: Duration,
    pub remove: Duration,
}

/// The check before `stage`: a `macos-pkg` entry is an artifact entry, which names no repository
/// and no package version (`Failed`, `release`, `order_invalid`).
pub fn check_entry(has_repository: bool, has_package_version: bool) -> Result<(), Failure> {
    match (has_repository, has_package_version) {
        (false, false) => Ok(()),
        (true, _) => Err(Failure::new(
            Reason::OrderInvalid,
            "a macos-pkg entry names a repository",
        )),
        (false, true) => Err(Failure::new(
            Reason::OrderInvalid,
            "a macos-pkg entry names a package version",
        )),
    }
}

/// Whether a Mac announces `macos-pkg`: `installer` and `pkgutil` are present under `host`, and
/// the signers file named at least one signer (a file the loader refused stopped the start).
pub fn announces(host: &Host, signers: Option<&Signers>) -> bool {
    let present = |tool: Tool| host.real(tool.path()).is_file();
    signers.is_some() & present(Tool::Installer) & present(Tool::Pkgutil)
}

/// The installer, over the host's tools, file system, records, lock and signers.
pub struct MacosPkg<'a> {
    pub tools: &'a dyn Tools,
    pub host: &'a Host,
    pub records: &'a dyn Records,
    pub lock: &'a InstallerLock,
    pub signers: &'a Signers,
}

impl MacosPkg<'_> {
    /// Runs `tool` with `args`, at most `deadline`; a tool that cannot be run is `io_error`.
    fn run(&self, tool: Tool, args: &[String], deadline: Duration) -> Result<ToolOutput, Failure> {
        self.tools
            .run(tool, args, deadline)
            .map_err(|e| Failure::new(Reason::IoError, format!("{}: {e}", tool.path())))
    }

    /// The receipt of the order's identifier, `None` when macOS has none.
    fn receipt(
        &self,
        order: &Order,
        deadline: Duration,
    ) -> Result<Option<receipt::Receipt>, Failure> {
        let out = self.run(
            Tool::Pkgutil,
            &receipt::pkg_info_args(&order.identifier),
            deadline,
        )?;
        if out.code != Some(0) {
            let said = format!(
                "{}{}",
                out.stderr_text(),
                String::from_utf8_lossy(&out.stdout)
            );
            return match said.contains(receipt::NO_RECEIPT) {
                true => Ok(None),
                false => Err(Failure::new(
                    Reason::IoError,
                    format!(
                        "pkgutil --pkg-info-plist exited {:?}: {}",
                        out.code,
                        said.trim()
                    ),
                )),
            };
        }
        receipt::parse_pkg_info(&out.stdout)
            .map(Some)
            .map_err(|e| Failure::new(Reason::IoError, format!("pkgutil --pkg-info-plist {e}")))
    }

    /// The identifiers of the receipts that list the file at the logical `path`.
    fn owners(&self, path: &str, deadline: Duration) -> Result<Vec<String>, Failure> {
        let out = self.run(Tool::Pkgutil, &receipt::file_info_args(path), deadline)?;
        if out.code != Some(0) {
            return Err(Failure::new(
                Reason::IoError,
                format!("pkgutil --file-info-plist {path} exited {:?}", out.code),
            ));
        }
        receipt::parse_file_info(&out.stdout)
            .map_err(|e| Failure::new(Reason::IoError, format!("pkgutil --file-info-plist {e}")))
    }
}

/// `path` as a tool's argument; a path that is not UTF-8 is `io_error`.
fn arg(path: &Path) -> Result<String, Failure> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure::new(Reason::IoError, format!("{} is not UTF-8", path.display())))
}

/// An I/O error of `what` as `io_error`.
fn io(what: &str, e: std::io::Error) -> Failure {
    Failure::new(Reason::IoError, format!("{what}: {e}"))
}
