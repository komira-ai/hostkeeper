//! A maintenance run end to end, in process, over any fake host: the real coordinator (its
//! enrollment, its host groups, its Xcode store and its `Maintenance` service, reached through its
//! gRPC router as the operator and as each host) in [`World`], and on each host the real daemon, its
//! heartbeat loop and its run record in [`Node`], over a fake host that implements [`Host`]. The
//! Linux fake host is `support_linux`'s, the Mac's `support_mac`'s.
//!
//! The daemon's launcher ([`node`]) runs the maintenance hook's `prepare`, `cutover` and `verify`
//! in process through the real hook ([`run_step`]) over the kinds the host gives ([`Host::kinds`]),
//! where a host runs `hostkeeper maintenance-hook` in a unit, and each service's `drain` and
//! `resume` through the host's service ([`Host::service_step`]).
//!
//! A reboot is the test's: once the host asked for one, [`Node::reboot`] boots the fake host into
//! a new boot id ([`Host::boot`]: the daemon's clock and the hook's boot id together), drops the
//! daemon and its loop as the reboot ends them, and starts a new daemon on the same directories,
//! whose start resumes the turn from its run record.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
use hostkeeper_daemon::maintenance_collect::{Collector, LinuxScope};
use hostkeeper_daemon::maintenance_hook::{
    Ending as HookEnding, HookPorts, HookStep, MaintainParams, Rebooter, Registry, StoreRecord,
    run_step,
};
use hostkeeper_daemon::maintenance_run::Driven;
use hostkeeper_daemon::start::{self, Started};
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_hook::Outcome;
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
    AdditiveCall, BootIdSource, CrashGate, CrashPoint, LaunchError, StepCall, StepExit,
    StepLauncher, ToolCall, ToolExit,
};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, UPDATER_ROLE,
};
use prost::Message;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, SanType,
};
use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};

use crate::support::coordinated::OPERATOR;
use crate::support::pki::Ca;
use crate::support::typed::{call, request_of};
use crate::support::{OWN, Rig, Scratch, config, manifest_with};
use crate::support_maintain::DRAINS;

mod node;
mod world;

pub use node::{Node, Ordered, after_drained};
pub use world::{World, peer};

/// The service of every host, whose hook declares `drain` and `resume`.
pub const SERVICE: &str = "alpha";
/// The health check of [`SERVICE`]: a key of the rig's fake health client, which nothing dials.
const HEALTH_URL: &str = "http://127.0.0.1:9301/healthz";
/// The exit of a hook step that ended deferred.
pub const EXIT_DEFERRED: i32 = 75;

/// `Coordinator.Heartbeat`.
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
const APPROVE: &str = "/hostkeeper.operator.v1.Enrollment/Approve";
const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";
const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
const CANCEL: &str = "/hostkeeper.operator.v1.Maintenance/Cancel";
const PUT_BACK: &str = "/hostkeeper.operator.v1.Maintenance/PutBack";

/// The second the coordinator's clock reads at its first start.
pub const COORDINATOR_START_SECS: u64 = 1_000;

/// The steps of a host between staging and done: those of a host in its turn.
pub const IN_TURN: [op::MaintenanceHostStep; 6] = [
    op::MaintenanceHostStep::Staging,
    op::MaintenanceHostStep::Draining,
    op::MaintenanceHostStep::Applying,
    op::MaintenanceHostStep::Rebooting,
    op::MaintenanceHostStep::Checking,
    op::MaintenanceHostStep::PuttingBack,
];

/// What a fake host gives the daemon and the maintenance hook of a test: the hook's kinds, its
/// reboot and its boot id, the collectors of the daemon's report, its services' `drain` and
/// `resume`, and its next boot.
pub trait Host: BootIdSource + Send + Sync + Sized + 'static {
    /// The platform the host's heartbeats name; `None`: the one the daemon names, the build's.
    fn platform(&self) -> Option<pb::Platform>;
    /// Notes that the maintenance hook's `step` starts.
    fn hook_started(&self, step: HookStep);
    /// Notes how the maintenance hook's `step` ended.
    fn hook_ended(&self, step: HookStep, ending: &HookEnding);
    /// The maintenance hook's kinds on this host for a run whose directory is `run_dir`.
    fn kinds(self: &Arc<Self>, run_dir: &Path) -> Registry;
    /// The host's reboot, as the maintenance hook asks for it.
    fn rebooter(self: &Arc<Self>) -> Box<dyn Rebooter>;
    /// What the daemon's report looks at.
    fn collectors(self: &Arc<Self>) -> Vec<Box<dyn Collector>>;
    /// `service`'s `step` (`drain` or `resume`) with the params file `params`: its exit code.
    fn service_step(&self, service: &str, step: &str, params: &Path) -> i32;
    /// The reboot the host asked for: the next boot. Returns the new boot's id.
    fn boot(&self) -> String;
}

/// The `maintain` order of the first offer of a host's turn in the run `run_id`: `RUN`, try 1,
/// of `kinds`.
pub fn first_offer(run_id: &str, kinds: &[pb::MaintenanceKind]) -> Ordered {
    Ordered {
        id: format!("maintain-{run_id}"),
        order_try: 1,
        action: pb::MaintainAction::Run,
        kinds: kinds.to_vec(),
        ..Ordered::default()
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
