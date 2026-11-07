//! The tests of enrollment at the coordinator: the calls and their refusals, the nonces, the
//! heartbeat path's questions (identity, deny rows, duplicates, the retiring host's slot), the
//! start, the server certificate renewed while the coordinator runs, the operator's
//! `Enrollment.List`, `Enrollment.Approve` and `Enrollment.Reject` with their audit,
//! `Hosts.Retire`, the pending cap, the expiry of a request no operator approved in time, and the
//! Mac path. In-process only: a call is a handler or [`crate::enrollment::call`] with an explicit
//! peer certificate, and no socket is opened.

mod approve;
mod approve_refusals;
mod audit;
mod contested;
mod expiry;
mod heartbeat;
mod inventory;
mod inventory_approve;
mod list;
mod mac;
mod mac_restart;
mod nonces;
mod operator;
mod refusals;
mod reject;
mod renew;
mod retire;
mod rpc;
mod server_renewal;
mod setup;
pub(crate) mod support;
mod wire;
