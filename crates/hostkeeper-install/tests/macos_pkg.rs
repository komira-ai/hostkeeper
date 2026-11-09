//! The steps of the `macos-pkg` installer (packages-repositories.md section 4.7) against a fake
//! Mac: `check_entry`, `announces`, `stage`, `activate` (an update, a rollback, a repair of
//! drift), `remove`, `retire`, and the observer's `drift::observe`.

#[path = "macos_pkg/activate.rs"]
mod activate;
#[path = "macos_pkg/drift.rs"]
mod drift;
#[path = "macos_pkg/host_errors.rs"]
mod host_errors;
#[path = "macos_pkg/remove.rs"]
mod remove;
#[path = "macos_pkg/stage.rs"]
mod stage;
#[path = "macos_pkg/support/mod.rs"]
mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use hostkeeper_install::host::Ids;
use hostkeeper_install::macos_pkg::drift::{Drift, Seen, observe};
use hostkeeper_install::macos_pkg::record::PkgRecord;
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::macos_pkg::{Activated, Order, announces, check_entry};
use hostkeeper_install::tools::{Tool, ToolOutput};
use hostkeeper_install::{Class, Failure, Outcome, Reason};
use hostkeeper_launchd::launchctl::OUTPUT_MAX;
use support::*;

pub const BOTH: [&str; 2] = [SERVICE, "svc.worker"];
pub const HOME: &str = "/Library/Application Support/svc";

pub fn home(rel: &str) -> String {
    format!("{HOME}/{rel}")
}

/// Version 1.0: runs two jobs, and lists a directory and a file version 2.0 drops.
pub fn v1() -> FakePkg {
    FakePkg::standard(SERVICE, IDENTIFIER, "1.0")
        .label("svc.worker")
        .file("old/legacy", "legacy", 0o644)
}

/// Version 2.0: runs one job, and lists a directory version 1.0 lacks.
pub fn v2() -> FakePkg {
    FakePkg::standard(SERVICE, IDENTIFIER, "2.0").file("lib/x", "lib v2", 0o644)
}

/// A rig with 1.0 staged and activated as a first install.
pub struct Installed {
    pub rig: Rig,
    pub dir1: PathBuf,
    pub order1: Order,
}

pub fn installed(name: &str) -> Installed {
    let rig = Rig::new(name);
    rig.mac.add(v1());
    let order1 = rig.order_run("1.0", &BOTH);
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    Installed { rig, dir1, order1 }
}

impl Installed {
    /// Stages 2.0 beside 1.0.
    pub fn stage_v2(&self) -> (PathBuf, Order) {
        self.rig.mac.add(v2());
        let order = self.rig.order("2.0");
        (self.rig.stage(&order, "svc-2.0"), order)
    }

    /// 1.0 activated, then 2.0 activated over it.
    pub fn updated(name: &str) -> (Installed, PathBuf, Order) {
        let i = installed(name);
        let (dir2, order2) = i.stage_v2();
        i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap();
        (i, dir2, order2)
    }
}

pub fn reason<T: std::fmt::Debug>(result: Result<T, Failure>) -> Reason {
    result.unwrap_err().reason()
}

pub fn only_verbs(rig: &Rig, tool: Tool) -> Vec<String> {
    rig.mac
        .calls_of(tool)
        .into_iter()
        .map(|a| a[0].clone())
        .collect()
}

pub fn no_install_ran(rig: &Rig) {
    assert!(rig.mac.calls_of(Tool::Installer).is_empty());
    assert!(rig.mac.calls_of(Tool::Launchctl).is_empty());
}
