//! `hostkeeper-coordinator`: tells each host's updater which version to run, each service by its
//! update plan: stages of a growing share of its hosts, the hosts of a stage updating in
//! parallel under the per-role limits and minimums of the desired state, and one service's
//! rollout live at a time by default (`docs/design/rollout-update-plans.md`;
//! `docs/design/host-updates.md` for the rules a host is ordered under).
//!
//! - [`identity`]: who is calling, from the client certificate's subjectAltNames alone;
//! - [`enrollment`]: how a host gets and keeps its certificate (`Enroll`, `Poll`, `Renew`) over
//!   `hostkeeper_enrollment`'s CA and store, the start that opens them and makes the
//!   coordinator's server certificate, and what the heartbeat path asks of them (who an
//!   enrolled host's certificate names, whether it is denied, retiring or duplicate);
//! - [`desired`]: the desired state of each service, in `desired.json`, replaced atomically,
//!   with the [`registry`] of every version CI has set and its release (the digest of its
//!   package, or the record of its release index; neither orders a host anything);
//! - [`fleet`]: the last heartbeat of every host, the live and down rule, the quiet start, and
//!   what each host's last gRPC heartbeat said of its Sigstore trusted root ([`trusted_root`]);
//! - [`acks`]: the failure reports an override of a bad version acknowledged;
//! - [`verdicts`]: the release failures heard, each counted once per host, order id and try,
//!   and the [`verdict_file`] that stores them with the verdict each host was last acknowledged,
//!   before an answer acknowledges it;
//! - [`bake`] and [`canary`]: how long an updated host holds its slot, and the hosts that go
//!   alone first (the first stage of a plan);
//! - [`stages`]: the update plan of a service: its stages over the eligible hosts, where the
//!   plan stands, and the gate it puts on a host's order (`max_updating`, `min_up`, the live
//!   rollouts across services);
//! - [`rollouts`]: where each service's plan stands as the ops store keeps it, what enters a
//!   stage by itself, and the operator's `Rollouts.Advance`, `Pause` and `Resume`;
//! - [`converge`]: whether the farm has reached the desired version, for CI to wait on;
//! - [`decide`]: the decision as a pure function of the desired state, the fleet, the host
//!   and the time: an update, an install, an uninstall, or why none;
//! - [`reconcile`]: the last good version, the bad ones, and the automatic rollback;
//! - [`holds`]: the operator's holds of a service on a host, a role or the fleet, which the
//!   gate orders nothing and the counts leave out;
//! - [`assignment`]: which services each host runs: the rollout file's roles with the operator's
//!   entries per role and host on top, the minimum versions per role and host, and the hosts
//!   ordered to uninstall a service; the uninstall intents a heartbeat ends are cleared by
//!   `intents`;
//! - `host_groups`: the stored form of the host groups (their packages, policy, generation and
//!   desired versions) and of each (group, package)'s marks, which the decision by group reads
//!   (`group_scope`: what a heartbeat is decided over, with `group_converge`, `moving` and
//!   `install_order`); [`operator_host_groups`] and [`host_move`] are the `HostGroups` calls that
//!   write them, with `group_uninstall`, the uninstall intents a change of a group writes;
//! - [`order_terms`]: the order id, try and package digest an update answer carries to an agent
//!   that announced `order-v1`, over a transport that carries typed orders (the JSON route
//!   carries no order);
//! - [`force`]: the fleet-wide fix, a hookless `force` ordered at once past the update plan,
//!   and [`orders`], the typed orders a heartbeat answer carries (`force`, `abort`,
//!   `clear_record`, `install`, `uninstall`), with [`clear_record`], `Hosts.ClearRecord`;
//! - [`operator`]: the operator's calls over the messages of `hostkeeper.operator.v1` (marks,
//!   halts, aborts, exclusions, holds, the audit log), and [`assign`], its assignment calls
//!   (`Services.Assign`, `Unassign`, `SetMinVersion`, `List`), [`service_repository`], its
//!   `Services.SetRepository`, [`operator_enrollment`], its
//!   `Enrollment.List` and `Enrollment.Approve`, and [`hosts`], its `Hosts.List` and `Hosts.Get`;
//! - [`inventory`]: the host inventory (`Enrollment.AddRecord`, `ListRecords`, `RemoveRecord`,
//!   `OpenRecord`), kept in the ops file, the match of each request `Enrollment.List` shows
//!   against it, and the approval window of a record that names a role: while it is open, a
//!   request that matches the record exactly is approved by the coordinator itself
//!   ([`enrollment`]);
//! - [`ops`]: what operators decide (exclusions, labels, halts cleared), the marks of bad and
//!   last good versions and the audit ring, in the operations file, and every change as a line of
//!   the audit file beside it (`audit_file`); the halt reports and clear generations in the halts
//!   file beside it (`ops_file`);
//! - [`operate`]: the operator actions of the JSON route `/v1/operator`, each validated before it
//!   changes the ops store;
//! - [`services`]: services as data, from the rollout file: the roles each runs on and the
//!   settings of its rollout;
//! - [`password`]: Argon2id password hashes, the length policy and the cap on hashes at once;
//! - [`session`]: the browser sessions and the login limiter, in memory;
//! - [`login`] and [`login_routes`]: the operator's sign-in: `POST /v1/login`, `POST /v1/logout`,
//!   `GET /v1/session` and `POST /v1/password` on the private surface, and the session cookie
//!   the JSON gateway accepts beside a bearer token;
//! - [`tokens`]: the operator's named tokens, kept as hashes and re-read while the coordinator
//!   runs;
//! - [`service`]: the coordinator itself: heartbeats in, the desired state set by CI's OIDC
//!   token or an operator token, the status read by the operator token;
//! - `releases`: the release each version was set with by `SetDesired` with targets, kept
//!   beside the desired state, checked by the rules of `hostkeeper-release`, and read back by
//!   `Releases.GetRelease`;
//! - `repositories`: the registry of package repositories the operator keeps (`Repositories`:
//!   [`operator_repositories`]), kept beside the desired state, and `repository_push`: the
//!   set of it each Linux host is sent in the answer to a heartbeat over gRPC, and what its
//!   daemon reported of it;
//! - [`status`]: what an operator reads, and the convergence of a service;
//! - [`alerts`]: the conditions that need a person, derived from what the coordinator holds,
//!   answered by `Alerts.List` and logged with an `alert=` field;
//! - [`stamp`]: the commit and the version `--version` prints;
//! - [`http`] and [`listen`]: the two HTTP surfaces (the heartbeat surface and the private
//!   surface of CI and the operator) and the TLS listeners under them; `health`, the plain
//!   loopback listener of `--health-listen` that answers `GET /healthz` to a probe;
//!   `version_route`, the private surface's `GET /v1/version`, which says which release the
//!   coordinator is;
//! - [`grpc`]: the gRPC services on those surfaces (`hostkeeper.agent.v1.Coordinator` on the
//!   heartbeat one, `hostkeeper.operator.v1` on the private one), the JSON gateway of the
//!   operator API, and the router that tells them from the frozen JSON routes;
//! - `wait`: `Releases.WaitConverged`, a read of the convergence held until it ends the wait,
//!   and the call deadline and connection rule a held call has;
//! - [`ui`]: the static files of `--ui-dir` (by default, for the binary,
//!   [`config::DEFAULT_UI_DIR`] when it is a directory), which the private surface serves under
//!   `/ui/`;
//! - [`config`]: the flags and the start, under the [`lock`] of the state files, with CI's spent
//!   token ids in the [`jti_file`] beside them.
//! - [`backup`]: `backup` and `restore`, a copy of the state files and the enrollment store
//!   taken under the same lock, with the SHA-256 of each file in its manifest, and putting one
//!   back into targets that hold no state;
//! - [`transfer`]: `export`, the state of the database of `--database-dsn-file` written into
//!   files of the file store that hold no state.
//!
//! The wire types of the heartbeat and the reply are `hostkeeper_wire::wire`.

/// The commit this coordinator was built from: the 12 digits `--version` shows after the
/// `+` (from a release build's `RELEASE_COMMIT`, else from the checkout), or `unknown` where
/// the build had neither (see [`stamp`]). `GET /v1/status` and `GET /v1/convergence` say it as
/// `commit`.
pub const BUILD_COMMIT: &str = stamp::BUILD_COMMIT;

pub(crate) mod abm;
pub mod accounts;
pub mod acks;
pub(crate) mod agent_service;
pub mod alerts;
mod alerts_mdm;
pub mod assign;
pub mod assignment;
pub mod audit_actions;
pub(crate) mod audit_file;
pub mod backup;
pub mod bake;
pub mod canary;
pub mod clear_record;
pub mod config;
pub mod converge;
pub mod decide;
pub mod defer;
pub mod desired;
pub mod enrollment;
pub(crate) mod facts;
mod features;
pub mod fleet;
pub mod force;
pub(crate) mod group_converge;
mod group_halt;
pub(crate) mod group_reach;
pub(crate) mod group_scope;
pub(crate) mod group_uninstall;
pub mod grpc;
pub mod halt;
pub(crate) mod health;
pub mod holds;
pub(crate) mod host_group_proto;
pub(crate) mod host_groups;
pub mod host_move;
pub mod hosts;
pub mod http;
pub mod identity;
pub mod initial_admin;
pub(crate) mod install_order;
pub(crate) mod intents;
pub mod inventory;
pub mod jti_file;
pub mod listen;
mod live;
pub mod lock;
pub mod login;
pub(crate) mod login_routes;
pub mod mdm;
pub(crate) mod mdm_enroll;
pub(crate) mod mdm_certs;
pub(crate) mod mdm_name;
pub mod mdm_push;
pub mod mdm_secrets;
mod mdm_server;
pub mod mdm_settings;
pub mod mdm_setup;
pub mod mdm_status;
pub mod membership;
pub(crate) mod moving;
pub mod operate;
pub mod operator;
pub(crate) mod operator_abm;
pub mod operator_accounts;
pub mod operator_enrollment;
pub mod operator_host_groups;
pub mod operator_mdm;
mod operator_mdm_enroll;
pub mod operator_mdm_push;
#[cfg(test)]
#[path = "operator_mdm_push_cert_tests.rs"]
mod operator_mdm_push_cert_tests;
#[cfg(test)]
#[path = "operator_mdm_push_tests.rs"]
mod operator_mdm_push_tests;
pub mod operator_mdm_setup;
#[cfg(test)]
#[path = "operator_mdm_setup_tests.rs"]
mod operator_mdm_setup_tests;
pub mod operator_repositories;
pub mod ops;
pub(crate) mod ops_file;
pub mod order_terms;
pub mod orders;
pub mod origin_https;
pub mod password;
pub(crate) mod picks;
pub mod proxy_cache;
mod proxy_cache_disk;
pub mod reconcile;
pub mod registry;
pub(crate) mod releases;
pub(crate) mod repositories;
pub(crate) mod repository_credential;
pub mod repository_origin;
pub(crate) mod repository_push;
pub(crate) mod repository_serve;
pub mod rollouts;
mod running;
pub mod service;
pub(crate) mod service_repository;
pub mod services;
pub mod session;
pub mod stages;
pub mod stamp;
pub mod status;
pub(crate) mod store;
pub mod tokens;
pub mod transfer;
pub mod trusted_root;
pub mod ui;
pub mod verdict_file;
pub mod verdicts;
pub(crate) mod version_route;
pub(crate) mod wait;
pub(crate) mod write_record;

#[cfg(test)]
mod agent_service_tests;
#[cfg(test)]
mod assign_calls_tests;
#[cfg(test)]
mod assign_edge_tests;
#[cfg(test)]
mod assign_support;
#[cfg(test)]
mod assigned_counts_tests;
#[cfg(test)]
mod assignment_tests;
#[cfg(test)]
mod audit_actions_tests;
#[cfg(test)]
mod audit_file_tests;
#[cfg(test)]
mod audit_list_tests;
#[cfg(test)]
mod audit_refusal_tests;
#[cfg(test)]
mod audit_rotation_tests;
#[cfg(test)]
pub(crate) mod backup_accounts_tests;
#[cfg(test)]
mod backup_refusal_tests;
#[cfg(test)]
mod backup_releases_tests;
#[cfg(test)]
pub(crate) mod backup_repositories_tests;
#[cfg(test)]
mod backup_segment_tests;
#[cfg(test)]
mod backup_tests;
#[cfg(test)]
mod bake_canary_tests;
#[cfg(test)]
mod bake_review_tests;
#[cfg(test)]
mod by_groups_only_tests;
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
mod clear_record_tests;
#[cfg(test)]
mod clear_record_verdict_tests;
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
mod facts_tests;
#[cfg(test)]
mod first_heartbeat_tests;
#[cfg(test)]
mod first_serving_tests;
#[cfg(test)]
mod force_targets_tests;
#[cfg(test)]
mod force_tests;
#[cfg(test)]
mod group_calls_tests;
#[cfg(test)]
mod group_ci_tests;
#[cfg(test)]
mod group_converge_tests;
#[cfg(test)]
mod group_decision_support;
#[cfg(test)]
mod group_decision_tests;
#[cfg(test)]
mod group_force_tests;
#[cfg(test)]
mod group_incident_tests;
#[cfg(test)]
mod group_install_order_tests;
#[cfg(test)]
mod group_marks_tests;
#[cfg(test)]
mod group_set_desired_tests;
#[cfg(test)]
mod group_uninstall_tests;
#[cfg(test)]
mod halt_membership_tests;
#[cfg(test)]
mod halt_race_tests;
#[cfg(test)]
mod holds_calls_tests;
#[cfg(test)]
mod holds_tests;
#[cfg(test)]
mod host_cert_state_tests;
#[cfg(test)]
mod host_group_list_tests;
#[cfg(test)]
mod host_group_move_tests;
#[cfg(test)]
mod host_group_proto_tests;
#[cfg(test)]
mod host_group_put_policy_tests;
#[cfg(test)]
mod host_group_put_tests;
#[cfg(test)]
mod host_group_remove_tests;
#[cfg(test)]
mod host_groups_access_tests;
#[cfg(test)]
mod host_groups_support;
#[cfg(test)]
mod host_held_tests;
#[cfg(test)]
mod host_inventory_tests;
#[cfg(test)]
mod hosts_groups_tests;
#[cfg(test)]
mod hosts_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod index_set_support;
#[cfg(test)]
mod index_set_tests;
#[cfg(test)]
mod install_tests;
#[cfg(test)]
mod inventory_platform_tests;
#[cfg(test)]
mod inventory_tests;
#[cfg(test)]
mod inventory_window_tests;
#[cfg(test)]
mod live_converged_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
pub(crate) mod login_support;
#[cfg(test)]
mod marks_support;
#[cfg(test)]
mod marks_tests;
#[cfg(test)]
pub(crate) mod mdm_pki_support;
#[cfg(test)]
mod membership_tests;
#[cfg(test)]
mod moving_facts_tests;
#[cfg(test)]
mod moving_tests;
#[cfg(test)]
mod one_live_update_coordinator_tests;
#[cfg(test)]
mod one_live_update_release_tests;
#[cfg(test)]
mod one_live_update_tests;
#[cfg(test)]
mod operator_mdm_devices_tests;
#[cfg(test)]
mod operator_mdm_name_tests;
#[cfg(test)]
mod operator_repositories_tests;
#[cfg(test)]
mod ops_commit_tests;
#[cfg(test)]
mod ops_crash_tests;
#[cfg(test)]
mod ops_downgrade_tests;
#[cfg(test)]
mod ops_platform_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod order_terms_tests;
#[cfg(test)]
mod origin_https_tests;
#[cfg(test)]
mod password_tests;
#[cfg(test)]
mod picks_private_tests;
#[cfg(test)]
mod picks_tests;
#[cfg(test)]
mod plan_rollout_tests;
#[cfg(test)]
mod plan_status_tests;
#[cfg(test)]
mod priority_tests;
#[cfg(test)]
mod recorded_definition_tests;
#[cfg(test)]
mod recorded_review_tests;
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
mod release_set_repository_tests;
#[cfg(test)]
mod release_set_tests;
#[cfg(test)]
mod releases_tests;
#[cfg(test)]
mod repositories_support;
#[cfg(test)]
mod repositories_tests;
#[cfg(test)]
mod repository_listen_tests;
#[cfg(test)]
mod repository_origin_support;
#[cfg(test)]
mod repository_origin_tests;
#[cfg(test)]
mod repository_private_tests;
#[cfg(test)]
mod repository_push_group_tests;
#[cfg(test)]
mod repository_push_private_tests;
#[cfg(test)]
mod repository_push_tests;
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
mod rollouts_calls_support;
#[cfg(test)]
mod rollouts_calls_tests;
#[cfg(test)]
mod rollouts_refusal_tests;
#[cfg(test)]
mod rollouts_restart_tests;
#[cfg(test)]
mod rollouts_state_tests;
#[cfg(test)]
mod running_tests;
#[cfg(test)]
mod self_update_fallback_tests;
#[cfg(test)]
mod service_repository_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod services_repository_tests;
#[cfg(test)]
mod services_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod stages_edge_tests;
#[cfg(test)]
mod stages_tests;
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
mod store_orders_tests;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tierstore_service_tests;
#[cfg(test)]
mod tokens_tests;
#[cfg(test)]
mod try_per_service_tests;
#[cfg(test)]
mod try_refusal_tests;
#[cfg(test)]
mod uninstall_tests;
#[cfg(test)]
mod unmark_tests;
#[cfg(test)]
mod verdict_acks_tests;
#[cfg(test)]
mod verdict_file_tests;
#[cfg(test)]
mod version_route_tests;
