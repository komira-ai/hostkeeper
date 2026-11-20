//! The `macos` kind of the maintenance hook on a Mac (docs/design/maintenance-updates.md, sections
//! 5.2 and 5.5). Its `prepare` is the stage ([`Stage`]): the free space of the data volume checked,
//! then `softwareupdate --download` of each update, or `--fetch-full-installer` of a major upgrade
//! the order allows, nothing installed. Its `cutover`, `after_reboot` and `verify` are not written
//! yet: each fails `unsupported_kind`, as the stub ([`Unsupported`]) does.

mod stage;

pub use stage::Stage;

use super::kind::{Cutover, KindError, MaintenanceKind, Unsupported};
use super::params::{Kind, MaintainParams};

/// The `macos` kind: its stage, and the steps not written yet.
pub struct MacosKind {
    stage: Stage,
}

impl MacosKind {
    /// The kind staging with `stage`.
    pub fn new(stage: Stage) -> MacosKind {
        MacosKind { stage }
    }

    /// The kind of this Mac.
    pub fn at_host() -> MacosKind {
        MacosKind::new(Stage::at_host())
    }
}

impl MaintenanceKind for MacosKind {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.stage.run(params)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        Unsupported(Kind::Macos).cutover(params)
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        Unsupported(Kind::Macos).after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Macos).verify(params)
    }
}
