//! The tests of enrollment at the coordinator: the calls and their refusals, the nonces, the
//! heartbeat path's questions (identity, deny rows, duplicates, the retiring host's slot), and
//! the start. In-process only: a call is a handler or [`crate::enrollment::call`] with an
//! explicit peer certificate, and no socket is opened.

mod heartbeat;
mod inventory;
mod nonces;
mod refusals;
mod renew;
mod rpc;
mod setup;
mod support;
mod wire;
