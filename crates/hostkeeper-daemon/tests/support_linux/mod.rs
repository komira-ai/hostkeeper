//! A Linux maintenance run end to end, in process: the real coordinator (its enrollment, its host
//! groups and its `Maintenance` service, reached through its gRPC router as the operator and as
//! each host), and on each host the real daemon, its heartbeat loop and its run record, over a
//! fake Linux host.
//!
//! The fake host ([`Machine`]) is a scratch directory standing for the host's files (its
//! `os-release`, its kernels under `lib/modules`, `run/reboot-required` and its boot id), the
//! programs the maintenance hook and the report run (`uname`, `apt-get`, `apt-mark`, the install
//! through `env`, and `systemctl reboot`), and one service, `alpha`, whose hook declares `drain`
//! and `resume`. Every change the host goes through is one line of its effects, in order: the
//! stage, each drain and resume, each install, the reboot asked for, each boot into a new boot id
//! and each check.
//!
//! The maintenance hook's `cutover` and `verify` are the real hook's ([`run_step`]) over the
//! real `linux` and `reboot` kinds, the real apt upgrader, the real reboot check and the real
//! check of the Linux kinds, each over the fake host's programs and files: the launcher of the
//! daemon ([`HostLauncher`]) runs them in process where a host runs `hostkeeper maintenance-hook`
//! in a unit. The hook's `prepare` (the stage, whose programs this crate's tests cannot reach) is
//! answered by the launcher: it logs `stage` and succeeds.
//!
//! A reboot is the test's: once the hook asked for one, [`Node::reboot`] boots the fake host into
//! a new boot id (the daemon's clock and the hook's boot id together, the newest kernel running,
//! `run/reboot-required` gone), drops the daemon and its loop as the reboot ends them, and starts
//! a new daemon on the same directories, whose start resumes the turn from its run record.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser as _;
use hostkeeper_coordinator::config::{Cli, Command, build};
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::enrollment::Enrollment;
use hostkeeper_coordinator::http::{Peer, Surface};
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::verdict_file::{VerdictFile, verdicts_path};
use hostkeeper_daemon::cli::maintenance_source;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::maintenance_apt::Apt;
use hostkeeper_daemon::maintenance_collect::{Collector, HostRoot, LinuxScope};
use hostkeeper_daemon::maintenance_hook::linux::{self as hook_linux, LinuxCheck, ServiceHealth};
use hostkeeper_daemon::maintenance_hook::{
    HookPorts, HookStep, MaintainParams, Registry, StoreRecord, run_step,
};
use hostkeeper_daemon::maintenance_host::HostFacts;
use hostkeeper_daemon::maintenance_linux::apt::AptUpgrade;
use hostkeeper_daemon::maintenance_linux::kinds::{LinuxKind, RebootCheck, RebootKind};
use hostkeeper_daemon::maintenance_linux::rebooter::HostRebooter;
use hostkeeper_daemon::maintenance_own::OwnPackages;
use hostkeeper_daemon::maintenance_reboot::RebootNeed;
use hostkeeper_daemon::maintenance_run::Driven;
use hostkeeper_daemon::start::{self, Started};
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_hook::Outcome;
use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::testing::{
    FakeCerts, FakeClock as LinkClock, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, ClientIdentity, CoordinatorLink, DEFAULT_HEARTBEAT_SECS, Feature,
    Heartbeat, Heartbeater, LinkError, MaintainOrder, OrderId, Reason, Reply, StopFlag,
};
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock as oidc_clock, config_text, jwks_of, keys, spent,
};
use hostkeeper_oidc::{OidcConfig, Roots, Verifier};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_steps::maintenance::{MaintenanceLoaded, MaintenanceRecord, MaintenanceStore};
use hostkeeper_steps::{
    AdditiveCall, BootId, BootIdError, BootIdSource, CrashGate, CrashPoint, LaunchError, StepCall,
    StepExit, StepLauncher, ToolCall, ToolExit,
};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, UPDATER_ROLE,
};
use prost::Message;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, SanType,
};
use rustls::pki_types::CertificateDer;

use crate::support::coordinated::OPERATOR;
use crate::support::pki::Ca;
use crate::support::typed::{call, request_of};
use crate::support::{OWN, Rig, Scratch, config, manifest_with};
use crate::support_maintain::DRAINS;

mod machine;
mod node;
mod world;

pub use machine::{Due, Machine};
pub use node::{Node, Ordered, after_drained};
pub use world::World;
use world::peer;

/// The service of every host, whose hook declares `drain` and `resume`.
pub const SERVICE: &str = "alpha";
/// The health check of [`SERVICE`]: a key of the rig's fake health client, which nothing dials.
const HEALTH_URL: &str = "http://127.0.0.1:9301/healthz";
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
/// The exit of a hook step that ended deferred.
const EXIT_DEFERRED: i32 = 75;
/// What the fake install prints on standard error when it fails.
pub const INSTALL_ERROR: &str = "E: the fake mirror is unreachable";

/// `Coordinator.Heartbeat`.
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
const APPROVE: &str = "/hostkeeper.operator.v1.Enrollment/Approve";
const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";
const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
const CANCEL: &str = "/hostkeeper.operator.v1.Maintenance/Cancel";
const PUT_BACK: &str = "/hostkeeper.operator.v1.Maintenance/PutBack";

/// The steps of a host between staging and done: those of a host in its turn.
pub const IN_TURN: [op::MaintenanceHostStep; 6] = [
    op::MaintenanceHostStep::Staging,
    op::MaintenanceHostStep::Draining,
    op::MaintenanceHostStep::Applying,
    op::MaintenanceHostStep::Rebooting,
    op::MaintenanceHostStep::Checking,
    op::MaintenanceHostStep::PuttingBack,
];

/// The `maintain` order of the first offer of a host's turn in the run `run_id`: `RUN`, try 1,
/// of `kinds`.
pub fn first_offer(run_id: &str, kinds: &[pb::MaintenanceKind]) -> Ordered {
    Ordered {
        id: format!("maintain-{run_id}"),
        order_try: 1,
        action: pb::MaintainAction::Run,
        kinds: kinds.to_vec(),
    }
}

/// The maintenance table of a group: batch 1, one host left serving, every other key its
/// default.
pub fn one_at_a_time() -> op::MaintenancePolicy {
    op::MaintenancePolicy {
        batch: Some(1),
        min_serving: Some(1),
        ..op::MaintenancePolicy::default()
    }
}

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
