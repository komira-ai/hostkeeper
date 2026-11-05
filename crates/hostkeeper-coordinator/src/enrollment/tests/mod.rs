//! The tests of enrollment at the coordinator: the calls and their refusals, the nonces, the
//! heartbeat path's questions (identity, deny rows, duplicates, the retiring host's slot), the
//! start, the operator's `Enrollment.List` and `Enrollment.Approve` with their audit, the
//! pending cap, and the Mac path. In-process only: a call is a handler or [`crate::enrollment::call`] with an
//! explicit peer certificate, and no socket is opened.

mod approve;
mod approve_refusals;
mod heartbeat;
mod inventory;
mod list;
mod mac;
mod mac_restart;
mod nonces;
mod operator;
mod refusals;
mod renew;
mod rpc;
mod setup;
mod support;
mod wire;
