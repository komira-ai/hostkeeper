//! The `xcode` kind of the maintenance hook on a Mac (docs/design/maintenance-updates.md, section
//! 2.4). Its `prepare` is the stage ([`stage::Stage`]): the Xcodes of the order fetched from the
//! coordinator's store, checked, expanded and verified in the staging directory while the Mac
//! still serves. Its `cutover`, `after_reboot` and `verify` are not written yet and fail
//! `unsupported_kind`.

pub mod stage;
pub mod store;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_package::gh::SystemRunner;
use hostkeeper_steps::Clock;

use super::kind::{Cutover, KindError, MaintenanceKind, Unsupported};
use super::params::{Kind, MaintainParams};
use crate::cli::{DEFAULT_CONFIG, PKI_DIR};
use crate::maintenance_collect::{APPLICATIONS, HostRoot};
use crate::maintenance_mac::Statvfs;
use crate::maintenance_plist::HostPlists;
use stage::Stage;
use store::HostStore;

/// The owner the daemon config must have on a host: root.
const ROOT_UID: u32 = 0;

/// The `xcode` kind: the stage, and the stub for every other step.
pub struct XcodeKind {
    stage: Stage,
}

impl XcodeKind {
    /// The kind whose `prepare` is `stage`.
    #[must_use]
    pub fn new(stage: Stage) -> XcodeKind {
        XcodeKind { stage }
    }

    /// The kind of this Mac: the store of the coordinator the daemon config
    /// ([`DEFAULT_CONFIG`]) names, with the host's certificate ([`PKI_DIR`]); `xip` and
    /// `codesign` run as child processes; the free space of the volume of `/Applications`; the
    /// Mac's own files.
    #[must_use]
    pub fn at_host() -> XcodeKind {
        let clock: Arc<dyn Clock> = Arc::new(crate::adapters::host_clock());
        XcodeKind::new(Stage {
            store: Box::new(HostStore::new(
                Path::new(DEFAULT_CONFIG),
                ROOT_UID,
                Path::new(PKI_DIR),
                Arc::clone(&clock),
            )),
            runner: Box::new(SystemRunner::new(Arc::clone(&clock))),
            space: Box::new(Statvfs::at(Path::new(APPLICATIONS))),
            plists: Box::new(HostPlists::at_host()),
            root: HostRoot::host(),
            clock,
        })
    }
}

impl MaintenanceKind for XcodeKind {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.stage.run(&params.maintain().xcodes)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        Unsupported(Kind::Xcode).cutover(params)
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        Unsupported(Kind::Xcode).after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Xcode).verify(params)
    }
}
