//! The `xcode` kind of the maintenance hook on a Mac (docs/design/maintenance-updates.md, section
//! 2.4). Its `prepare` is the stage ([`stage::Stage`]): the Xcodes of the order fetched from the
//! coordinator's store, checked, expanded and verified in the staging directory while the Mac
//! still serves. Its `cutover` and `verify` are the apply ([`apply::Apply`]), on the drained Mac:
//! each staged Xcode moved into place, its licence accepted and its first launch run, the default
//! selected and the old Xcodes the order names removed; then each checked. Xcode needs no reboot,
//! so its `cutover` never asks for one and its `after_reboot`, never called, fails
//! `unsupported_kind`. The `probe` kind's archive check ([`check`]) shares the stage's ports.

pub mod apply;
pub mod check;
#[cfg(test)]
mod fakes;
mod signature;
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
use apply::Apply;
use stage::Stage;
use store::HostStore;

/// The owner the daemon config must have on a host: root.
const ROOT_UID: u32 = 0;

/// The `xcode` kind: its stage and its apply.
pub struct XcodeKind {
    stage: Stage,
    apply: Apply,
}

impl XcodeKind {
    /// The kind whose `prepare` is `stage` and whose `cutover` and `verify` are `apply`.
    #[must_use]
    pub fn new(stage: Stage, apply: Apply) -> XcodeKind {
        XcodeKind { stage, apply }
    }

    /// The kind of this Mac: the stage over the Mac's own ports ([`host_stage`]) and the apply over
    /// the Mac's own programs and files.
    #[must_use]
    pub fn at_host() -> XcodeKind {
        XcodeKind::new(host_stage(), Apply::at_host())
    }
}

/// The stage of this Mac, over its own ports: the store of the coordinator the daemon config
/// ([`DEFAULT_CONFIG`]) names, with the host's certificate ([`PKI_DIR`]); programs run as child
/// processes; the free space of the volume of `/Applications`; and the Mac's own files. The `xcode`
/// kind stages with it, and the `probe` kind's archive check ([`check::ArchiveCheck`]) reads the
/// store and runs Apple's tools through it.
fn host_stage() -> Stage {
    let clock: Arc<dyn Clock> = Arc::new(crate::adapters::host_clock());
    Stage {
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
    }
}

impl MaintenanceKind for XcodeKind {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.stage.run(&params.maintain().xcodes)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        self.apply.cutover(params)
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        Unsupported(Kind::Xcode).after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.apply.verify(params)
    }
}
