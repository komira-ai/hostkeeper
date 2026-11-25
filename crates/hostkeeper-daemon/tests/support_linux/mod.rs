//! A Linux maintenance run end to end, in process (`support_e2e`: the real coordinator, and on
//! each host the real daemon, its heartbeat loop and its run record), over a fake Linux host.
//!
//! The fake host ([`Machine`]) is a scratch directory standing for the host's files (its
//! `os-release`, its kernels under `lib/modules`, `run/reboot-required` and its boot id), the
//! programs the maintenance hook and the report run (`uname`, `apt-get`, `apt-mark`, the install
//! through `env`, and `systemctl reboot`), and one service, `alpha`, whose hook declares `drain`
//! and `resume`. Every change the host goes through is one line of its effects, in order: the
//! stage, each drain and resume, each install, the reboot asked for, each boot into a new boot id
//! and each check.
//!
//! The maintenance hook's `prepare`, `cutover` and `verify` are the real hook's over the real
//! `linux` and `reboot` kinds, the real apt upgrader, the real reboot check and the real check of
//! the Linux kinds, each over the fake host's programs and files: the daemon's launcher runs them
//! in process where a host runs `hostkeeper maintenance-hook` in a unit, and the host logs `stage`
//! for each `prepare` and `check` for each `verify`. The `prepare` of `reboot` is the real kind's;
//! the `prepare` of `linux` (its stage, whose programs this crate's tests cannot reach; they are
//! tested in the hook's own module) is [`machine::StageAnswered`]'s: it succeeds and runs nothing.
//!
//! A reboot boots the fake host into a new boot id with the newest kernel running and
//! `run/reboot-required` gone.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::maintenance_apt::Apt;
use hostkeeper_daemon::maintenance_collect::{Collector, HostRoot};
use hostkeeper_daemon::maintenance_hook::linux::{self as hook_linux, LinuxCheck, ServiceHealth};
use hostkeeper_daemon::maintenance_hook::{
    Cutover, Ending as HookEnding, HookStep, KindError, MaintainParams, MaintenanceKind, Rebooter,
    Registry,
};
use hostkeeper_daemon::maintenance_host::HostFacts;
use hostkeeper_daemon::maintenance_linux::apt::AptUpgrade;
use hostkeeper_daemon::maintenance_linux::kinds::{LinuxKind, RebootCheck, RebootKind};
use hostkeeper_daemon::maintenance_linux::rebooter::HostRebooter;
use hostkeeper_daemon::maintenance_own::OwnPackages;
use hostkeeper_daemon::maintenance_reboot::RebootNeed;
use hostkeeper_daemon::testing::RecordedWaits;
use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};

use crate::support::Scratch;
pub use crate::support_e2e::{
    EXIT_DEFERRED, Host, Ordered, SERVICE, World, after_drained, first_offer, one_at_a_time,
};

mod machine;

pub use machine::{Due, Machine};

/// A host of a Linux test.
pub type Node = crate::support_e2e::Node<Machine>;

impl Node {
    /// The host `host` of the group `group` of the test `test`, running `alpha` 1.0.0 on a fake
    /// Linux host with `due` due; its daemon started.
    pub fn new(world: &Arc<World>, test: &str, host: &str, group: &str, due: Due) -> Node {
        Node::on(
            world,
            test,
            host,
            group,
            Machine::new(&format!("{test}-{host}"), due),
        )
    }
}

/// The kernel every fake host runs at first.
pub const OLD_KERNEL: &str = "6.8.0-40-generic";
/// The kernel an install of the due kernel update leaves installed.
pub const NEW_KERNEL: &str = "6.8.0-45-generic";
/// The package of the due kernel update, as `apt-get` names it.
pub const KERNEL_PACKAGE: &str = "linux-image-generic";
/// What `apt-get --simulate dist-upgrade` prints on a host with the kernel update due: one
/// security update of a kernel image.
pub const KERNEL_UPDATE: &str =
    "Inst linux-image-generic [6.8.0.40.40] (6.8.0.45.45 Ubuntu:24.04/noble-security [amd64])\n";
/// The installed security update of [`NEW_PACKAGES`].
pub const SECURITY_PACKAGE: &str = "python3-jwt";
/// What `apt-get --simulate dist-upgrade` prints on an Ubuntu host booted from a newer kernel
/// series than 6.8 whose upgrade would install two packages it does not have yet, a library (a
/// security update) and a 6.8 kernel image (not one), beside one installed security update,
/// [`SECURITY_PACKAGE`].
pub const NEW_PACKAGES: &str = "\
Inst libdrm-amdgpu1 (2.4.125-1ubuntu0.1~24.04.2 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Inst python3-jwt [2.7.0-1ubuntu0.4] (2.7.0-1ubuntu0.5 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [all])
Inst linux-image-6.8.0-151-generic (6.8.0-151.151 Ubuntu:24.04/noble-updates [amd64])
";
/// The seconds the install waits for the package manager's lock.
const LOCK_WAIT: u64 = 300;
/// What the fake install prints on standard error when it fails.
pub const INSTALL_ERROR: &str = "E: the fake mirror is unreachable";

/// What a host goes through in a turn of a run of the kernel update, in order.
pub fn kernel_turn() -> Vec<String> {
    [
        "stage".to_owned(),
        format!("drain {SERVICE}"),
        format!("install {KERNEL_PACKAGE}"),
        "reboot".to_owned(),
        "boot boot-2".to_owned(),
        "check".to_owned(),
        format!("resume {SERVICE}"),
    ]
    .to_vec()
}
