//! `hostkeeper-coordinator`: tells each host's updater which version to run, one live update at a
//! time across every service and role (`docs/design/host-updates.md`). The per-role limits and
//! minimums a desired state carries are kept, written and printed, and no decision reads them.
//!
//! - [`identity`]: who is calling, from the client certificate's subjectAltNames alone;
//! - [`desired`]: the desired state of each service, in `desired.json`, replaced atomically,
//!   with the [`registry`] of every version CI has set and the digest of its package;
//! - [`fleet`]: the last heartbeat of every host, the live and down rule, the quiet start;
//! - [`acks`]: the failure reports an override of a bad version acknowledged;
//! - [`verdicts`]: the release failures heard, each counted once per host, order id and try;
//! - [`bake`] and [`canary`]: how long an updated host holds its slot, and the hosts that go
//!   alone first;
//! - [`converge`]: whether the farm has reached the desired version, for CI to wait on;
//! - [`decide`]: the decision as a pure function of the desired state, the fleet, the host
//!   and the time;
//! - [`reconcile`]: the last good version, the bad ones, and the automatic rollback;
//! - [`holds`]: the operator's holds of a service on a host, a role or the fleet, which the
//!   gate orders nothing and the counts leave out;
//! - [`force`]: the fleet-wide fix, a hookless `force` ordered at once past the one live update,
//!   and [`orders`], the operator's orders (`force`, `abort`) a typed heartbeat answer carries;
//! - [`operator`]: the operator's calls over the messages of `hostkeeper.operator.v1` (marks,
//!   halts, aborts, exclusions, holds, the audit log);
//! - [`ops`]: what operators decide (exclusions, labels, halts cleared), the marks of bad and
//!   last good versions and the audit ring, in the operations file, and every change as a line of
//!   the audit file beside it (`audit_file`); the halt reports and clear generations in the halts
//!   file beside it (`ops_file`);
//! - [`operate`]: the operator actions of the JSON route `/v1/operator`, each validated before it
//!   changes the ops store;
//! - [`services`]: services as data, from the rollout file: the roles each runs on and the
//!   settings of its rollout;
//! - [`tokens`]: the operator's named tokens, kept as hashes and re-read while the coordinator
//!   runs;
//! - [`service`]: the coordinator itself: heartbeats in, the desired state set by CI's OIDC
//!   token, the status read by the operator token;
//! - [`status`]: what an operator reads, and the convergence of a service;
//! - [`http`] and [`listen`]: the two HTTP surfaces (the heartbeat surface and the private
//!   surface of CI and the operator) and the TLS listeners under them;
//! - [`ui`]: the static files of `--ui-dir`, which the private surface serves under `/ui/`;
//! - [`config`]: the flags and the start, under the [`lock`] of the state files, with CI's spent
//!   token ids in the [`jti_file`] beside them.
//!
//! The wire types of the heartbeat and the reply are `hostkeeper_wire::wire`.

/// The commit this coordinator was built from: the 12 digits `--version` shows after the
/// `+`, or `unknown` where the build had no git checkout. `GET /v1/status` and
/// `GET /v1/convergence` say it as `commit`.
pub const BUILD_COMMIT: &str = env!("HOSTKEEPER_BUILD_COMMIT");

pub mod acks;
pub(crate) mod audit_file;
pub mod bake;
pub mod canary;
pub mod config;
pub mod converge;
pub mod decide;
pub mod defer;
pub mod desired;
pub mod fleet;
pub mod force;
pub mod halt;
pub mod holds;
pub mod http;
pub mod identity;
pub mod jti_file;
pub mod listen;
pub mod lock;
pub mod membership;
pub mod operate;
pub mod operator;
pub mod ops;
pub(crate) mod ops_file;
pub mod orders;
pub mod reconcile;
pub mod registry;
pub mod service;
pub mod services;
pub mod status;
pub mod tokens;
pub mod ui;
pub mod verdicts;

#[cfg(test)]
mod audit_file_tests;
#[cfg(test)]
mod bake_canary_tests;
#[cfg(test)]
mod bake_review_tests;
#[cfg(test)]
mod converge_tests;
#[cfg(test)]
mod decide_golden_tests;
#[cfg(test)]
mod decide_tests;
#[cfg(test)]
mod decide_why_tests;
#[cfg(test)]
mod defer_tests;
#[cfg(test)]
mod desired_dropped_tests;
#[cfg(test)]
mod desired_sync_tests;
#[cfg(test)]
mod desired_tests;
#[cfg(test)]
mod extras_dropped_tests;
#[cfg(test)]
mod force_targets_tests;
#[cfg(test)]
mod force_tests;
#[cfg(test)]
mod halt_membership_tests;
#[cfg(test)]
mod halt_race_tests;
#[cfg(test)]
mod holds_calls_tests;
#[cfg(test)]
mod holds_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod marks_support;
#[cfg(test)]
mod marks_tests;
#[cfg(test)]
mod membership_tests;
#[cfg(test)]
mod one_live_update_coordinator_tests;
#[cfg(test)]
mod one_live_update_release_tests;
#[cfg(test)]
mod one_live_update_tests;
#[cfg(test)]
mod ops_commit_tests;
#[cfg(test)]
mod ops_crash_tests;
#[cfg(test)]
mod ops_downgrade_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod priority_tests;
#[cfg(test)]
mod recorded_definition_tests;
#[cfg(test)]
mod recorded_review_tests;
#[cfg(test)]
mod rollback_definition_tests;
#[cfg(test)]
mod rollback_membership_tests;
#[cfg(test)]
mod rollback_override_tests;
#[cfg(test)]
mod rollback_race_tests;
#[cfg(test)]
mod rollback_tests;
#[cfg(test)]
mod self_update_fallback_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod services_tests;
#[cfg(test)]
mod state_safety_digest_tests;
#[cfg(test)]
mod state_safety_halt_tests;
#[cfg(test)]
mod state_safety_jti_tests;
#[cfg(test)]
mod state_safety_rollout_tests;
#[cfg(test)]
mod status_service_tests;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tierstore_service_tests;
#[cfg(test)]
mod tokens_tests;
