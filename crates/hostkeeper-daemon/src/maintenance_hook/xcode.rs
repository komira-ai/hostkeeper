//! The `xcode` kind of the maintenance hook on a Mac (docs/design/maintenance-updates.md, section
//! 2.4). Its `prepare` is the stage ([`stage::Stage`]): the Xcodes of the order fetched from the
//! coordinator's store, checked, expanded and verified in the staging directory while the Mac
//! still serves. Its `cutover` and `verify` are the apply ([`apply::Apply`]), on the drained Mac:
//! each staged Xcode moved into place, its licence accepted and its first launch run, the default
//! selected and the old Xcodes the order names removed; then each checked. Xcode needs no reboot,
//! so its `cutover` never asks for one and its `after_reboot`, never called, fails
//! `unsupported_kind`.

pub mod apply;
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

    /// The kind of this Mac: the store of the coordinator the daemon config
    /// ([`DEFAULT_CONFIG`]) names, with the host's certificate ([`PKI_DIR`]); `xip` and
    /// `codesign` run as child processes; the free space of the volume of `/Applications`; the
    /// Mac's own files; and the apply over the Mac's own programs and files.
    #[must_use]
    pub fn at_host() -> XcodeKind {
        let clock: Arc<dyn Clock> = Arc::new(crate::adapters::host_clock());
        XcodeKind::new(
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
            },
            Apply::at_host(),
        )
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
