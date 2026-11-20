//! The `linux` kind of the maintenance hook on a Linux host, under systemd or OpenRC
//! (docs/design/maintenance-updates.md, section 5.2). Its `prepare` is the stage ([`Stage`]):
//! the free space checked, then a download alone with `apt-get`, `dnf` or `apk`, nothing
//! installed. Its `cutover`, `after_reboot` and `verify` are not written yet: each fails
//! `unsupported_kind`, as the stub ([`Unsupported`]) does.

mod apk;
mod apt;
mod dnf;
mod stage;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use stage::Stage;

use super::kind::{Cutover, KindError, MaintenanceKind, Unsupported};
use super::params::{Kind, MaintainParams};

/// The `linux` kind: its stage, and the steps not written yet.
pub struct Linux {
    stage: Stage,
}

impl Linux {
    /// The kind staging with `stage`.
    pub fn new(stage: Stage) -> Linux {
        Linux { stage }
    }

    /// The kind of this host, a held package manager's lock waited for `lock_wait_secs` at most.
    pub fn at_host(lock_wait_secs: u64) -> Linux {
        Linux::new(Stage::at_host(lock_wait_secs))
    }
}

impl MaintenanceKind for Linux {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.stage.run(params)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        Unsupported(Kind::Linux).cutover(params)
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        Unsupported(Kind::Linux).after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Linux).verify(params)
    }
}
