//! The `macos` kind of the maintenance hook on a Mac (docs/design/maintenance-updates.md, sections
//! 5.2, 5.5 and 5.6). Its `prepare` is the stage ([`Stage`]): the free space of the data volume
//! checked, then `softwareupdate --download` of each update, or `--fetch-full-installer` of a major
//! upgrade the order allows, nothing installed. Its other steps are the apply ([`Apply`]), on the
//! order's path `install` alone: `cutover` checks the update account's file and FileVault, chooses
//! the install from the Mac's own listings and keeps it; the kind's own restart, which the hook
//! runs once it recorded the reboot under way, installs it with the update account's password on
//! the program's standard input (`softwareupdate --install ... --restart`, or the installer app's
//! `startosinstall`), which restarts the Mac, or, with FileVault on, a minor update's
//! `softwareupdate --install` without `--restart` and then `fdesetup authrestart -inputplist`;
//! `after_reboot` reads the build; `verify` checks the target build and then removes a major
//! upgrade's installer app.

mod apply;
mod plan;
mod stage;

pub use apply::{AccountFile, Apply, Installers};
pub(crate) use stage::HostPrograms;
pub use stage::{Programs, Stage};

use std::path::Path;

use super::kind::{Cutover, KindError, MaintenanceKind};
use super::params::MaintainParams;

/// The `macos` kind: its stage and its apply.
pub struct MacosKind {
    stage: Stage,
    apply: Apply,
}

impl MacosKind {
    /// The kind staging with `stage` and applying with `apply`.
    pub fn new(stage: Stage, apply: Apply) -> MacosKind {
        MacosKind { stage, apply }
    }

    /// The kind of this Mac, keeping its install's plan in `run_dir`.
    pub fn at_host(run_dir: Option<&Path>) -> MacosKind {
        MacosKind::new(Stage::at_host(), Apply::at_host(run_dir))
    }
}

impl MaintenanceKind for MacosKind {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.stage.run(params)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        self.apply.cutover(params)
    }

    fn restart(&self, params: &MaintainParams) -> Option<Result<(), KindError>> {
        Some(self.apply.install(params))
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        self.apply.after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.apply.verify(params)
    }
}
